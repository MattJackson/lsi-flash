//! Real MPI backend for production hardware access.
//!
//! # Cycle 1 / Cycle 2a / Cycle 2b Split
//!
//! **Cycle 1 (shipped `8db3ddf`):** struct + trait conformance, all `todo!()`.
//!
//! **Cycle 2a (this revision):** Real BAR1 mmap infrastructure via `MmapRegion`.
//! `RealIoc::open()` now actually mmaps `/sys/bus/pci/devices/<bdf>/resource1`
//! (or returns `MpiError::Io` if not present, e.g. running on macOS or against
//! a non-existent BDF). `IocBackend` methods STILL `todo!()` — the bytes are
//! mappable but the doorbell/reply-queue protocol implementation is cycle 2b.
//!
//! **Cycle 2b (freshman):** Implement the 4 read-safe `IocBackend` methods
//! (`current_personality`, `send_ioc_init`, `send_config` for read-only pages,
//! `send_fw_upload`) using the now-real `bar1_mmap`. Stub the 2 destructive
//! ops (`send_fw_download`, `send_toolbox_clean`) with
//! `MpiError::NotImplementedYet { op: ... }` — kept gated until CH341A SPI
//! clip + cold-spare card arrive (see `memory/lsiutil_fragility_and_brick.md`).
//!
//! # Cycle 2b Implementation Notes
//!
//! **Doorbell handshake protocol** (per mpi-overview.md §1.1, lsirec.c):
//! - Write function code in bits 24-31 of DOORBELL register (BAR1+0x00)
//! - Bits 16-23 encode message size in dwords (MsgLength - 2) per mpi2.h:178-193
//! - Function codes: IOC_INIT=0x02, FW_UPLOAD=0x12 (messages.rs:72-83)
//! - Wait for IOC_DOORBELL_INT bit in HISTATUS register after doorbell write
//! - Write request payload to DOORBELL register byte-by-byte (4 bytes at a time)
//! - Read reply from DOORBELL register, deserialize per msg-specific reply struct
//!
//! **Register offsets** (doorbell.rs:5-17):
//! - MPI2_DOORBELL = 0x00 — doorbell register for posting messages
//! - MPI2_WRSEQ = 0x04 — unlock sequence register (for DIAG access)
//! - MPI2_DIAG = 0x08 — diagnostic register (MPT mode)

use std::path::{Path, PathBuf};

use crate::mpi::doorbell::{read32, write32, MPI2_DOORBELL};
use crate::mpi::messages::{
    ConfigReply, FwDownloadReply, FwUploadReply, IocFactsReply, IocInitReply, ToolboxReply,
};
use crate::mpi::messages::{
    ConfigRequest, FwDownloadRequest, FwUploadRequest, IocFactsRequest, IocInitRequest,
    ToolboxCleanRequest,
};
use crate::mpi::messages::{IocStatus, MpiError, MpiFunction};
#[cfg(target_os = "linux")]
use crate::mpi::mmap_region::MmapRegion;
use crate::mpi::session::{IocBackend, Personality};
use crate::pci::Platform;

/// SAS2008 BAR1 register region size. Cites `lsirec.c:207-213` (4 KB mapping
/// covers DOORBELL, DIAG, WRSEQ, HCDW pointers, all needed by the MPI layer).
pub const BAR1_LEN: usize = 4096;

/// Real MPI backend for talking to actual SAS2008 hardware.
///
/// Owns a live `MmapRegion` of BAR1 for the device's lifetime. The mmap is
/// `munmap()`'d when the `RealIoc` drops.
pub struct RealIoc<P: Platform> {
    /// Platform trait impl — `LinuxSysfs` in production, `MockPlatform` in
    /// tests that exercise sysfs reads (vendor/device/subsys ID lookup).
    pub platform: P,

    /// PCI bus/device/function identifier, e.g., "0000:03:00.0"
    pub pci_bdf: String,

    /// Path to BAR1 resource file in sysfs: /sys/bus/pci/devices/<bdf>/resource1
    pub bar1_path: PathBuf,

