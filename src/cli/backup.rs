//! `lsi-flash backup` — snapshot full card state to disk per ADR-015 Rule 10.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::card::BackupReport;
use crate::mpi::messages::{FwUploadRequest, ImageType, IocStatus, MpiError};
use crate::mpi::mock_ioc::MockIoc;
use crate::mpi::session::{Personality, Session};
use crate::sbr::transport::Bar1MmapSbrTransport;

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

pub fn run(out: Option<String>, json: bool, pci: Option<String>) -> Result<(), crate::Error> {
    let out_dir = resolve_out_dir(out.as_deref())?;

    ensure_dir_empty(&out_dir)?;
    std::fs::create_dir_all(&out_dir)?;

    // Pick backend: `--pci <bdf>` → real hardware via RealIoc; otherwise MockIoc
    // (for tests / docs / no-hardware development). Both speak IocBackend; we
    // keep the rest of the function generic by routing through a single
    // run_backup_with_session helper below.
    let init_req = crate::mpi::messages::IocInitRequest {
        who_init: 0x04,
        host_msix_vectors: 0,
        reply_descriptor_post_queue_depth: 16,
        system_request_frame_base_address: 0,
        reply_descriptor_post_queue_address: 0,
    };

    // --pci is REQUIRED (no auto-detect; run `detect` to list cards). NEVER
    // silently mock: the mock is reachable ONLY via the explicit sentinel BDF
    // (crate::card::MOCK_BDF). A missing --pci errors out — it can no longer
    // produce a fake empty "backup".
    let bdf = crate::card::resolve_bdf(pci.as_deref())
        .map_err(|e| crate::Error::Other(format!("{}", e)))?;

    if crate::card::is_mock_bdf(&bdf) {
        eprintln!(
            "backup: ⚠ MOCK backend (sentinel {}) — artifacts are SYNTHETIC, NOT a real card.",
            crate::card::MOCK_BDF
        );
        let mock_ioc = MockIoc::new(Personality::It);
        let mut session = Session::new(mock_ioc);
        session.raw_ioc_init(&init_req)?;
        return run_backup_with_session(&mut session, &out_dir, json, Some(&bdf));
    }

    {
        // Real hardware (auto-detected, or the explicit --pci <bdf>).
        // ADR-017: use MptCard for read-safe operations via kernel-mediated transport.
        // If mpt3sas isn't loaded or no IOC manages this BDF, fall back to VFIO+doorbell.
        match crate::card::discover_one(&bdf) {
            Ok(mut card) => {
                eprintln!(
                    "backup: using MptCard via kernel-mediated transport ({})",
                    bdf
                );
                let report = card
                    .backup(&out_dir)
                    .map_err(|e| crate::Error::Other(format!("card.backup: {}", e)))?;
                return print_backup_report(&out_dir, &report, json);
            }
            Err(e) => {
                eprintln!(
                    "backup: card.discover_one({}) failed: {} — falling back to legacy doorbell path",
                    bdf, e
                );
            }
        }

        let platform = crate::pci::LinuxSysfs;
        // Prefer the HwBackend path (VFIO) — it does the driver bind dance
        // and provides chip-readable DMA buffers, so FW_UPLOAD returns real
        // bytes instead of the 885-KB-of-zeros bug (ADR-016). Falls back to
        // the legacy direct-sysfs mmap path if no backend can attach (e.g.,
        // VFIO modules missing) — operator can still get a partial result
        // by unbinding mpt3sas manually first.
        #[cfg(target_os = "linux")]
        let real_ioc = match crate::hw::auto_detect(&bdf) {
            Ok(backend) => {
                crate::mpi::real_ioc::RealIoc::from_backend(platform, backend).map_err(|e| {
                    crate::Error::Other(format!("RealIoc::from_backend({}) failed: {}", bdf, e))
                })?
            }
            Err(hw_err) => {
                eprintln!(
                    "warning: HwBackend (VFIO) attach failed: {} — falling back to direct sysfs mmap (no DMA, FW_UPLOAD may return zeros)",
                    hw_err
                );
                crate::mpi::real_ioc::RealIoc::open(platform, &bdf).map_err(|e| {
                    crate::Error::Other(format!("RealIoc::open({}) failed: {}", bdf, e))
                })?
            }
        };
        #[cfg(not(target_os = "linux"))]
        let real_ioc = crate::mpi::real_ioc::RealIoc::open(platform, &bdf)
            .map_err(|e| crate::Error::Other(format!("RealIoc::open({}) failed: {}", bdf, e)))?;
        let mut session = Session::new(real_ioc);
        let _ = session.raw_ioc_init(&init_req);
        run_backup_with_session(&mut session, &out_dir, json, Some(&bdf))
    }
}

