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
    RestoreReport,
};
use crate::mpi::messages::{FwDownloadReply, FwUploadReply, ImageType, IocStatus, MpiFunction};
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

    /// Write a previously-captured backup's firmware regions back to THIS card
    /// via FW_DOWNLOAD. Destructive. Per ADR-015 Rule 8 (non-destructive
    /// round-trip), restoring the same OEM firmware is the safe first write test.
    ///
    /// Cites: ADR-015 at `/Users/mjackson/Developer/lsi-flash-notes/01-architecture/adr/015-brick-post-mortem-rules.md`
    /// - Rule 6: any non-Success IOCStatus during flash = immediate hard stop
    /// - Rule 8: restoring same OEM firmware is the sanctioned safe write
    /// - Rule 11a: pre-flight region size guard before first destructive byte
    ///
    /// Chunking per R0 RE doc (`fw-download-write-sequence.md`): 16 KB chunks,
    /// LAST_SEGMENT flag on final chunk only. Skip nvdata (OPEN issue with ITYPE
    /// asymmetry between upload=0x06 vs download=0x03).
    fn restore(&mut self, backup_dir: &Path) -> Result<RestoreReport, CardError> {
        use std::fs;

        const CHUNK_SIZE: usize = 0x4000; // 16 KB per lsiutil.c:210

        let mut regions_written = 0usize;
        let mut region_names = Vec::new();

        // Regions to restore: FW (0x01) then BIOS (0x02) only. Skip FlashLayout/NVDATA
        // due to ITYPE asymmetry (backup captured as 0x06, lsiutil downloads as 0x03).
        let regions_to_restore = [
            (ImageType::Fw, "firmware.bin"),
            (ImageType::Bios, "bios.rom"),
        ];

        for (image_type, file_name) in regions_to_restore {
            let file_path = backup_dir.join(file_name);

            // Read the region file bytes
            let mut file_bytes = fs::read(&file_path).map_err(CardError::Io)?;

            if file_bytes.is_empty() {
                return Err(CardError::Transport(format!(
                    "Region file {} is empty",
                    file_name
                )));
            }

            // Pre-flight integrity guard (ADR-015 Rule 5): verify manifest records match disk.
            // For R1, validate backup artifacts before any FW_DOWNLOAD to prevent writing
            // corrupted/truncated images. Live FLASH_LAYOUT capacity guard (Rule 11a) remains OPEN.
            let backup_manifest_path = backup_dir.join("manifest.toml");
            let manifest_content = fs::read_to_string(&backup_manifest_path).map_err(|e| {
                CardError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "Backup manifest not found at {:?}: {}",
                        backup_manifest_path, e
                    ),
                ))
            })?;

            let manifest: BackupManifest = toml::from_str(&manifest_content).map_err(|e| {
                CardError::Transport(format!("Failed to parse backup manifest: {}", e))
            })?;

            // Find and verify each region artifact in the manifest
            let artifact = manifest
                .artifacts
                .iter()
                .find(|a| a.path == file_name)
                .ok_or(CardError::Transport(format!(
                    "Region {} not found in backup manifest - refusing to write unverified image",
                    file_name
                )))?;

            // Verify size matches
            if artifact.size as usize != file_bytes.len() {
                return Err(CardError::Transport(format!(
                    "Size mismatch for {}: disk={} vs manifest={}",
                    file_name,
                    file_bytes.len(),
                    artifact.size
                )));
            }

            // Verify SHA256 matches
            let mut hasher = Sha256::new();
            hasher.update(&file_bytes);
            let result = hasher.finalize();
            let computed_sha256 = format!("{:x}", result);
            if computed_sha256 != artifact.sha256 {
                return Err(CardError::Transport(format!(
                    "SHA256 mismatch for {}: disk={} vs manifest={}",
                    file_name, computed_sha256, artifact.sha256
                )));
            }

            // Build the MPI 2.0 FW_DOWNLOAD request: 36 bytes (20-byte header + 16-byte TCSGE)
            // Per fw-download-write-sequence.md §4 layout:
            // - Function = 0x09 (FwDownload) at offset 0x03
            // - ImageType at offset 0x00
            // - MsgFlags = 0x01 on last chunk, else 0x00 at offset 0x07
            // - TotalImageSize at offset 0x0C..0x0F (full file len)
            // - ImageOffset at offset 0x1C..0x1F (per-chunk advancing offset)
            // - ImageSize at offset 0x20..0x23 (per-chunk size, =CHUNK_SIZE except final)

            let mut offset: usize = 0;
            let total_size = file_bytes.len() as u32;

            while offset < file_bytes.len() {
                let chunk_size = (file_bytes.len() - offset).min(CHUNK_SIZE);
                let is_last_chunk = (offset + chunk_size) >= file_bytes.len();

                // Build 36-byte FW_DOWNLOAD request inline
                let mut req_bytes = [0u8; 36];

                // Header (20 bytes)
                req_bytes[0] = image_type.as_u8(); // ImageType at offset 0x00
                req_bytes[1] = 0x00; // Reserved1
                req_bytes[2] = 0x00; // ChainOffset
                req_bytes[3] = MpiFunction::FwDownload.as_u8(); // Function at offset 0x03
                req_bytes[4..6].copy_from_slice(&0u16.to_le_bytes()); // Reserved2
                let msg_flags = if is_last_chunk { 0x01 } else { 0x00 };
                req_bytes[7] = msg_flags; // MsgFlags at offset 0x07 (LAST_SEGMENT on final)
                req_bytes[8] = 0x00; // VP_ID
                req_bytes[9] = 0x00; // VF_ID
                req_bytes[10..12].copy_from_slice(&0u16.to_le_bytes()); // Reserved4
                req_bytes[12..16].copy_from_slice(&total_size.to_le_bytes()); // TotalImageSize at offset 0x0C (full file len)
                req_bytes[16..20].copy_from_slice(&(0u32).to_le_bytes()); // Reserved6

                // TCSGE (16 bytes)
                req_bytes[20] = 0x00; // Reserved1 at offset 0x14
                req_bytes[21] = 0x00; // ContextSize at offset 0x15 = 0
                req_bytes[22] = 0x0C; // DetailsLength at offset 0x16 = 12
                req_bytes[23] = 0x00; // Flags at offset 0x17 = TRANSACTION_ELEMENT (0x00)
                req_bytes[24..28].copy_from_slice(&(0u32).to_le_bytes()); // Reserved2 at offset 0x18
                req_bytes[28..32].copy_from_slice(&(offset as u32).to_le_bytes()); // ImageOffset at offset 0x1C
                req_bytes[32..36].copy_from_slice(&(chunk_size as u32).to_le_bytes()); // ImageSize at offset 0x20

                let mut reply_buf = [0u8; 64];
                let bytes_written = self
                    .transport
                    .send_with_sge_offset(
                        &req_bytes,
                        9, // data_sge_offset_words = 36/4 words
                        &mut reply_buf,
                        None, // data_in: host→IOC for download
                        Some(&mut file_bytes[offset..offset + chunk_size]),
                    )
                    .map_err(|e| {
                        CardError::Transport(format!(
                            "FW_DOWNLOAD type={:?} offset={} size={} send: {}",
                            image_type, offset, chunk_size, e
                        ))
                    })?;

                let reply =
                    FwDownloadReply::parse(&reply_buf[..bytes_written.min(reply_buf.len())])
                        .map_err(|e| {
                            CardError::Transport(format!(
                                "FW_DOWNLOAD type={:?} offset={} reply parse: {}",
                                image_type, offset, e
                            ))
                        })?;

                // ADR-015 Rule 6: hard stop on any non-Success IOCStatus
                if reply.ioc_status != IocStatus::Success {
                    return Err(CardError::Transport(format!(
                        "FW_DOWNLOAD type={:?} offset={} returned status 0x{:04x}",
                        image_type, offset, reply.ioc_status as u16
                    )));
                }

                offset += chunk_size;
            }

            regions_written += 1;
            region_names.push(file_name.to_string());
        }

        Ok(RestoreReport {
            timestamp: chrono::Utc::now().to_rfc3339(),
            regions_written,
            regions: region_names,
        })
    }

    /// Read the 256-byte SBR from the chip's I2C EEPROM via TOOLBOX_ISTWI.
    ///
    /// Wire format per `mpi2_tool.h:171-200` (`MPI2_TOOLBOX_ISTWI_READ_WRITE_REQUEST`):
    /// - Tool = 0x03 (MPI2_TOOLBOX_ISTWI_READ_WRITE_TOOL) at offset 0x00
    /// - Function = 0x17 (MPI_FUNCTION_TOOLBOX) at offset 0x03
    /// - DevIndex at offset 0x14 — SBR EEPROM is typically 0x00 on SAS2008 (first ISTWI device)
    /// - Action = 0x01 (MPI2_TOOL_ISTWI_ACTION_READ_DATA) at offset 0x15
    /// - TxDataLength = 0 at offset 0x18 (pure read)
    /// - RxDataLength = 256 at offset 0x1A (full SBR)
    /// - Total header/body size = 48 bytes (0x30), SGL at offset 0x30
    ///
    /// Reply format per `mpi2_tool.h:214-228` (`MPI2_TOOLBOX_ISTWI_REPLY`):
    /// - IOCStatus at offset 0x0E (U16 LE) — 0 = SUCCESS
    /// - IstwiStatus at offset 0x16 (U8)
    ///
    /// Transport: uses Bar1MmapSbrTransport (DIRECT BAR1 mmap, NO VFIO RESET).
    /// Direct `/sys/.../resource1` mmap via lsirec-style pattern — unbinds mpt3sas
    /// briefly (~1s blip), reads SBR via I²C bit-bang on BAR1, rebinds on Drop.
    /// NO device reset → SAS PHY links stay up (no reboot required).
    ///
    /// Fallback: VfioI2cSbrTransport retained for kernel-lockdown / SecureBoot
    /// systems where raw resource1 mmap is blocked. NOT the default due to
    /// disk yank / reboot cost of VFIO bind dance.
    fn sbr_read(&mut self) -> Result<[u8; 256], CardError> {
        use crate::sbr::transport::{Bar1MmapSbrTransport, SbrTransport};

        // Transport override for experimentation: LSI_SBR_TRANSPORT=istwi|vfio
        // (default = bar1-mmap). ISTWI is kernel-mediated (mpt3sas stays bound,
        // no disk disruption) — used to prove out the zero-disruption path.
        match std::env::var("LSI_SBR_TRANSPORT").as_deref() {
            Ok("istwi") => {
                use crate::sbr::transport::IstwiSbrTransport;
                let transport = crate::mpt::Mpt3CtlTransport::open(&self.identity.bdf)
                    .map_err(|e| CardError::Transport(format!("istwi mpt3ctl open: {}", e)))?;
                let mut t = IstwiSbrTransport {
                    transport: Box::new(transport),
                };
                return t
                    .read_sbr()
                    .map_err(|e| CardError::Transport(format!("sbr {}: {}", t.name(), e)));
            }
            Ok("vfio") => {
                use crate::sbr::transport::VfioI2cSbrTransport;
                let mut t = VfioI2cSbrTransport::open(&self.identity.bdf)
                    .map_err(|e| CardError::Transport(format!("vfio sbr transport: {}", e)))?;
                return t
                    .read_sbr()
                    .map_err(|e| CardError::Transport(format!("sbr {}: {}", t.name(), e)));
            }
            _ => {}
        }

        // PRIMARY: direct BAR1 mmap (lsirec.c:205-213 style). NO VFIO, NO reset.
        let mut t = Bar1MmapSbrTransport::open(&self.identity.bdf)
            .map_err(|e| CardError::Transport(format!("sbr transport: {}", e)))?;
        let bytes = t
            .read_sbr()
            .map_err(|e| CardError::Transport(format!("sbr {}: {}", t.name(), e)))?;
        Ok(bytes)

        /* Fallback (kernel-lockdown / SecureBoot): uncomment if resource1 mmap fails:
        use crate::sbr::transport::{SbrTransport, VfioI2cSbrTransport};
        let mut t = VfioI2cSbrTransport::open(&self.identity.bdf)
            .map_err(|e| CardError::Transport(format!("vfio fallback sbr transport: {}", e)))?;
        let bytes = t.read_sbr()
            .map_err(|e| CardError::Transport(format!("sbr {}: {}", t.name(), e)))?;
        Ok(bytes)
        */
    }
}

