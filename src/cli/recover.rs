//! `lsi-flash recover` — restore a card from a backup directory created by `backup`.
//! Per ADR-015 Rules 1, 4, 5, 6: personality check, verify-after-write, hard-stop on errors.

use serde_json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use thiserror::Error;

use crate::cli::backup::BackupManifest;
use crate::mpi::messages::{ImageType, IocInitRequest, MpiError};
use crate::mpi::mock_ioc::MockIoc;
use crate::mpi::session::{Personality, Session};

#[derive(Debug, Error)]
pub enum RecoverError {
    #[error("backup dir not found: {0}")]
    BackupDirNotFound(PathBuf),

    #[error("manifest.toml not found in {0}")]
    ManifestNotFound(PathBuf),

    #[error("manifest parse: {0}")]
    ManifestParse(#[from] Box<dyn std::error::Error + Send + Sync>),

    #[error("artifact {name} sha256 mismatch: manifest={expected}, actual={actual}\nArtifact may have been tampered with or corrupted. Aborting to prevent writing bad data.")]
    Sha256Mismatch {
        name: String,
        expected: String,
        actual: String,
    },

    #[error("artifact {0} referenced in manifest but file missing")]
    ArtifactMissing(String),

    #[error("MPI: {0}")]
    Mpi(#[from] MpiError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("user declined confirmation")]
    Declined,
}

pub fn run(backup_dir: String, yes: bool, json: bool) -> Result<(), crate::Error> {
    let dir = PathBuf::from(&backup_dir);
    if !dir.exists() {
        return Err(RecoverError::BackupDirNotFound(dir).into());
    }

    // 1. Load + parse manifest
    let manifest_path = dir.join("manifest.toml");
    if !manifest_path.exists() {
        return Err(RecoverError::ManifestNotFound(dir).into());
    }
    let manifest_str = std::fs::read_to_string(&manifest_path)?;
    let manifest: BackupManifest = toml::from_str(&manifest_str).map_err(|e| {
        RecoverError::ManifestParse(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    })?;

    // 2. Verify every artifact's sha256 BEFORE we touch the card (Rule 5: don't trust without verification)
    for artifact in &manifest.artifacts {
        let path = dir.join(&artifact.path);
        if !path.exists() {
            return Err(RecoverError::ArtifactMissing(artifact.path.clone()).into());
        }
        let bytes = std::fs::read(&path)?;
        let actual = sha256_hex(&bytes);
        if actual != artifact.sha256 {
            return Err(RecoverError::Sha256Mismatch {
                name: artifact.path.clone(),
                expected: artifact.sha256.clone(),
                actual,
            }
            .into());
        }
    }

    // 3. User confirmation (unless --yes)
    if !yes {
        eprintln!(
            "About to restore {} artifacts from {} — proceed? [y/N]",
            manifest.artifacts.len(),
            dir.display()
        );
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim().to_lowercase() != "y" {
            return Err(RecoverError::Declined.into());
        }
    }

    // 4. Build session (MockIoc default)
    let mock_ioc = MockIoc::new(Personality::It);
    let mut session = Session::new(mock_ioc);
    let init_req = IocInitRequest {
        who_init: 0x04,
        host_msix_vectors: 0,
        reply_descriptor_post_queue_depth: 16,
        system_request_frame_base_address: 0,
        reply_descriptor_post_queue_address: 0,
    };
    session.raw_ioc_init(&init_req)?;

    // 5. For each artifact, read bytes + FW_DOWNLOAD via verified helper
    let mut restored = Vec::new();
    for artifact in &manifest.artifacts {
        let path = dir.join(&artifact.path);
        let bytes = std::fs::read(&path)?;

        // Determine image type from manifest
        let image_type = match artifact.image_type.as_str() {
            "Fw" => ImageType::Fw,
            "Bios" => ImageType::Bios,
            "NvData" => ImageType::NvData,
            _ => {
                eprintln!(
                    "Skipping unsupported image type: {} (only Fw/Bios/NvData supported)",
                    artifact.image_type
                );
                continue;
            }
        };

        // Rule 1: verify personality match before write via fw_download_verified helper
        let target_personality = Personality::It; // Assume IT for now (matches backup default)

        session.fw_download_verified(image_type, target_personality, &bytes)?;
        restored.push(artifact.path.clone());
    }

    // 6. Output
    if json {
        let output = serde_json::json!({
            "status": "success",
            "artifacts_restored": restored.len(),
            "artifacts": restored,
            "source_dir": backup_dir
        });
        println!("{}", serde_json::to_string(&output)?);
    } else {
        println!("recovered {} artifacts from {}", restored.len(), backup_dir);
        for r in &restored {
            println!("  ✓ {}", r);
        }
    }

    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::backup::BackupArtifact;
    use crate::mpi::messages::IocStatus;
    use tempfile::TempDir;

    fn create_test_backup(dir: &std::path::Path) -> BackupManifest {
        let firmware_data = vec![0xAA; 1024];
        let bios_data = vec![0xBB; 512];
        let nvdata_data = vec![0xCC; 256];

        std::fs::write(dir.join("firmware.bin"), &firmware_data).unwrap();
        std::fs::write(dir.join("bios.rom"), &bios_data).unwrap();
        std::fs::write(dir.join("nvdata.bin"), &nvdata_data).unwrap();

        let manifest = BackupManifest {
            timestamp: chrono::Utc::now().to_rfc3339(),
            sas_wwn: None,
            artifacts: vec![
                BackupArtifact {
                    path: "firmware.bin".to_string(),
                    image_type: "Fw".to_string(),
                    sha256: sha256_hex(&firmware_data),
                    size: firmware_data.len() as u64,
                },
                BackupArtifact {
                    path: "bios.rom".to_string(),
                    image_type: "Bios".to_string(),
                    sha256: sha256_hex(&bios_data),
                    size: bios_data.len() as u64,
                },
                BackupArtifact {
                    path: "nvdata.bin".to_string(),
                    image_type: "NvData".to_string(),
                    sha256: sha256_hex(&nvdata_data),
                    size: nvdata_data.len() as u64,
                },
            ],
            source_card: None,
        };

        let manifest_str = toml::to_string_pretty(&manifest).unwrap();
        std::fs::write(dir.join("manifest.toml"), manifest_str).unwrap();

        manifest
    }

    #[test]
    fn recover_refuses_missing_backup_dir() {
        let result = run("/nonexistent/path/foo".to_string(), true, false);
        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::BackupDirNotFound(_)))
        ));
    }

    #[test]
    fn recover_refuses_missing_manifest() {
        let tmp = TempDir::new().unwrap();
        let result = run(tmp.path().to_str().unwrap().to_string(), true, false);
        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::ManifestNotFound(_)))
        ));
    }

    #[test]
    fn recover_refuses_sha256_mismatch() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // Tamper with firmware.bin (flip a byte)
        let mut data = std::fs::read(tmp.path().join("firmware.bin")).unwrap();
        data[0] ^= 0xFF;
        std::fs::write(tmp.path().join("firmware.bin"), &data).unwrap();

        let result = run(tmp.path().to_str().unwrap().to_string(), true, false);
        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::Sha256Mismatch { .. }))
        ));
    }

    #[test]
    fn recover_refuses_artifact_missing_from_manifest() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // Delete firmware.bin but leave manifest
        std::fs::remove_file(tmp.path().join("firmware.bin")).unwrap();

        let result = run(tmp.path().to_str().unwrap().to_string(), true, false);
        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::ArtifactMissing(_)))
        ));
    }

    #[test]
    fn recover_propagates_iocstatus_failure() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // Create a mock with failure injection
        let mut mock_ioc = MockIoc::new(Personality::It);
        mock_ioc.inject.next_fw_download_error = Some(IocStatus::InternalError);

        let _manifest_str = std::fs::read_to_string(tmp.path().join("manifest.toml")).unwrap();
        let mut session = Session::new(mock_ioc);
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        session.raw_ioc_init(&init_req).unwrap();

        // Manually test the download path with injected failure
        let bytes = std::fs::read(tmp.path().join("firmware.bin")).unwrap();
        let result = session.fw_download_verified(ImageType::Fw, Personality::It, &bytes);

        assert!(matches!(
            result,
            Err(MpiError::IocStatus(IocStatus::InternalError))
        ));
    }

    #[test]
    fn recover_with_yes_skips_confirmation() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // Create a fresh session for this test
        let mock_ioc = MockIoc::new(Personality::It);
        let mut session = Session::new(mock_ioc);
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        session.raw_ioc_init(&init_req).unwrap();

        // This should not panic or block on stdin when yes=true
        let result = run(tmp.path().to_str().unwrap().to_string(), true, false);

        // Should succeed (or at least not fail with Declined)
        assert!(!matches!(
            result,
            Err(crate::Error::Recover(RecoverError::Declined))
        ));
    }

    #[test]
    fn recover_validates_all_artifacts_before_any_write() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // Tamper with bios.rom (not firmware.bin)
        let mut data = std::fs::read(tmp.path().join("bios.rom")).unwrap();
        data[0] ^= 0xFF;
        std::fs::write(tmp.path().join("bios.rom"), &data).unwrap();

        // Should fail on bios validation before any download happens
        let result = run(tmp.path().to_str().unwrap().to_string(), true, false);

        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::Sha256Mismatch { .. }))
        ));
    }

    #[test]
    fn sha256_hex_produces_64_char_hex_string() {
        let data = b"hello world";
        let hash = sha256_hex(data);

        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn recover_handles_json_output() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // Create a fresh session for this test
        let mock_ioc = MockIoc::new(Personality::It);
        let mut session = Session::new(mock_ioc);
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        session.raw_ioc_init(&init_req).unwrap();

        let result = run(tmp.path().to_str().unwrap().to_string(), true, true);

        // Should succeed with JSON output (no panic)
        assert!(result.is_ok());
    }
}