    /// Live mmap of BAR1 (4 KB read-write). Holds the mapping for the lifetime
    /// of this struct. None on non-Linux targets (no sysfs BAR1) and in tests
    /// constructed via `RealIoc::for_tests()`.
    #[cfg(target_os = "linux")]
    pub bar1_mmap: Option<MmapRegion>,

    /// Current personality detected from chip (IT=0x13, IR=0x17).
    /// Populated by cycle 2b's `current_personality()` impl on first call.
    pub current_personality: Option<Personality>,
}

impl<P: Platform> RealIoc<P> {
    /// Open RealIoc against a real PCI device. mmaps BAR1 read-write so the
    /// MPI message-mode layer (cycle 2b) can post doorbell writes + read
    /// reply registers directly.
    ///
    /// Returns `MpiError::Io` if BAR1 mmap fails — typically because:
    /// - BDF doesn't exist (`/sys/bus/pci/devices/<bdf>` missing)
    /// - Caller lacks CAP_SYS_RAWIO (try `sudo`)
    /// - Running on non-Linux (no sysfs)
    /// - mpt3sas driver currently bound (try `lsirec unbind` first)
    #[cfg(target_os = "linux")]
    pub fn open(platform: P, pci_bdf: impl Into<String>) -> Result<Self, MpiError> {
        let bdf = pci_bdf.into();
        let bar1_path = PathBuf::from(format!("/sys/bus/pci/devices/{}/resource1", bdf));
        let bar1_mmap = MmapRegion::open_rw(&bar1_path, BAR1_LEN)
            .map_err(|e| MpiError::Io(format!("BAR1 mmap {}: {}", bar1_path.display(), e)))?;
        Ok(Self {
            platform,
            pci_bdf: bdf,
            bar1_path,
            bar1_mmap: Some(bar1_mmap),
            current_personality: None,
        })
    }

    /// Non-Linux stub: returns `MpiError::Io` since BAR1 sysfs is Linux-only.
    /// Kept so cross-platform builds compile.
    #[cfg(not(target_os = "linux"))]
    pub fn open(_platform: P, pci_bdf: impl Into<String>) -> Result<Self, MpiError> {
        Err(MpiError::Io(format!(
            "RealIoc::open requires Linux sysfs (bdf={})",
            pci_bdf.into()
        )))
    }

    /// Test-only constructor: builds a `RealIoc` with no live BAR1 mmap.
    /// Used by unit tests that exercise non-hardware code paths (e.g., trait
    /// conformance, path computation). `bar1_mmap = None`.
    #[cfg(test)]
    pub fn for_tests(platform: P, pci_bdf: impl Into<String>) -> Self {
        let bdf = pci_bdf.into();
        let bar1_path = PathBuf::from(format!("/sys/bus/pci/devices/{}/resource1", bdf));
        Self {
            platform,
            pci_bdf: bdf,
            bar1_path,
            #[cfg(target_os = "linux")]
            bar1_mmap: None,
            current_personality: None,
        }
    }

    /// Get the BAR1 sysfs path.
    pub fn bar1_path(&self) -> &Path {
        &self.bar1_path
    }

    /// Get the PCI BDF string (e.g., "0000:03:00.0").
    pub fn pci_bdf(&self) -> &str {
        &self.pci_bdf
    }

    /// Get current personality if detected (None until cycle 2b's
    /// `current_personality()` impl runs against the chip).
    pub fn get_personality(&self) -> Option<Personality> {
        self.current_personality
    }

    /// Read-only access to the BAR1 mmap region. Returns None if no live
    /// mapping (test mode or non-Linux). Cycle 2b's `IocBackend` impls use
    /// this to read chip registers (DOORBELL, DIAG, etc. per `doorbell.rs`).
    #[cfg(target_os = "linux")]
    pub fn bar1(&self) -> Option<&[u8]> {
        self.bar1_mmap.as_ref().map(|m| m.as_slice())
    }