// ============================================================================
// Tests — verified against ADR-017 requirements
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::{chip_family_from_pci, Card, ChipFamily};

    /// Mock transport for testing MptCard without real hardware.
    /// Captures requests and returns canned responses for various MPI operations.
    struct MockTransport {
        _phantom: (),
    }

    impl MptTransport for MockTransport {
        fn send_with_sge_offset(
            &mut self,
            request: &[u8],
            data_sge_offset_words: u32,
            reply: &mut [u8],
            data_in: Option<&mut [u8]>,
            _data_out: Option<&mut [u8]>,
        ) -> Result<usize, crate::mpt::TransportError> {
            // Mock IOC_FACTS reply - per detect() expects IOCStatus at offset 2-3 in 12-byte request
            if request.len() == 12 && request[3] == MpiFunction::IocFacts.as_u8() {
                reply[2] = 0x00; // IOC Status Success (offset 2-3 per detect() read)
                reply[3] = 0x00;
                return Ok(16);
            }

            // Mock FW_UPLOAD reply for each image_type
            if request.len() == 36 && request[3] == MpiFunction::FwUpload.as_u8() {
                let image_type_byte = request[0];
                let img_type = ImageType::from_u8(image_type_byte).unwrap_or(ImageType::Fw);

                // Write success status to reply (offset 14-15 for IOCStatus)
                reply[14] = 0x00; // IOC Status Success low byte
                reply[15] = 0x00; // IOC Status Success high byte

                // Write actual_image_size to offset 20-23 of reply (per FwUploadReply layout)
                let image_size = match img_type {
                    ImageType::Fw => 885000u32,         // ~885 KB from dev-1 measurement
                    ImageType::Bios => 65536u32,        // 64 KB BIOS ROM
                    ImageType::FlashLayout => 32768u32, // 32 KB NVDATA
                    _ => 0,
                };

                reply[20..24].copy_from_slice(&image_size.to_le_bytes());

                // Fill data_in with dummy image data if provided
                if let Some(buf) = data_in {
                    for (i, byte) in buf.iter_mut().enumerate() {
                        *byte = (i % 256) as u8;
                    }
                }

                return Ok(32); // Reply size
            }

            // Mock FW_DOWNLOAD reply for each image_type (Function=0x09)
            if request.len() == 36 && request[3] == MpiFunction::FwDownload.as_u8() {
                let image_type_byte = request[0];
                let _img_type = ImageType::from_u8(image_type_byte).unwrap_or(ImageType::Fw);

                // Write success status to reply (offset 14-15 for IOCStatus)
                reply[14] = 0x00; // IOC Status Success low byte
                reply[15] = 0x00; // IOC Status Success high byte

                return Ok(32); // Reply size
            }

            // Mock TOOLBOX_ISTWI reply - captures request bytes for verification
            if request.len() == 48 && request[0] == 0x03 {
                // Verify it's a TOOLBOX_ISTWI_READ_WRITE_TOOL request (Tool=0x03)
                assert_eq!(
                    request[3],
                    MpiFunction::Toolbox.as_u8(),
                    "Function must be Toolbox"
                );
                assert_eq!(
                    data_sge_offset_words, 12,
                    "SGL offset must be 12 words (48 bytes)"
                );

                // Check Action field at offset 0x15 = READ_DATA (0x01)
                if request.len() > 0x15 {
                    assert_eq!(request[0x15], 0x01, "Action must be READ_DATA");
                }

                // Check RxDataLength at offset 0x1A-0x1B = 256
                if request.len() > 0x1C {
                    let rx_len = u16::from_le_bytes([request[0x1A], request[0x1B]]);
                    assert_eq!(rx_len, 256, "RxDataLength must be 256");
                }

                // Fill data_in (the sbr buffer) with a canned 256-byte payload.
                // Note: TOOLBOX_ISTWI reads flow IOC→host via data_in parameter per mpt3ctl semantics.
                if let Some(buf) = data_in {
                    for (i, byte) in buf.iter_mut().enumerate() {
                        *byte = (i % 256) as u8;
                    }
                }

                // Write success status to reply at offset 0x0E-0x0F (IOCStatus)
                reply[0x0E] = 0x00; // IOC Status Success low byte
                reply[0x0F] = 0x00; // IOC Status Success high byte

                return Ok(16); // Reply size for TOOLBOX_ISTWI_REPLY header
            }

            Err(crate::mpt::TransportError::Other(
                "unexpected request".into(),
            ))
        }
    }

    /// Test chip_family_from_pci mapping for known SAS2008 VID:DID pairs.
    #[test]
    fn test_chip_family_from_pci_known_mappings() {
        // LSI 9211-8i IT/IR (Dell H200 Adapter) - confirmed in card-database.toml
        assert_eq!(chip_family_from_pci(0x1000, 0x0072), ChipFamily::Sas2008);

        // LSI 9211-8i IMR (Dell H310 Mini Mono) - confirmed in card-database.toml
        assert_eq!(chip_family_from_pci(0x1000, 0x0073), ChipFamily::Sas2008);

        // Sas2208 single confirmed entry (Lenovo ServeRAID M5110)
        assert_eq!(chip_family_from_pci(0x1000, 0x0084), ChipFamily::Sas2208);

        // Sas3008 single confirmed entry (LSI 9300 series)
        assert_eq!(chip_family_from_pci(0x1000, 0x00C0), ChipFamily::Sas3008);
    }

    /// Test chip_family_from_pci returns Unknown for unknown VID:DID pairs.
    #[test]
    fn test_chip_family_from_pci_unknown() {
        // Completely unknown device
        assert_eq!(chip_family_from_pci(0x1234, 0x5678), ChipFamily::Unknown);

        // LSI VID but unknown DID in Sas2208 range marked Unknown
        assert_eq!(chip_family_from_pci(0x1000, 0x0085), ChipFamily::Unknown);

        // LSI VID but unknown DID in Sas3008 range marked Unknown
        assert_eq!(chip_family_from_pci(0x1000, 0x00C1), ChipFamily::Unknown);
    }

    /// Test MptCard::sbr_read via MockTransport - verifies wire format and canned response.
    /// NOTE: This test is ignored because sbr_read now uses VfioI2cSbrTransport which requires
    /// VFIO infrastructure (not available in tests). The ISTWI path remains gated behind NotImplemented.
    #[test]
    #[ignore = "sbr_read now uses VfioI2cSbrTransport; ISTWI path returns NotImplemented"]
    fn test_mptcard_sbr_read_via_mock_transport() {
        let transport = Box::new(MockTransport { _phantom: () });

        let mut card = MptCard {
            identity: CardIdentity {
                bdf: "0000:03:00.0".to_string(),
                vendor_id: 0x1000,
                device_id: 0x0072,
                subsystem_vid: Some(0x1028),
                subsystem_did: Some(0x1F1D),
                chip_family: ChipFamily::Sas2008,
                friendly_name: None,
            },
            transport,
        };

        let sbr_bytes = card.sbr_read().expect("sbr_read should succeed with mock");

        // Verify the returned bytes match the canned payload (i % 256)
        for (i, &byte) in sbr_bytes.iter().enumerate() {
            assert_eq!(
                byte,
                (i % 256) as u8,
                "SBR byte at offset {} should match canned payload",
                i
            );
        }

        // Verify first few bytes are predictable
        assert_eq!(sbr_bytes[0], 0x00);
        assert_eq!(sbr_bytes[1], 0x01);
        assert_eq!(sbr_bytes[255], 0xFF);
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

    /// restore() sends FW_DOWNLOAD for each region with correct function code (0x09)
    #[test]
    fn test_restore_sends_fw_download_per_region() {
        struct MockTransportWithCapture {
            captured_requests: Arc<std::sync::Mutex<Vec<(ImageType, usize, u32)>>>,
        }

        impl MptTransport for MockTransportWithCapture {
            fn send_with_sge_offset(
                &mut self,
                request: &[u8],
                _data_sge_offset_words: u32,
                reply: &mut [u8],
                _data_in: Option<&mut [u8]>,
                data_out: Option<&mut [u8]>,
            ) -> Result<usize, crate::mpt::TransportError> {
                // Handle FW_DOWNLOAD (Function=0x09)
                if request.len() == 36 && request[3] == MpiFunction::FwDownload.as_u8() {
                    let image_type = ImageType::from_u8(request[0]).unwrap_or(ImageType::Fw);

                    // BUG-1 FIX VERIFICATION: Extract TotalImageSize from bytes 0x0C..0x10
                    let total_image_size =
                        u32::from_le_bytes([request[12], request[13], request[14], request[15]]);

                    // Capture the call with region, chunk size, and TotalImageSize
                    let chunk_size =
                        u32::from_le_bytes([request[32], request[33], request[34], request[35]])
                            as usize;

                    self.captured_requests.lock().unwrap().push((
                        image_type,
                        chunk_size,
                        total_image_size,
                    ));

                    // Verify data_out carries the file bytes (host→IOC)
                    if let Some(buf) = data_out {
                        assert!(!buf.is_empty(), "data_out should carry file bytes");
                    } else {
                        panic!("data_out should be Some for FW_DOWNLOAD");
                    }

                    // Fill reply with success status
                    reply[14] = 0x00;
                    reply[15] = 0x00;

                    return Ok(32);
                }

                Err(crate::mpt::TransportError::Other(
                    "unexpected request".into(),
                ))
            }
        }

        let out_dir = std::env::temp_dir().join("lsi-flash-test-restore");
        let _ = fs::remove_dir_all(&out_dir);
        fs::create_dir_all(&out_dir).unwrap();

        // Create test firmware and bios files with known sizes
        let fw_size = 1000usize;
        let bios_size = 500usize;
        fs::write(out_dir.join("firmware.bin"), vec![0xAA; fw_size]).unwrap();
        fs::write(out_dir.join("bios.rom"), vec![0xBB; bios_size]).unwrap();

        // Create manifest with correct SHA256 hashes so restore proceeds
        let mut hasher = Sha256::new();
        hasher.update(vec![0xAA; fw_size]);
        let result = hasher.finalize();
        let fw_sha256 = format!("{:x}", result);

        let mut hasher = Sha256::new();
        hasher.update(vec![0xBB; bios_size]);
        let result = hasher.finalize();
        let bios_sha256 = format!("{:x}", result);

        let manifest = BackupManifest {
            timestamp: chrono::Utc::now().to_rfc3339(),
            sas_wwn: None,
            artifacts: vec![
                BackupArtifact {
                    path: "firmware.bin".to_string(),
                    image_type: "Fw".to_string(),
                    sha256: fw_sha256.clone(),
                    size: fw_size as u64,
                },
                BackupArtifact {
                    path: "bios.rom".to_string(),
                    image_type: "Bios".to_string(),
                    sha256: bios_sha256.clone(),
                    size: bios_size as u64,
                },
            ],
            source_card: None,
        };

        let toml_str = toml::to_string_pretty(&manifest).unwrap();
        fs::write(out_dir.join("manifest.toml"), toml_str).unwrap();

        use crate::card::BackupArtifact;
        use std::sync::{Arc, Mutex};
        let captured_requests: Arc<Mutex<Vec<(ImageType, usize, u32)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(MockTransportWithCapture {
            captured_requests: captured_requests.clone(),
        });

        let mut card = MptCard {
            identity: CardIdentity {
                bdf: "0000:03:00.0".to_string(),
                vendor_id: 0x1000,
                device_id: 0x0072,
                subsystem_vid: Some(0x1028),
                subsystem_did: Some(0x1F1D),
                chip_family: ChipFamily::Sas2008,
                friendly_name: None,
            },
            transport,
        };

        let report = card.restore(&out_dir).unwrap();

        assert_eq!(report.regions_written, 2); // fw + bios
        assert_eq!(report.regions.len(), 2);
        assert!(report.regions.contains(&"firmware.bin".to_string()));
        assert!(report.regions.contains(&"bios.rom".to_string()));

        let calls = captured_requests.lock().unwrap().clone();

        // Should have at least 2 FW_DOWNLOAD calls (one per region)
        assert!(
            calls.len() >= 2,
            "Should have at least 2 FW_DOWNLOAD calls (one per region)"
        );

        // BUG-1 FIX VERIFICATION: TotalImageSize must equal actual file length for each chunk
        let mut fw_total_size_seen = None;
        let mut bios_total_size_seen = None;

        for (image_type, _chunk_size, total_image_size) in &calls {
            match image_type {
                ImageType::Fw => {
                    fw_total_size_seen = Some(*total_image_size);
                    // This assertion would FAIL with old 0u32 code and PASS after fix
                    assert_eq!(
                        *total_image_size, fw_size as u32,
                        "TotalImageSize for firmware.bin must equal file length ({})",
                        fw_size
                    );
                }
                ImageType::Bios => {
                    bios_total_size_seen = Some(*total_image_size);
                    // This assertion would FAIL with old 0u32 code and PASS after fix
                    assert_eq!(
                        *total_image_size, bios_size as u32,
                        "TotalImageSize for bios.rom must equal file length ({})",
                        bios_size
                    );
                }
                _ => {}
            }
        }

        // Verify we saw TotalImageSize for both regions
        assert!(
            fw_total_size_seen.is_some(),
            "Should have seen firmware.bin TotalImageSize"
        );
        assert!(
            bios_total_size_seen.is_some(),
            "Should have seen bios.rom TotalImageSize"
        );

        // Clean up
        let _ = fs::remove_dir_all(&out_dir);
    }

    /// restore() hard stops on IOCStatus failure (ADR-015 Rule 6)
    #[test]
    fn test_restore_hard_stops_on_iocstatus_failure() {
        struct MockTransportWithFailure;

        impl MptTransport for MockTransportWithFailure {
            fn send_with_sge_offset(
                &mut self,
                request: &[u8],
                _data_sge_offset_words: u32,
                reply: &mut [u8],
                _data_in: Option<&mut [u8]>,
                _data_out: Option<&mut [u8]>,
            ) -> Result<usize, crate::mpt::TransportError> {
                if request.len() == 36 && request[3] == MpiFunction::FwDownload.as_u8() {
                    let image_type = ImageType::from_u8(request[0]).unwrap_or(ImageType::Fw);

                    // Return failure on second region (BIOS)
                    if image_type == ImageType::Bios {
                        reply[14] = 0x04; // IOCStatus::InternalError
                        reply[15] = 0x00;
                    } else {
                        reply[14] = 0x00;
                        reply[15] = 0x00;
                    }

                    return Ok(32);
                }

                Err(crate::mpt::TransportError::Other(
                    "unexpected request".into(),
                ))
            }
        }

        let out_dir = std::env::temp_dir().join("lsi-flash-test-restore-fail");
        let _ = fs::remove_dir_all(&out_dir);
        fs::create_dir_all(&out_dir).unwrap();

        // Create test files
        let fw_content = vec![0xAA; 100];
        let bios_content = vec![0xBB; 200];
        fs::write(out_dir.join("firmware.bin"), &fw_content).unwrap();
        fs::write(out_dir.join("bios.rom"), &bios_content).unwrap();

        // Create manifest with correct SHA256 hashes so restore proceeds past integrity check
        use crate::card::BackupArtifact;

        let mut hasher = Sha256::new();
        hasher.update(&fw_content);
        let result = hasher.finalize();
        let fw_sha256 = format!("{:x}", result);

        let mut hasher = Sha256::new();
        hasher.update(&bios_content);
        let result = hasher.finalize();
        let bios_sha256 = format!("{:x}", result);

        let manifest = BackupManifest {
            timestamp: chrono::Utc::now().to_rfc3339(),
            sas_wwn: None,
            artifacts: vec![
                BackupArtifact {
                    path: "firmware.bin".to_string(),
                    image_type: "Fw".to_string(),
                    sha256: fw_sha256.clone(),
                    size: 100u64,
                },
                BackupArtifact {
                    path: "bios.rom".to_string(),
                    image_type: "Bios".to_string(),
                    sha256: bios_sha256.clone(),
                    size: 200u64,
                },
            ],
            source_card: None,
        };

        let toml_str = toml::to_string_pretty(&manifest).unwrap();
        fs::write(out_dir.join("manifest.toml"), toml_str).unwrap();

        let transport = Box::new(MockTransportWithFailure);

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

        let result = card.restore(&out_dir);

        // Should fail on BIOS region with non-Success IOCStatus
        assert!(result.is_err());

        if let Err(e) = result {
            assert!(
                format!("{}", e).to_lowercase().contains("status"),
                "Error should mention status"
            );
        }

        // Clean up
        let _ = fs::remove_dir_all(&out_dir);
    }

    /// restore() refuses corrupt or truncated region via manifest integrity check
    #[test]
    fn test_restore_refuses_corrupt_or_truncated_region() {
        use crate::card::BackupArtifact;

        // Create a backup directory with valid files but mismatched manifest
        let out_dir = std::env::temp_dir().join("lsi-flash-test-restore-corrupt");
        let _ = fs::remove_dir_all(&out_dir);
        fs::create_dir_all(&out_dir).unwrap();

        // Create firmware.bin with known content
        let original_content = vec![0xCC; 256];
        fs::write(out_dir.join("firmware.bin"), &original_content).unwrap();

        // Compute correct SHA256
        let mut hasher = Sha256::new();
        hasher.update(&original_content);
        let _result = hasher.finalize();

        // Create manifest with WRONG sha256 (simulating corruption)
        let manifest = BackupManifest {
            timestamp: chrono::Utc::now().to_rfc3339(),
            sas_wwn: None,
            artifacts: vec![BackupArtifact {
                path: "firmware.bin".to_string(),
                image_type: "Fw".to_string(),
                sha256: "wrongsha256value1234567890abcdef1234567890abcdef1234567890abcd"
                    .to_string(), // WRONG!
                size: 256u64,
            }],
            source_card: None,
        };

        let toml_str = toml::to_string_pretty(&manifest).unwrap();
        fs::write(out_dir.join("manifest.toml"), toml_str).unwrap();

        // Create MptCard with mock transport that should NEVER be called
        struct MockTransportShouldNotRun;
        impl MptTransport for MockTransportShouldNotRun {
            fn send_with_sge_offset(
                &mut self,
                _request: &[u8],
                _data_sge_offset_words: u32,
                _reply: &mut [u8],
                _data_in: Option<&mut [u8]>,
                _data_out: Option<&mut [u8]>,
            ) -> Result<usize, crate::mpt::TransportError> {
                panic!("Should not send FW_DOWNLOAD after manifest integrity failure");
            }
        }

        let transport = Box::new(MockTransportShouldNotRun);

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

        let result = card.restore(&out_dir);

        // Should fail with Transport error about SHA256 mismatch
        assert!(result.is_err());
        if let Err(e) = result {
            let err_str = format!("{}", e);
            assert!(
                err_str.to_lowercase().contains("sha256")
                    || err_str.to_lowercase().contains("mismatch"),
                "Error should mention SHA256 or mismatch: {}",
                err_str
            );
        }

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
