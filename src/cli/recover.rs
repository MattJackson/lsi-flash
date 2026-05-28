//! `lsi-flash recover` — restore a card from a backup directory created by `backup`.
//! Per ADR-015 Rules 1, 4, 5, 6: personality check, verify-after-write, hard-stop on errors.

use serde_json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use thiserror::Error;

use crate::cli::backup::BackupManifest;
use crate::mpi::messages::MpiError;

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

pub fn run(
    backup_dir: String,
    yes: bool,
    json: bool,
    pci: Option<String>,
    dry_run: bool,
) -> Result<(), crate::Error> {
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
    if !yes && !dry_run {
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

    // 4. Resolve bdf (mirror backup::run)
    let bdf = match pci {
        Some(b) => b,
        None => {
            // In dry-run mode without PCI specified, use a placeholder since we won't touch hardware
            if dry_run {
                "0000:00:00.0".to_string()
            } else {
                let devices = crate::pci::discover_sas2008_devices_linux().map_err(|e| {
                    crate::Error::Other(format!("recover: failed to enumerate PCI devices: {}", e))
                })?;
                match devices.first() {
                    Some(first) => first.bdf.clone(),
                    None => {
                        return Err(crate::Error::Other(
                            "recover: no SAS2008 card found — specify --pci".to_string(),
                        ))
                    }
                }
            }
        }
    };

    // 5. Dry-run path: print plan, touch no hardware
    if dry_run {
        let firmware_artifacts: Vec<_> = manifest
            .artifacts
            .iter()
            .filter(|a| a.path == "firmware.bin" || a.path == "bios.rom")
            .collect();

        if json {
            let output = serde_json::json!({
                "status": "dry-run",
                "regions_planned": firmware_artifacts.len(),
                "regions": firmware_artifacts.iter().map(|a| serde_json::json!({
                    "path": a.path,
                    "size": a.size,
                    "sha256": a.sha256
                })).collect::<Vec<_>>(),
                "source_dir": backup_dir,
                "pci_bdf": bdf
            });
            println!("{}", serde_json::to_string(&output)?);
        } else {
            eprintln!(
                "[dry-run] would restore {} regions from {} to {}:",
                firmware_artifacts.len(),
                backup_dir,
                bdf
            );
            for artifact in &firmware_artifacts {
                eprintln!(
                    "  {} ({} bytes, sha256={})",
                    artifact.path,
                    artifact.size,
                    &artifact.sha256[..16]
                );
            }
            eprintln!("(nvdata.bin skipped: IT asymmetry per R0 doc)");
        }
        return Ok(());
    }

    // 6. Real path: dispatch through Card trait (same as backup)
    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("recover: discover_one({}): {}", bdf, e)))?;

    let report = card
        .restore(std::path::Path::new(&backup_dir))
        .map_err(|e| crate::Error::Other(format!("recover: card.restore: {}", e)))?;

    // 7. Output (honest about which regions were actually written)
    if json {
        let output = serde_json::json!({
            "status": "success",
            "regions_restored": report.regions_written,
            "regions": report.regions,
            "source_dir": backup_dir
        });
        println!("{}", serde_json::to_string(&output)?);
    } else {
        println!(
            "restored {} regions from {}",
            report.regions_written, backup_dir
        );
        for region in &report.regions {
            println!("  ✓ {}", region);
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
        let result = run(
            "/nonexistent/path/foo".to_string(),
            true,
            false,
            None,
            false,
        );
        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::BackupDirNotFound(_)))
        ));
    }

    #[test]
    fn recover_refuses_missing_manifest() {
        let tmp = TempDir::new().unwrap();
        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            false,
            None,
            false,
        );
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

        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            false,
            None,
            false,
        );
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

        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            false,
            None,
            false,
        );
        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::ArtifactMissing(_)))
        ));
    }

    #[test]
    fn recover_with_yes_skips_confirmation() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // This should not panic or block on stdin when yes=true
        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            false,
            None,
            false,
        );

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
        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            false,
            None,
            false,
        );

        assert!(matches!(
            result,
            Err(crate::Error::Recover(RecoverError::Sha256Mismatch { .. }))
        ));
    }

    #[test]
    fn recover_dry_run_touches_no_hardware() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        // Dry-run with valid backup should succeed without calling discover_one
        // (which would fail without hardware)
        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            false,
            None,
            true,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn recover_dry_run_json_output() {
        let tmp = TempDir::new().unwrap();
        create_test_backup(tmp.path());

        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            true,
            None,
            true,
        );

        assert!(result.is_ok());
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

        // Use dry_run since we don't have hardware in test environment
        let result = run(
            tmp.path().to_str().unwrap().to_string(),
            true,
            true,
            None,
            true,
        );

        // Should succeed with JSON output (no panic)
        assert!(result.is_ok());
    }
}