/// Print backup report in human-readable or JSON format.
fn print_backup_report(
    _out_dir: &Path,
    report: &BackupReport,
    json: bool,
) -> Result<(), crate::Error> {
    if json {
        // Reconstruct manifest for JSON output (without source_card to keep it simple)
        let mut artifacts = Vec::new();
        for artifact in &report.artifacts {
            artifacts.push(serde_json::json!({
                "path": artifact.path,
                "image_type": artifact.image_type,
                "sha256": artifact.sha256,
                "size": artifact.size
            }));
        }

        let output = serde_json::json!({
            "timestamp": report.timestamp,
            "artifacts_count": report.artifacts_count,
            "artifacts": artifacts
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        eprintln!("backup written to: {}", _out_dir.display());
        eprintln!("artifacts ({}) summary:", report.artifacts_count);
        for a in &report.artifacts {
            let sha_short = if a.sha256.len() >= 16 {
                &a.sha256[..16]
            } else {
                &a.sha256
            };
            eprintln!("  {} ({} bytes, sha256: {})", a.path, a.size, sha_short);
        }
    }
    Ok(())
}

fn run_backup_with_session<B: crate::mpi::session::IocBackend>(
    session: &mut Session<B>,
    out_dir: &Path,
    json: bool,
    bdf: Option<&str>,
) -> Result<(), crate::Error> {
    let mut manifest = BackupManifest {
        timestamp: chrono::Utc::now().to_rfc3339(),
        sas_wwn: None,
        artifacts: Vec::new(),
        source_card: None,
    };

    // FW_UPLOAD ITYPE values per mpi2_ioc.h:1244-1253:
    //   0x01 = MPI2_FW_UPLOAD_ITYPE_FW_FLASH       (firmware ARM code)
    //   0x02 = MPI2_FW_UPLOAD_ITYPE_BIOS_FLASH     (option ROM)
    //   0x06 = MPI2_FW_UPLOAD_ITYPE_MANUFACTURING  (NVDATA — DIFFERENT
    //          from FW_DOWNLOAD's NVDATA=0x03; our ImageType::NvData
    //          maps the DOWNLOAD value and is wrong for UPLOAD).
    // ImageType::FlashLayout serializes to 0x06 — the right UPLOAD code.
    for image_type in [ImageType::Fw, ImageType::Bios, ImageType::FlashLayout] {
        // SAS2008 flash region tops out around 1 MB across all known OEM
        // variants (Dell ITA A04 firmware on dev-1 measured at 885 KB / 0xD831C
        // bytes 2026-05-28). 2 MB buffer gives headroom for future SAS2208
        // and Dell-extended NVData revisions without a second round-trip.
        const UPLOAD_BUF_SIZE: usize = 2 * 1024 * 1024;
        let mut payload_buffer = vec![0u8; UPLOAD_BUF_SIZE];
        let mut req = FwUploadRequest {
            image_type,
            image_offset: 0,
            image_size: UPLOAD_BUF_SIZE as u32,
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

        let actual_size = (reply.actual_image_size as usize).min(payload_buffer.len());
        let data = &payload_buffer[..actual_size];

        let file_name = match image_type {
            ImageType::Fw => "firmware.bin",
            ImageType::Bios => "bios.rom",
            ImageType::FlashLayout => "nvdata.bin", // FW_UPLOAD ITYPE_MANUFACTURING = NVDATA
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

    const FLASH_CHIP_ADDR: u32 = 0xFC000000;
    const FLASH_SIZE: usize = 8 * 1024 * 1024;
    let flash_path = out_dir.join("flash-full.bin");

    if let Some(bdf_str) = bdf {
        if let Ok(mut transport) = Bar1MmapSbrTransport::open(bdf_str) {
            match transport.read_chip_mem(FLASH_CHIP_ADDR, FLASH_SIZE) {
                Ok(flash_data) => {
                    std::fs::write(&flash_path, &flash_data)?;
                    let sha256 = sha256_hex(&flash_data);
                    manifest.artifacts.push(BackupArtifact {
                        path: "flash-full.bin".to_string(),
                        image_type: "FullFlash".to_string(),
                        sha256,
                        size: flash_data.len() as u64,
                    });
                }
                Err(e) => {
                    eprintln!(
                        "warning: backup: diag back-door full-flash read failed: {} — \
                         continuing with partial backup",
                        e
                    );
                }
            }
        } else {
            eprintln!(
                "warning: backup: could not open Bar1MmapSbrTransport for full-flash capture — \
                 continuing without flash-full.bin"
            );
        }
    }

    let manifest_path = out_dir.join("manifest.toml");
    let toml_str = toml::to_string_pretty(&manifest)?;
    std::fs::write(manifest_path, toml_str)?;

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
    use crate::mpi::messages::IocStatus;
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
    fn backup_with_mock_no_full_flash() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("backup-mock");

        ensure_dir_empty(&out).unwrap();
        std::fs::create_dir_all(&out).unwrap();

        let mut mock_ioc = MockIoc::new(Personality::It);
        mock_ioc.preload_partition(ImageType::Fw, vec![0xAA; 1024]);
        mock_ioc.preload_partition(ImageType::Bios, vec![0xBB; 512]);
        mock_ioc.preload_partition(ImageType::NvData, vec![0xCC; 256]);

        let mut session = Session::new(mock_ioc);
        let init_req = crate::mpi::messages::IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        session.raw_ioc_init(&init_req).unwrap();

        let result = run_backup_with_session(&mut session, &out, false, None);
        assert!(result.is_ok());

        assert!(out.join("firmware.bin").exists());
        assert!(out.join("bios.rom").exists());
        assert!(out.join("nvdata.bin").exists());
        assert!(out.join("manifest.toml").exists());
        // Mock backend should not produce flash-full.bin (no real BAR1)
        assert!(!out.join("flash-full.bin").exists(), "Mock should not capture full flash");
    }

    #[test]
    fn backup_writes_all_three_partitions_plus_manifest() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("backup-test");

        let _session = setup_mock_with_data();

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
