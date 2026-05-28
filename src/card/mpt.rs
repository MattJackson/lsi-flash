//! `MptCard` — `Card` impl for Fusion-MPT chips (SAS2008/SAS2208/SAS3008).
//!
//! Implements the `Card` trait per ADR-017 (see
//! `/Users/mjackson/Developer/lsi-flash-notes/01-architecture/adr/017-card-trait-and-pluggable-transport.md`).
//! Wraps a pluggable `MptTransport` implementation — today only `Mpt3CtlTransport`
//! is supported (kernel-mediated via `/dev/mpt3ctl`). Future cycles may add
//! `VfioDoorbellTransport` for destructive ops.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::card::{
    BackupReport, Card, CardError, CardIdentity, ChipFamily, DetectReport, Personality,
};
use crate::mpi::messages::{FwUploadReply, ImageType, IocStatus, MpiFunction};
use crate::mpt::{Mpt3CtlTransport, MptTransport};

/// `Card` impl for Fusion-MPT chips (SAS2008, SAS2208, SAS3008, etc.).
/// Wraps a pluggable `MptTransport` per ADR-017.
pub struct MptCard {
    identity: CardIdentity,
    transport: Box<dyn MptTransport>,
}

impl MptCard {
    /// Discover an MptCard at the given BDF. Today: opens Mpt3CtlTransport
    /// (kernel-mediated, mpt3sas-bound). Future: falls back to
    /// VfioDoorbellTransport when mpt3sas isn't loaded.
    pub fn discover_one(bdf: &str) -> Result<Self, CardError> {
        // 1. Read VID/DID/subsys from sysfs using existing PCI helper
        let devices = crate::pci::discover_sas2008_devices_linux()
            .map_err(|e| CardError::PciEnumeration(format!("{}", e)))?;

        let dev = devices
            .iter()
            .find(|d| d.bdf == bdf)
            .ok_or(CardError::UnsupportedCard(0, 0))?;

        // 2. Map (VID, DID) → ChipFamily
        let chip_family = match (dev.vendor_id, dev.device_id) {
            (0x1000, 0x0072) => ChipFamily::Sas2008,
            (0x1000, 0x0084) => ChipFamily::Sas2208,
            (0x1000, 0x00C0) => ChipFamily::Sas3008,
            _ => ChipFamily::Unknown,
        };

        // 3. Look up friendly_name in card_database by full subsys
        let card_db = crate::card_database::load_embedded()
            .map_err(|e| CardError::PciEnumeration(e.to_string()))?;

        let friendly_name = crate::card_database::identify_card(
            &card_db,
            dev.vendor_id,
            dev.device_id,
            dev.subsystem_vendor_id,
            dev.subsystem_device_id,
        )
        .map(|info| info.display.clone());

        // 4. Open Mpt3CtlTransport — return CardError::Transport on failure
        let transport =
            Mpt3CtlTransport::open(bdf).map_err(|e| CardError::Transport(format!("{}", e)))?;

        // 5. Construct Self
        let identity = CardIdentity {
            bdf: bdf.to_string(),
            vendor_id: dev.vendor_id,
            device_id: dev.device_id,
            subsystem_vid: Some(dev.subsystem_vendor_id),
            subsystem_did: Some(dev.subsystem_device_id),
            chip_family,
            friendly_name,
        };

        Ok(Self {
            identity,
            transport: Box::new(transport),
        })
    }
}

impl Card for MptCard {
    fn identity(&self) -> &CardIdentity {
        &self.identity
    }

    fn detect(&mut self) -> Result<DetectReport, CardError> {
        // Build 12-byte IOC_FACTS request, send via transport, parse reply.
        // Reuse the body from cli/detect.rs::try_fetch_via_mpt3ctl (lines ~225-255).

        let mut req_bytes = [0u8; 12];
        req_bytes[0] = 0x00; // Reserved
        req_bytes[1] = 0x00; // Reserved
        req_bytes[2] = 0x00; // Reserved
        req_bytes[3] = MpiFunction::IocFacts.as_u8();
        req_bytes[4..6].copy_from_slice(&0u16.to_le_bytes()); // Reserved
        req_bytes[6] = 0x00; // MsgFlags
        req_bytes[7] = 0x00; // VP_ID
        req_bytes[8] = 0x00; // VF_ID
        req_bytes[8..12].copy_from_slice(&[0u8; 4]); // Reserved

        let mut reply_buf = [0u8; 64];
        let _bytes_written = self
            .transport
            .send_with_sge_offset(&req_bytes, 5, &mut reply_buf, None, None)
            .map_err(|e| CardError::Transport(format!("{}", e)))?;

        // Parse IOC_FACTS reply (we just need to verify it succeeded)
        let facts_len = reply_buf.len().min(12);
        if facts_len < 4 {
            return Err(CardError::Transport("IOC_FACTS reply too short".into()));
        }

        let ioc_status = u16::from_le_bytes([reply_buf[2], reply_buf[3]]);
        if ioc_status != IocStatus::Success as u16 {
            return Err(CardError::Transport(format!(
                "IOC_FACTS returned status 0x{:04x}",
                ioc_status
            )));
        }

        Ok(DetectReport {
            bdf: self.identity.bdf.clone(),
            chip_family: self.identity.chip_family,
        })
    }