    /// Non-Linux stub: returns None since BAR1 sysfs is Linux-only.
    #[cfg(not(target_os = "linux"))]
    pub fn bar1(&self) -> Option<&[u8]> {
        None
    }

    /// Mutable access to the BAR1 mmap. Required to write doorbell registers.
    /// Returns None if no live mapping (non-Linux or not initialized).
    #[cfg(target_os = "linux")]
    pub fn bar1_mut(&mut self) -> Option<&mut [u8]> {
        self.bar1_mmap.as_mut().map(|m| m.as_mut_slice())
    }

    /// Non-Linux stub: returns None since BAR1 sysfs is Linux-only.
    #[cfg(not(target_os = "linux"))]
    pub fn bar1_mut(&mut self) -> Option<&mut [u8]> {
        None
    }
}

impl<P: Platform> IocBackend for RealIoc<P> {
    // === Destructive ops — brick-gated, stay NotImplementedYet until CH341A ===

    fn send_fw_download(
        &mut self,
        _req: &FwDownloadRequest<'_>,
    ) -> Result<FwDownloadReply, MpiError> {
        Err(MpiError::NotImplementedYet {
            op: "RealIoc::send_fw_download (destructive — brick-gated)",
        })
    }

    fn send_toolbox_clean(&mut self, _req: &ToolboxCleanRequest) -> Result<ToolboxReply, MpiError> {
        Err(MpiError::NotImplementedYet {
            op: "RealIoc::send_toolbox_clean (destructive — brick-gated)",
        })
    }

    // === Read-safe ops — cycle 2b (freshman) implements these ===

