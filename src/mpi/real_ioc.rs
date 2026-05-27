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

use std::path::{Path, PathBuf};

use crate::mpi::messages::{
    ConfigReply, FwDownloadReply, FwUploadReply, IocInitReply, ToolboxReply,
};
use crate::mpi::messages::{
    ConfigRequest, FwDownloadRequest, FwUploadRequest, IocInitRequest, ToolboxCleanRequest,
};
use crate::mpi::messages::MpiError;
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

    /// Mutable access to the BAR1 mmap. Required to write doorbell registers.
    /// Returns None if no live mapping. Cycle 2b's `send_*` methods use this.
    #[cfg(target_os = "linux")]
    pub fn bar1_mut(&mut self) -> Option<&mut [u8]> {
        self.bar1_mmap.as_mut().map(|m| m.as_mut_slice())
    }
}

impl<P: Platform> IocBackend for RealIoc<P> {
    // === Destructive ops — brick-gated, stay NotImplementedYet until CH341A ===

    fn send_fw_download<'a>(
        &mut self,
        _req: &FwDownloadRequest<'a>,
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
        _req: &'a mut FwUploadRequest<'a>,
    ) -> Result<FwUploadReply, MpiError> {
        todo!("cycle 2b: FW_UPLOAD via doorbell handshake + reply-queue (uses self.bar1_mut())")
    }

    fn send_config<'a>(&mut self, _req: &ConfigRequest<'a>) -> Result<ConfigReply, MpiError> {
        todo!("cycle 2b: CONFIG read-page via doorbell handshake (uses self.bar1_mut())")
    }

    fn send_ioc_init(&mut self, _req: &IocInitRequest) -> Result<IocInitReply, MpiError> {
        todo!("cycle 2b: IOC_INIT handshake — sets up reply queue (uses self.bar1_mut())")
    }

    fn current_personality(&self) -> Result<Personality, MpiError> {
        todo!("cycle 2b: read firmware version via doorbell handshake, decode personality byte")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpi::messages::ImageType;
    use crate::mpi::session::{IocBackend as _, Personality};
    use crate::pci::MockPlatform;

    /// for_tests builds a RealIoc without hardware mmap (bar1_mmap = None).
    /// Cycle 2a verifies the constructor + path computation still work.
    #[test]
    fn realioc_for_tests_succeeds_against_mock_platform() {
        let mock = MockPlatform::new();
        let realioc = RealIoc::for_tests(mock, "0000:03:00.0");
        assert_eq!(realioc.pci_bdf(), "0000:03:00.0");
        #[cfg(target_os = "linux")]
        assert!(realioc.bar1().is_none(), "test ctor leaves bar1_mmap = None");
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
        assert!(matches!(result, Err(MpiError::NotImplementedYet { .. })),
            "send_fw_download must be brick-gated (NotImplementedYet), got: {result:?}");

        let clean_req = ToolboxCleanRequest {
            flags: crate::mpi::messages::ToolboxCleanFlags::FLASH,
        };
        let result = realioc.send_toolbox_clean(&clean_req);
        assert!(matches!(result, Err(MpiError::NotImplementedYet { .. })),
            "send_toolbox_clean must be brick-gated (NotImplementedYet), got: {result:?}");
    }

    #[test]
    fn realioc_read_safe_ops_are_todo_for_cycle_2b() {
        let mock = MockPlatform::new();
        let mut realioc: RealIoc<MockPlatform> = RealIoc::for_tests(mock, "0000:03:00.0");

        // Read-safe ops are todo!() — cycle 2b fills these in.
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            realioc.send_ioc_init(&init_req)
        }));
        assert!(result.is_err(), "send_ioc_init should todo!() until cycle 2b");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            realioc.current_personality()
        }));
        assert!(result.is_err(), "current_personality should todo!() until cycle 2b");
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
        assert!(matches!(result, Err(MpiError::Io(_))),
            "open() against nonexistent BDF should return MpiError::Io, got: {result:?}");
    }

    /// BAR1_LEN must be at least large enough to cover all SAS2008 register
    /// offsets exposed by doorbell.rs (DOORBELL=0x00, DIAG=0x10, WRSEQ=0x14,
    /// HCDW pointers at 0x74..0x7C). 4 KB easily covers this per lsirec.c.
    #[test]
    fn bar1_len_covers_all_known_register_offsets() {
        assert!(BAR1_LEN >= 0x100, "BAR1_LEN must cover at least the first 256 bytes");
    }
}
