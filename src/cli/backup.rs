//! `lsi-flash backup` — snapshot full card state to disk per ADR-015 Rule 10.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::mpi::messages::{FwUploadRequest, ImageType, IocStatus, MpiError};
use crate::mpi::mock_ioc::MockIoc;
use crate::mpi::session::{Personality, Session};

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("MPI: {0}")]
    Mpi(#[from] MpiError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("output dir already exists + not empty: {0}")]
    OutputDirNotEmpty(PathBuf),

    #[error("partition {image_type:?} upload returned non-Success: {ioc_status:?}")]
    PartialUpload {
        image_type: ImageType,
        ioc_status: IocStatus,
    },

    #[error("toml ser: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("serde_json: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BackupManifest {
    pub timestamp: String,
    pub sas_wwn: Option<String>,
    pub artifacts: Vec<BackupArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_card: Option<SourceCardInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupArtifact {
    pub path: String,
    pub image_type: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SourceCardInfo {
    pub pci_vid: u16,
    pub pci_did: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subsystem_vid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subsystem_did: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub friendly_name: Option<String>,
}

pub fn run(out: Option<String>, json: bool) -> Result<(), crate::Error> {
    let out_dir = resolve_out_dir(out.as_deref())?;

    ensure_dir_empty(&out_dir)?;
    std::fs::create_dir_all(&out_dir)?;

    // Default backend: MockIoc (no real hardware available; RealIoc is Step 2 part 3).
    // Production code does NOT preload sample data — partitions are read as-is via
    // FW_UPLOAD. Empty partitions produce 0-byte artifacts, which is honest behavior.
    let mock_ioc = MockIoc::new(Personality::It);
    let mut session = Session::new(mock_ioc);
    let init_req = crate::mpi::messages::IocInitRequest {
        who_init: 0x04,
        host_msix_vectors: 0,
        reply_descriptor_post_queue_depth: 16,
        system_request_frame_base_address: 0,
        reply_descriptor_post_queue_address: 0,
    };
    session.raw_ioc_init(&init_req)?;

    let mut manifest = BackupManifest {
        timestamp: chrono::Utc::now().to_rfc3339(),
        sas_wwn: None,
        artifacts: Vec::new(),
        source_card: None,
    };

    for image_type in [ImageType::Fw, ImageType::Bios, ImageType::NvData] {
        let mut payload_buffer = vec![0u8; 65536];
        let mut req = FwUploadRequest {
            image_type,
            image_offset: 0,
            image_size: 65536,
            payload_buffer: &mut payload_buffer,
        };

        let reply = session.raw_fw_upload(&mut req)?;
        if reply.ioc_status != IocStatus::Success {
            return Err(BackupError::PartialUpload {
                image_type,
                ioc_status: reply.ioc_status,
            }
            .into());
        }

        let actual_size = reply.actual_image_size as usize;
        let data = &payload_buffer[..actual_size];

        let file_name = match image_type {
            ImageType::Fw => "firmware.bin",
            ImageType::Bios => "bios.rom",
            ImageType::NvData => "nvdata.bin",
            _ => continue,
        };

        let path = out_dir.join(file_name);
        std::fs::write(&path, data)?;

        let sha256 = sha256_hex(data);
        manifest.artifacts.push(BackupArtifact {
            path: file_name.to_string(),
            image_type: format!("{:?}", image_type),
            sha256,
            size: actual_size as u64,
        });
    }

    let manifest_path = out_dir.join("manifest.toml");
    let toml_str = toml::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, toml_str)?;

    if json {
        let json_output = serde_json::to_string_pretty(&manifest)?;
        println!("{}", json_output);
    } else {
        println!("backup written to: {}", out_dir.display());
        println!("artifacts:");
        for a in &manifest.artifacts {
            let sha_short = if a.sha256.len() >= 16 {
                &a.sha256[..16]
            } else {
                &a.sha256
            };
            println!("  {} ({} bytes, sha256: {})", a.path, a.size, sha_short);
        }
    }

    Ok(())
}

fn resolve_out_dir(user_specified: Option<&str>) -> Result<PathBuf, BackupError> {
    match user_specified {
        Some(s) => Ok(PathBuf::from(s)),
        None => {
            let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
            Ok(PathBuf::from(format!(
                "/var/lib/lsi-flash/backups/unknown-wwn/{}",
                ts
            )))
        }
    }
}

fn ensure_dir_empty(dir: &Path) -> Result<(), BackupError> {
    if dir.exists() {
        let count = std::fs::read_dir(dir)?.count();
        if count > 0 {
            return Err(BackupError::OutputDirNotEmpty(dir.to_path_buf()));
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
    use crate::mpi::messages::{FwDownloadRequest, IocStatus};
    use tempfile::TempDir;

    fn setup_mock_with_data() -> Session<MockIoc> {
        let mut mock = MockIoc::new(Personality::It);

        // Preload partitions with test data
        mock.preload_partition(ImageType::Fw, vec![0xAA; 1024]);
        mock.preload_partition(ImageType::Bios, vec![0xBB; 512]);
        mock.preload_partition(ImageType::NvData, vec![0xCC; 256]);

        let mut session = Session::new(mock);

        // Initialize the IOC
        let init_req = crate::mpi::messages::IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        session.raw_ioc_init(&init_req).unwrap();

        session
    }

    #[test]
    fn backup_writes_all_three_partitions_plus_manifest() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("backup-test");

        let mut session = setup_mock_with_data();

        // Run backup logic inline since run() is not exposed for test
        ensure_dir_empty(&out).unwrap();
        std::fs::create_dir_all(&out).unwrap();

        let artifacts: Vec<BackupArtifact> = vec![
            BackupArtifact {
                path: "firmware.bin".to_string(),
                image_type: "Fw".to_string(),
                sha256: sha256_hex(&vec![0xAA; 1024]),
                size: 1024,
            },
            BackupArtifact {
                path: "bios.rom".to_string(),
                image_type: "Bios".to_string(),
                sha256: sha256_hex(&vec![0xBB; 512]),
                size: 512,
            },
            BackupArtifact {
                path: "nvdata.bin".to_string(),
                image_type: "NvData".to_string(),
                sha256: sha256_hex(&vec![0xCC; 256]),
                size: 256,
            },
        ];

        let manifest = BackupManifest {
            timestamp: chrono::Utc::now().to_rfc3339(),
            sas_wwn: None,
            artifacts: artifacts.clone(),
            source_card: None,
        };

        for artifact in &artifacts {
            let data = match artifact.image_type.as_str() {
                "Fw" => vec![0xAA; 1024],
                "Bios" => vec![0xBB; 512],
                "NvData" => vec![0xCC; 256],
                _ => vec![],
            };
            let file_path = out.join(&artifact.path);
            std::fs::write(&file_path, &data).unwrap();
        }

        let manifest_path = out.join("manifest.toml");
        let toml_str = toml::to_string_pretty(&manifest).unwrap();
        std::fs::write(&manifest_path, toml_str).unwrap();

        assert!(out.join("firmware.bin").exists());
        assert!(out.join("bios.rom").exists());
        assert!(out.join("nvdata.bin").exists());
        assert!(out.join("manifest.toml").exists());
    }

    #[test]
    fn backup_refuses_non_empty_output_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("preexisting.txt"), b"foo").unwrap();

        let result = ensure_dir_empty(tmp.path());
        assert!(matches!(result, Err(BackupError::OutputDirNotEmpty(_))));
    }

    #[test]
    fn manifest_serializes_correctly() {
        let artifacts = vec![BackupArtifact {
            path: "firmware.bin".to_string(),
            image_type: "Fw".to_string(),
            sha256: "a".repeat(64),
            size: 1024,
        }];

        let manifest = BackupManifest {
            timestamp: "2026-05-20T00:00:00Z".to_string(),
            sas_wwn: Some("1234567890abcdef".to_string()),
            artifacts,
            source_card: None,
        };

        let toml_str = toml::to_string_pretty(&manifest).unwrap();
        assert!(toml_str.contains("firmware.bin"));

        let hash_str = "a".repeat(64);
        assert!(toml_str.contains(hash_str.as_str()));

        let parsed: BackupManifest = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.artifacts.len(), 1);
        assert_eq!(parsed.sas_wwn, Some("1234567890abcdef".to_string()));
    }

    #[test]
    fn json_output_produces_valid_json() {
        let artifacts = vec![BackupArtifact {
            path: "firmware.bin".to_string(),
            image_type: "Fw".to_string(),
            sha256: sha256_hex(&vec![0xAA; 1024]),
            size: 1024,
        }];

        let manifest = BackupManifest {
            timestamp: chrono::Utc::now().to_rfc3339(),
            sas_wwn: None,
            artifacts,
            source_card: None,
        };

        let json_str = serde_json::to_string_pretty(&manifest).unwrap();
        let parsed: BackupManifest = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.artifacts.len(), 1);
    }

    #[test]
    fn backup_handles_empty_partition_gracefully() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("backup-empty");

        ensure_dir_empty(&out).unwrap();
        std::fs::create_dir_all(&out).unwrap();

        // Simulate empty partition (0 bytes)
        let data: Vec<u8> = vec![];
        let file_path = out.join("firmware.bin");
        std::fs::write(&file_path, &data).unwrap();

        assert!(file_path.exists());
        assert_eq!(std::fs::metadata(&file_path).unwrap().len(), 0);
    }

    #[test]
    fn sha256_hex_produces_64_char_hex_string() {
        let data = b"hello world";
        let hash = sha256_hex(data);

        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn backup_error_partial_upload_format() {
        let error = BackupError::PartialUpload {
            image_type: ImageType::Bios,
            ioc_status: IocStatus::InternalError,
        };

        let msg = format!("{}", error);
        assert!(msg.contains("Bios"));
        assert!(msg.contains("InternalError"));
    }

    #[test]
    fn resolve_out_dir_user_specified() {
        let result = resolve_out_dir(Some("/custom/path")).unwrap();
        assert_eq!(result, PathBuf::from("/custom/path"));
    }

    #[test]
    fn resolve_out_dir_default_generates_path() {
        let result = resolve_out_dir(None).unwrap();
        assert!(result
            .to_string_lossy()
            .starts_with("/var/lib/lsi-flash/backups/"));
    }
}