    fn backup(&mut self, out_dir: &Path) -> Result<BackupReport, CardError> {
        // FOR EACH image_type in [Fw, Bios, FlashLayout]:
        //   - Build 36-byte MPI 2.0 FW_UPLOAD request (20-byte header +
        //     16-byte TCSGE). Cites: src/cli/backup.rs:185-209 (run_backup_via_mpt3ctl)
        //   - transport.send_with_sge_offset(req, 9, reply, Some(data_in), None)
        //   - FwUploadReply::parse; check ioc_status == Success
        //   - Write data_in[..actual_image_size] to {fw,bios,nvdata}.bin
        //   - sha256 + append to manifest.artifacts
        // Write manifest.toml. Return BackupReport.

        let mut artifacts = Vec::new();
        const UPLOAD_BUF_SIZE: usize = 2 * 1024 * 1024;

        for image_type in [ImageType::Fw, ImageType::Bios, ImageType::FlashLayout] {
            let mut data_in = vec![0u8; UPLOAD_BUF_SIZE];

            // Build the MPI 2.0 FW_UPLOAD request: 20-byte header + 16-byte TCSGE
            // Cites: src/cli/backup.rs:185-209 (run_backup_via_mpt3ctl)
            let mut req_bytes = Vec::with_capacity(36);

            // Header (20 bytes)
            req_bytes.push(image_type.as_u8()); // 0x00 ImageType
            req_bytes.push(0x00); // 0x01 Reserved1
            req_bytes.push(0x00); // 0x02 ChainOffset
            req_bytes.push(MpiFunction::FwUpload.as_u8()); // 0x03 Function
            req_bytes.extend_from_slice(&0u16.to_le_bytes()); // 0x04 Reserved2
            req_bytes.push(0x00); // 0x06 Reserved3
            req_bytes.push(0x00); // 0x07 MsgFlags
            req_bytes.push(0x00); // 0x08 VP_ID
            req_bytes.push(0x00); // 0x09 VF_ID
            req_bytes.extend_from_slice(&0u16.to_le_bytes()); // 0x0A Reserved4
            req_bytes.extend_from_slice(&[0u8; 4]); // 0x0C Reserved5
            req_bytes.extend_from_slice(&[0u8; 4]); // 0x10 Reserved6

            // TCSGE — 16 bytes total (4-byte tcsge header + 12-byte details)
            // Cites: src/cli/backup.rs:202-208 for exact layout
            req_bytes.push(0x00); // 0x14 Reserved1
            req_bytes.push(0x00); // 0x15 ContextSize = 0
            req_bytes.push(0x0C); // 0x16 DetailsLength = 12
            req_bytes.push(0x00); // 0x17 Flags = MPI_SGE_FLAGS_TRANSACTION_ELEMENT (0x00)
            req_bytes.extend_from_slice(&[0u8; 4]); // 0x18 Reserved2
            req_bytes.extend_from_slice(&[0u8; 4]); // 0x1C ImageOffset = 0
            req_bytes.extend_from_slice(&(UPLOAD_BUF_SIZE as u32).to_le_bytes()); // 0x20 ImageSize

            let mut reply_buf = vec![0u8; 64];
            let bytes_written = self
                .transport
                .send_with_sge_offset(&req_bytes, 9, &mut reply_buf, Some(&mut data_in), None)
                .map_err(|e| {
                    CardError::Transport(format!("FW_UPLOAD type={:?} send: {}", image_type, e))
                })?;

            let reply = FwUploadReply::parse(&reply_buf[..bytes_written.min(reply_buf.len())])
                .map_err(|e| {
                    CardError::Transport(format!(
                        "FW_UPLOAD type={:?} reply parse: {}",
                        image_type, e
                    ))
                })?;

            if reply.ioc_status != IocStatus::Success {
                return Err(CardError::Transport(format!(
                    "FW_UPLOAD type={:?} returned status 0x{:04x}",
                    image_type, reply.ioc_status as u16
                )));
            }

            let actual_size = (reply.actual_image_size as usize).min(data_in.len());
            let data = &data_in[..actual_size];

            let file_name = match image_type {
                ImageType::Fw => "firmware.bin",
                ImageType::Bios => "bios.rom",
                ImageType::FlashLayout => "nvdata.bin",
                _ => continue,
            };

            let path = out_dir.join(file_name);
            fs::write(&path, data).map_err(CardError::Io)?;

            let mut hasher = Sha256::new();
            hasher.update(data);
            let result = hasher.finalize();
            let sha256 = format!("{:x}", result);

            let artifact = crate::card::BackupArtifact {
                path: file_name.to_string(),
                image_type: format!("{:?}", image_type),
                sha256,
                size: actual_size as u64,
            };
            artifacts.push(artifact);
        }

        // Write manifest.toml
        let source_card_info = SourceCardInfo {
            pci_vid: self.identity.vendor_id,
            pci_did: self.identity.device_id,
            subsystem_vid: self.identity.subsystem_vid,
            subsystem_did: self.identity.subsystem_did,
            friendly_name: self.identity.friendly_name.clone(),
        };

        let mut manifest = BackupManifest {
            timestamp: chrono::Utc::now().to_rfc3339(),
            sas_wwn: None,
            artifacts: artifacts.clone(),
            source_card: Some(source_card_info),
        };

        // Set sas_wwn if we have it (from NVDATA parsing - stub for now)
        manifest.sas_wwn = None; // TODO: parse from nvdata.bin later

        let toml_str = toml::to_string_pretty(&manifest)
            .map_err(|e| CardError::Transport(format!("manifest serialize: {}", e)))?;

        let manifest_path = out_dir.join("manifest.toml");
        fs::write(manifest_path, toml_str).map_err(CardError::Io)?;

        Ok(BackupReport {
            timestamp: chrono::Utc::now().to_rfc3339(),
            artifacts_count: artifacts.len(),
            artifacts,
        })
    }