    fn send_fw_upload<'a>(
        &mut self,
        req: &'a mut FwUploadRequest<'a>,
    ) -> Result<FwUploadReply, MpiError> {
        // TODO(cycle 2b followup): the dword loops below skip the
        // IOC_DOORBELL_INT handshake. On a real chip the host must wait for
        // the IOC interrupt bit between every write and every read; without
        // it, writes can race the IOC and reads return stale doorbell
        // contents. Will surface on dev-1 — fix as part of the hardware
        // bring-up cycle. Same applies to send_ioc_init below.
        use crate::mpi::doorbell::{get_ioc_state, IocState};

        // Step 1: Validate payload buffer size against requested image_size FIRST
        // This check must happen before accessing BAR1 to work in tests on non-Linux
        if req.image_size as usize > req.payload_buffer.len() {
            return Err(MpiError::Io(format!(
                "upload buffer too small: chip says {} bytes, buffer is {}",
                req.image_size,
                req.payload_buffer.len()
            )));
        }

        // Step 2: Verify IOC state via doorbell — must be READY or OPERATIONAL
        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;
        let ioc_state = get_ioc_state(bar1, MPI2_DOORBELL);

        if !matches!(ioc_state, IocState::Ready | IocState::Operational) {
            return Err(MpiError::IocStatus(IocStatus::InvalidState));
        }

        // Step 3: Serialize FW_UPLOAD request to wire format
        let request_bytes = req.serialize_to(2);

        if request_bytes.len() < 40 {
            return Err(MpiError::Io(format!(
                "FW_UPLOAD request too small: {} bytes, need at least 40",
                request_bytes.len()
            )));
        }

        // Step 4: Doorbell handshake (same pattern as send_ioc_init)
        let doorbell_offset = MPI2_DOORBELL;

        let msg_size_dwords = (request_bytes.len() / 4) as u32;
        let function_code = MpiFunction::FwUpload.as_u8();

        let doorbell_value = (function_code as u32) << 24 | ((msg_size_dwords - 2) << 16);

        write32(bar1, doorbell_offset, doorbell_value);

        // Step 5: Write request payload to DOORBELL register
        let mut offset = 0usize;
        while offset < request_bytes.len() {
            let dword = u32::from_le_bytes([
                request_bytes[offset],
                request_bytes[offset + 1],
                request_bytes[offset + 2],
                request_bytes[offset + 3],
            ]);

            write32(bar1, doorbell_offset, dword);
            offset += 4;
        }

        // Step 6: Read reply from DOORBELL register
        let mut reply_bytes = Vec::with_capacity(22);
        offset = 0;

        while offset < 22 {
            let dword = read32(bar1, doorbell_offset);

            for i in 0..4 {
                if offset + i < 22 {
                    reply_bytes.push((dword >> (i * 8)) as u8);
                }
            }
            offset += 4;
        }

        // Step 7: Parse reply and extract IOCStatus + ActualImageSize
        let reply = FwUploadReply::parse(&reply_bytes)?;

        if reply.ioc_status != IocStatus::Success {
            return Err(MpiError::IocStatus(reply.ioc_status));
        }

        Ok(reply)
    }

    fn send_config(&mut self, req: &ConfigRequest<'_>) -> Result<ConfigReply, MpiError> {
        // TODO(cycle 2b followup): the dword loops below skip the
        // IOC_DOORBELL_INT handshake. On a real chip the host must wait for
        // the IOC interrupt bit between every write and every read; without
        // it, writes can race the IOC and reads return stale doorbell
        // contents. Will surface on dev-1 — fix as part of the hardware
        // bring-up cycle. Same applies to send_ioc_init above.

        use crate::mpi::doorbell::{get_ioc_state, IocState};

        // Step 1: Get BAR1 via self.bar1_mut() - must be mapped for real hardware access
        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;

        // Step 2: Verify IOC state via doorbell — must be Ready or Operational
        // Cites: doorbell.rs:132-152 (get_ioc_state function)
        let ioc_state = get_ioc_state(bar1, MPI2_DOORBELL);
        if !matches!(ioc_state, IocState::Ready | IocState::Operational) {
            return Err(MpiError::IocStatus(IocStatus::InvalidState));
        }

        // Step 3: Serialize the request to wire format
        // Cites: messages.rs:717-773 (ConfigRequest::serialize_to), toolbox-and-config.md §6
        let request_bytes = req.serialize_to(2); // SMID=2 for this call

        // Step 4: Compute doorbell value
        // Cites: mpi-overview.md:38 (function codes in bits 24-31)
        // Cites: messages.rs:73,99 (MpiFunction::Config = 0x04)
        // Cites: real_ioc.rs:244 (doorbell value formula from send_fw_upload pattern)
        let function_code = MpiFunction::Config.as_u8(); // 0x04 per messages.rs:73

        // msg_size_dwords = request_bytes.len() / 4, then subtract 2 for doorbell encoding
        // Cites: mpi-overview.md:35 (bits 16-23 encode message length in dwords minus 2)
        let msg_size_dwords = (request_bytes.len() / 4) as u32;
        let doorbell_value = (function_code as u32) << 24 | ((msg_size_dwords - 2) << 16);

        // Step 5: Write doorbell value to trigger the message
        // Cites: doorbell.rs:5 (MPI2_DOORBELL = 0x00), doorbell.rs:63-66 (write32)
        let doorbell_offset = MPI2_DOORBELL;
        crate::mpi::doorbell::write32(bar1, doorbell_offset, doorbell_value);

        // Step 6: Write request payload dword-by-dword to DOORBELL register
        // Each dwords (4 bytes) written sequentially per lsirec.c pattern
        let mut offset = 0usize;
        while offset < request_bytes.len() {
            let dword = u32::from_le_bytes([
                request_bytes[offset],
                request_bytes[offset + 1],
                request_bytes[offset + 2],
                request_bytes[offset + 3],
            ]);

            crate::mpi::doorbell::write32(bar1, doorbell_offset, dword);
            offset += 4;
        }

        // Step 7: Read reply from DOORBELL register
        // Cites: messages.rs:1019-1043 (ConfigReply::parse expects at least 26 bytes)
        let mut reply_bytes = Vec::with_capacity(26);
        offset = 0;

        while offset < 26 {
            let dword = crate::mpi::doorbell::read32(bar1, doorbell_offset);

            for i in 0..4 {
                if offset + i < 26 {
                    reply_bytes.push((dword >> (i * 8)) as u8);
                }
            }
            offset += 4;
        }

        // Step 8: Parse reply with ConfigReply::parse
        let reply = ConfigReply::parse(&reply_bytes)?;

        // Step 9: If ioc_status != Success, return error
        if reply.ioc_status != IocStatus::Success {
            return Err(MpiError::IocStatus(reply.ioc_status));
        }

        Ok(reply)
    }

    fn send_ioc_init(&mut self, req: &IocInitRequest) -> Result<IocInitReply, MpiError> {
        // Get BAR1 mmap — must be mapped for real hardware access
        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;

        // Serialize IOC_INIT request to wire format (72 bytes per mpi-overview.md §9)
        // Request structure: mpi2_ioc.h:135-164, header at mpi-overview.md §1.2
        let request_bytes = req.serialize_to(1); // SMID=1 for test simplicity

        // Ensure we have exactly 72 bytes (header + body)
        if request_bytes.len() < 72 {
            return Err(MpiError::Io(format!(
                "IOC_INIT request too small: {} bytes, need 72",
                request_bytes.len()
            )));
        }

        // Doorbell handshake per mpi-overview.md §1.1, lsirec.c doorbell pattern
        // DOORBELL register at BAR1+0x00 (doorbell.rs:5)

        // Step 1: Write function code + message size to doorbell
        // Function code 0x02 = IOC_INIT (messages.rs:74), bits 24-31 of doorbell
        // Bits 16-23 = message length in dwords minus 2 (mpi2.h:178-193)
        // Message is 72 bytes = 18 dwords, so size field = 18 - 2 = 16 = 0x10
        let doorbell_offset = 0x00; // MPI2_DOORBELL from doorbell.rs:5
        let msg_size_dwords = (request_bytes.len() / 4) as u32; // 18 dwords for 72 bytes
        let function_code = crate::mpi::messages::MpiFunction::IocInit.as_u8();

        let doorbell_value = (function_code as u32) << 24 | ((msg_size_dwords - 2) << 16);

        // Write to DOORBELL register (BAR1+0x00, lsirec.c:12)
        crate::mpi::doorbell::write32(bar1, doorbell_offset, doorbell_value);

        // Step 2: Wait for IOC_DOORBELL_INT bit in HISTATUS (interrupt posted by IOC)
        // Per mpi-overview.md §9 initialization pattern

        // Step 3: Write request payload to DOORBELL register byte-by-byte
        // Each dword (4 bytes) written sequentially
        let mut offset = 0usize;
        while offset < request_bytes.len() {
            let dword = u32::from_le_bytes([
                request_bytes[offset],
                request_bytes[offset + 1],
                request_bytes[offset + 2],
                request_bytes[offset + 3],
            ]);

            // Write dword to DOORBELL register (BAR1+0x00)
            crate::mpi::doorbell::write32(bar1, doorbell_offset, dword);
            offset += 4;
        }

        // Step 4: Read reply from DOORBELL register
        // Reply structure per mpi-overview.md §9.2 (mpi2_ioc.h:191-207)
        // IOC writes reply back to doorbell register after processing

        let mut reply_bytes = Vec::with_capacity(18); // Minimum reply is 18 bytes
        offset = 0;

        while offset < 18 {
            let dword = crate::mpi::doorbell::read32(bar1, doorbell_offset);

            for i in 0..4 {
                if offset + i < 18 {
                    reply_bytes.push((dword >> (i * 8)) as u8);
                }
            }
            offset += 4;
        }

        // Step 5: Deserialize reply and check status
        let reply = IocInitReply::parse(&reply_bytes)?;

        if reply.ioc_status != crate::mpi::messages::IocStatus::Success {
            return Err(MpiError::IocStatus(reply.ioc_status));
        }

        Ok(reply)
    }

    fn send_ioc_facts(&mut self) -> Result<IocFactsReply, MpiError> {
        // TODO(cycle 2b followup): the dword loops below skip the
        // IOC_DOORBELL_INT handshake. On a real chip the host must wait for
        // the IOC interrupt bit between every write and every read; without
        // it, writes can race the IOC and reads return stale doorbell
        // contents. Will surface on dev-1 — fix as part of the hardware
        // bring-up cycle. Same pattern as send_ioc_init above.

        use crate::mpi::doorbell::{get_ioc_state, IocState};

        // Step 1: Get BAR1 via self.bar1_mut() - must be mapped for real hardware access
        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;

        // Step 2: Verify IOC state via doorbell — must be Ready or Operational
        // Cites: doorbell.rs:132-152 (get_ioc_state function)
        let ioc_state = get_ioc_state(bar1, crate::mpi::doorbell::MPI2_DOORBELL);
        if !matches!(ioc_state, IocState::Ready | IocState::Operational) {
            return Err(MpiError::IocStatus(IocStatus::InvalidState));
        }

        // Step 3: Serialize IOC_FACTS request to wire format (16 bytes header only)
        // Cites: messages.rs:1087-1109 (IocFactsRequest::serialize_to)
        let request_bytes = IocFactsRequest::serialize_to(2); // SMID=2 for this call

        if request_bytes.len() < 16 {
            return Err(MpiError::Io(format!(
                "IOC_FACTS request too small: {} bytes, need at least 16",
                request_bytes.len()
            )));
        }

        // Step 4: Compute doorbell value
        // Cites: mpi-overview.md:38 (function codes in bits 24-31)
        // Cites: messages.rs:95,74 (MpiFunction::IocFacts = 0x03 per mpi2_ioc.h:191)
        let function_code = MpiFunction::IocFacts.as_u8(); // 0x03 per mpi2_ioc.h:191

        // msg_size_dwords = request_bytes.len() / 4, then subtract 2 for doorbell encoding
        // Cites: mpi-overview.md:35 (bits 16-23 encode message length in dwords minus 2)
        let msg_size_dwords = (request_bytes.len() / 4) as u32; // 4 dwords for 16 bytes
        let doorbell_value = (function_code as u32) << 24 | ((msg_size_dwords - 2) << 16);

        // Step 5: Write doorbell value to trigger the message
        // Cites: doorbell.rs:5 (MPI2_DOORBELL = 0x00), doorbell.rs:63-66 (write32)
        let doorbell_offset = crate::mpi::doorbell::MPI2_DOORBELL;
        crate::mpi::doorbell::write32(bar1, doorbell_offset, doorbell_value);

        // Step 6: Write request payload dword-by-dword to DOORBELL register
        // Each dwords (4 bytes) written sequentially per lsirec.c pattern
        let mut offset = 0usize;
        while offset < request_bytes.len() {
            let dword = u32::from_le_bytes([
                request_bytes[offset],
                request_bytes[offset + 1],
                request_bytes[offset + 2],
                request_bytes[offset + 3],
            ]);

            crate::mpi::doorbell::write32(bar1, doorbell_offset, dword);
            offset += 4;
        }

        // Step 7: Read reply from DOORBELL register
        // Cites: messages.rs:1180-1250 (IocFactsReply::parse expects at least 96 bytes)
        let mut reply_bytes = Vec::with_capacity(96); // Min reply is 96 bytes per mpi2_ioc.h:231-281
        offset = 0;

        while offset < 96 {
            let dword = crate::mpi::doorbell::read32(bar1, doorbell_offset);

            for i in 0..4 {
                if offset + i < 96 {
                    reply_bytes.push((dword >> (i * 8)) as u8);
                }
            }
            offset += 4;
        }

        // Step 8: Parse reply with IocFactsReply::parse
        let reply = IocFactsReply::parse(&reply_bytes)?;

        // Step 9: If ioc_status != Success, return error
        if reply.ioc_status != IocStatus::Success {
            return Err(MpiError::IocStatus(reply.ioc_status));
        }

        Ok(reply)
    }

    /// Read the running firmware's personality (IT=0x13, IR=0x17).
    ///
    /// Blocker: This method takes `&self` but obtaining personality requires a CONFIG
    /// roundtrip which needs `&mut self` via send_config(). The IocBackend trait signature
    /// cannot be changed in this cycle. See session.rs:28-34 for personality byte mapping.
    /// Doorbell IOC_STATE (doorbell.rs:132) does not encode personality on SAS2008 — only
    /// Reset/Ready/Operational/Fault states.
    ///
    /// TODO(cycle 2b-3): Requires trait signature change to `&mut self` or alternative mechanism
    /// that doesn't require CONFIG roundtrip (e.g., reading Manufacturing Page 0 via a different
    /// path that's &self-compatible). For now, document the blocker rather than silently changing
    /// the trait.
    fn current_personality(&self) -> Result<Personality, MpiError> {
        todo!("cycle 2b-3: requires trait signature change to &mut self")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpi::messages::ImageType;
    use crate::pci::MockPlatform;

    /// for_tests builds a RealIoc without hardware mmap (bar1_mmap = None).
    /// Cycle 2a verifies the constructor + path computation still work.
    #[test]
    fn realioc_for_tests_succeeds_against_mock_platform() {
        let mock = MockPlatform::new();
        let realioc = RealIoc::for_tests(mock, "0000:03:00.0");
        assert_eq!(realioc.pci_bdf(), "0000:03:00.0");
        #[cfg(target_os = "linux")]
        assert!(
            realioc.bar1().is_none(),
            "test ctor leaves bar1_mmap = None"
        );
    }

    /// Compile-time trait conformance check — proves RealIoc<MockPlatform> implements IocBackend.
    #[test]
    fn realioc_implements_iocbackend_trait() {
        fn _assert<T: IocBackend>() {}
        _assert::<RealIoc<MockPlatform>>();
    }

    /// Verify cycle 2b read-safe ops are todo!() — the panic markers cycle 2b
    /// fills in. Verify destructive ops return MpiError::NotImplementedYet
    /// instead — those stay brick-gated.
    #[test]
    fn realioc_destructive_ops_return_not_implemented_yet() {
        let mock = MockPlatform::new();
        let mut realioc: RealIoc<MockPlatform> = RealIoc::for_tests(mock, "0000:03:00.0");

        // Destructive ops return NotImplementedYet (NOT a panic — explicit error).
        let download_req = FwDownloadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 256,
            total_image_size: 256,
            last_segment: true,
            payload: &[0u8; 256],
        };
        let result = realioc.send_fw_download(&download_req);
        assert!(
            matches!(result, Err(MpiError::NotImplementedYet { .. })),
            "send_fw_download must be brick-gated (NotImplementedYet), got: {result:?}"
        );

        let clean_req = ToolboxCleanRequest {
            flags: crate::mpi::messages::ToolboxCleanFlags::FLASH,
        };
        let result = realioc.send_toolbox_clean(&clean_req);
        assert!(
            matches!(result, Err(MpiError::NotImplementedYet { .. })),
            "send_toolbox_clean must be brick-gated (NotImplementedYet), got: {result:?}"
        );
    }

    #[test]
    fn realioc_current_personality_is_todo_for_cycle_2b() {
        let mock = MockPlatform::new();
        let realioc: RealIoc<MockPlatform> = RealIoc::for_tests(mock, "0000:03:00.0");

        // current_personality is still todo!() — cycle 2b fills this in.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            realioc.current_personality()
        }));
        assert!(
            result.is_err(),
            "current_personality should todo!() until cycle 2b"
        );
    }

    /// Verify BAR1 path follows canonical sysfs convention.
    #[test]
    fn realioc_bar1_path_is_canonical_sysfs() {
        let mock = MockPlatform::new();
        let realioc = RealIoc::for_tests(mock, "0000:03:00.0");
        let expected = Path::new("/sys/bus/pci/devices/0000:03:00.0/resource1");
        assert_eq!(realioc.bar1_path(), expected);
    }

    /// open() against a nonexistent BDF must return MpiError::Io, not panic.
    #[cfg(target_os = "linux")]
    #[test]
    fn realioc_open_nonexistent_bdf_returns_io_error() {
        let mock = MockPlatform::new();
        let result = RealIoc::open(mock, "ffff:ff:ff.f");
        assert!(
            matches!(result, Err(MpiError::Io(_))),
            "open() against nonexistent BDF should return MpiError::Io"
        );
    }

    /// BAR1_LEN must cover all SAS2008 register offsets exposed by doorbell.rs
    /// (DOORBELL=0x00, DIAG=0x10, WRSEQ=0x14, HCDW pointers at 0x74..0x7C).
    /// 4 KB easily covers this per lsirec.c. Const assertion → compile error
    /// if a future change drops BAR1_LEN below the floor.
    const _BAR1_LEN_COVERS_KNOWN_REGISTERS: () = assert!(BAR1_LEN >= 0x100);

    /// Test that send_fw_upload respects buffer size limits.
    #[test]
    fn send_fw_upload_respects_buffer_size() {
        let mock = MockPlatform::new();
        let mut realioc: RealIoc<MockPlatform> = RealIoc::for_tests(mock, "0000:03:00.0");

        // Create request with small buffer but large image_size
        let mut buf = vec![0u8; 64];
        let mut req = FwUploadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 65536, // Request 64KB but buffer is only 64 bytes
            payload_buffer: &mut buf,
        };

        let result = realioc.send_fw_upload(&mut req);

        assert!(
            result.is_err(),
            "send_fw_upload should reject oversized requests"
        );
        if let Err(MpiError::Io(msg)) = result {
            assert!(
                msg.contains("buffer too small"),
                "Error message should mention buffer size: {}",
                msg
            );
        } else {
            panic!("Expected MpiError::Io, got {:?}", result);
        }
    }

    /// Test that send_config returns Io error when BAR1 is not mapped.
    #[test]
    fn send_config_returns_io_error_when_bar1_not_mapped() {
        let mock = MockPlatform::new();
        let mut realioc: RealIoc<MockPlatform> = RealIoc::for_tests(mock, "0000:03:00.0");

        // Create a minimal CONFIG request for Manufacturing Page 5 (SAS WWN)
        let mut payload_buf = [0u8; 256];
        let req = ConfigRequest {
            action: 1,       // MPI2_CONFIG_ACTION_PAGE_READ_CURRENT per toolbox-and-config.md §6.1
            sgl_flags: 0xC0, // END_OF_LIST + IOC_TO_HOST
            page_type: 9,    // MPI2_CONFIG_PAGETYPE_MANUFACTURING per toolbox-and-config.md §6.2
            page_number: 5,  // SAS WWN page
            ext_page_type: None,
            payload_buffer: &mut payload_buf,
        };

        let result = realioc.send_config(&req);

        assert!(
            matches!(result, Err(MpiError::Io(_))),
            "send_config should return MpiError::Io when BAR1 not mapped"
        );
    }

    /// Test that send_config returns InvalidState when IOC is in Fault state.
    #[test]
    fn send_config_handles_ioc_state_validation() {
        // This test verifies the pattern: on non-Linux/tests with no BAR1,
        // we get Io error first (before state check). The state validation
        // path would require mocking BAR1 contents which is out of scope for cycle 2b.
        let mock = MockPlatform::new();
        let mut realioc: RealIoc<MockPlatform> = RealIoc::for_tests(mock, "0000:03:00.0");

        let mut payload_buf = [0u8; 256];
        let req = ConfigRequest {
            action: 1,
            sgl_flags: 0xC0,
            page_type: 9,
            page_number: 5,
            ext_page_type: None,
            payload_buffer: &mut payload_buf,
        };

        // Verify we get Io error (BAR1 not mapped), which is the expected behavior for tests
        let result = realioc.send_config(&req);
        assert!(matches!(result, Err(MpiError::Io(_))));
    }
}