    fn current_personality(&mut self) -> Result<Personality, CardError> {
        // Stub for now — return NotImplemented. Real impl needs Mfg Page 0 read
        // via CONFIG ioctl which has its own off-by-N (see TODO in cli/detect.rs:265-269).
        Err(CardError::NotImplemented("current_personality"))
    }
}

// ============================================================================
// BackupManifest and SourceCardInfo for manifest writing (not returned in report)
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    timestamp: String,
    sas_wwn: Option<String>,
    artifacts: Vec<crate::card::BackupArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_card: Option<SourceCardInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SourceCardInfo {
    pci_vid: u16,
    pci_did: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    subsystem_vid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subsystem_did: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    friendly_name: Option<String>,
}

// ============================================================================
// Tests — verified against ADR-017 requirements
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::{Card, ChipFamily};

    /// Mock transport for testing MptCard without real hardware
    struct MockTransport {
        _phantom: (),
    }

    impl MptTransport for MockTransport {
        fn send_with_sge_offset(
            &mut self,
            request: &[u8],
            _data_sge_offset_words: u32,
            reply: &mut [u8],
            data_in: Option<&mut [u8]>,
            _data_out: Option<&[u8]>,
        ) -> Result<usize, crate::mpt::TransportError> {
            // Mock IOC_FACTS reply (12 bytes)
            if request.len() == 12 && request[3] == MpiFunction::IocFacts.as_u8() {
                reply[0] = 0x00; // Version
                reply[1] = 0x02; // MPI Version High
                reply[2] = 0x00; // IOC Status (Success)
                reply[3] = 0x00;
                return Ok(12);
            }

            // Mock FW_UPLOAD reply for each image_type
            if request.len() == 36 {
                let image_type_byte = request[0];
                let img_type = ImageType::from_u8(image_type_byte).unwrap_or(ImageType::Fw);

                // Write success status to reply
                reply[2] = 0x00; // IOC Status Success
                reply[3] = 0x00;

                // Write actual_image_size to offset 16-19 of reply (per FwUploadReply layout)
                let image_size = match img_type {
                    ImageType::Fw => 885000u32,         // ~885 KB from dev-1 measurement
                    ImageType::Bios => 65536u32,        // 64 KB BIOS ROM
                    ImageType::FlashLayout => 32768u32, // 32 KB NVDATA
                    _ => 0,
                };

                reply[16..20].copy_from_slice(&image_size.to_le_bytes());

                // Fill data_in with dummy image data
                if let Some(buf) = data_in {
                    for (i, byte) in buf.iter_mut().enumerate() {
                        *byte = (i % 256) as u8;
                    }
                }

                return Ok(32); // Reply size
            }

            Err(crate::mpt::TransportError::Other(
                "unexpected request".into(),
            ))
        }
    }

    /// MptCard::identity() returns its identity — construct an MptCard using a MockTransport
    #[test]
    fn test_identity_returns_correct_values() {
        let transport = Box::new(MockTransport { _phantom: () });

        let card = MptCard {
            identity: CardIdentity {
                bdf: "0000:03:00.0".to_string(),
                vendor_id: 0x1000,
                device_id: 0x0072,
                subsystem_vid: Some(0x1028),
                subsystem_did: Some(0x1F1D),
                chip_family: ChipFamily::Sas2008,
                friendly_name: Some("Dell PERC H200 Adapter".to_string()),
            },
            transport,
        };

        let identity = card.identity();
        assert_eq!(identity.bdf, "0000:03:00.0");
        assert_eq!(identity.vendor_id, 0x1000);
        assert_eq!(identity.device_id, 0x0072);
        assert_eq!(identity.subsystem_vid, Some(0x1028));
        assert_eq!(identity.subsystem_did, Some(0x1F1D));
        assert_eq!(identity.chip_family, ChipFamily::Sas2008);
        assert_eq!(
            identity.friendly_name,
            Some("Dell PERC H200 Adapter".to_string())
        );
    }

    /// MptCard::current_personality returns NotImplemented — locks the stub behavior
    #[test]
    fn test_current_personality_returns_not_implemented() {
        let transport = Box::new(MockTransport { _phantom: () });

        let mut card = MptCard {
            identity: CardIdentity {
                bdf: "0000:03:00.0".to_string(),
                vendor_id: 0x1000,
                device_id: 0x0072,
                subsystem_vid: None,
                subsystem_did: None,
                chip_family: ChipFamily::Sas2008,
                friendly_name: None,
            },
            transport,
        };

        let result = card.current_personality();
        assert!(matches!(
            result,
            Err(CardError::NotImplemented("current_personality"))
        ));
    }

    /// MptCard::detect returns DetectReport with correct chip_family via mock transport
    #[test]
    fn test_detect_returns_correct_report() {
        let transport = Box::new(MockTransport { _phantom: () });

        let mut card = MptCard {
            identity: CardIdentity {
                bdf: "0000:04:00.0".to_string(),
                vendor_id: 0x1000,
                device_id: 0x0072,
                subsystem_vid: Some(0x1028),
                subsystem_did: Some(0x1F51),
                chip_family: ChipFamily::Sas2008,
                friendly_name: Some("Dell H310 Mini Monolithics".to_string()),
            },
            transport,
        };

        let report = card.detect().unwrap();
        assert_eq!(report.bdf, "0000:04:00.0");
        assert_eq!(report.chip_family, ChipFamily::Sas2008);
    }

    /// MptCard::backup writes artifacts and manifest to output directory
    #[test]
    fn test_backup_writes_artifacts() {
        let transport = Box::new(MockTransport { _phantom: () });

        let mut card = MptCard {
            identity: CardIdentity {
                bdf: "0000:03:00.0".to_string(),
                vendor_id: 0x1000,
                device_id: 0x0072,
                subsystem_vid: Some(0x1028),
                subsystem_did: Some(0x1F1D),
                chip_family: ChipFamily::Sas2008,
                friendly_name: Some("Dell PERC H200 Adapter".to_string()),
            },
            transport,
        };

        let out_dir = std::env::temp_dir().join("lsi-flash-test-backup");
        let _ = fs::remove_dir_all(&out_dir); // Clean up if exists
        fs::create_dir_all(&out_dir).unwrap();

        let report = card.backup(&out_dir).unwrap();

        assert_eq!(report.artifacts_count, 3); // fw, bios, nvdata

        // Verify files exist
        assert!(out_dir.join("firmware.bin").exists());
        assert!(out_dir.join("bios.rom").exists());
        assert!(out_dir.join("nvdata.bin").exists());
        assert!(out_dir.join("manifest.toml").exists());

        // Clean up
        let _ = fs::remove_dir_all(&out_dir);
    }

    /// Verify MptCard implements Send (required by Card trait)
    #[test]
    fn mptcard_is_send() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<MptCard>();
    }
}
