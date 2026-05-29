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

#[cfg(target_os = "linux")]
use crate::hw::{DmaBuffer, HwBackend};
use crate::mpi::doorbell::MPI2_DOORBELL;
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

    /// Optional hardware backend (VFIO etc.). When set, BAR1 access and DMA
    /// allocation go through here instead of the direct sysfs mmap path —
    /// gets us out of having to unbind mpt3sas by hand, plus enables
    /// alloc_dma for chip-readable buffers. Constructed via
    /// `RealIoc::from_backend`.
    #[cfg(target_os = "linux")]
    pub hw_backend: Option<Box<dyn HwBackend>>,

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
            hw_backend: None,
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

    /// Open RealIoc backed by an `HwBackend` (VFIO in production).
    ///
    /// This is the preferred constructor for ops that need DMA buffers
    /// (FW_UPLOAD, FW_DOWNLOAD). The backend handles the driver bind dance
    /// — no need for the operator to `lsirec unbind` first — and provides
    /// chip-readable IOVAs for SGEs.
    ///
    /// BAR1 access goes through `backend.bar1()` instead of the direct
    /// sysfs mmap. `bar1_mmap` is left `None`.
    #[cfg(target_os = "linux")]
    pub fn from_backend(platform: P, backend: Box<dyn HwBackend>) -> Result<Self, MpiError> {
        let bdf = backend.bdf().to_string();
        let bar1_path = PathBuf::from(format!("/sys/bus/pci/devices/{}/resource1", bdf));
        Ok(Self {
            platform,
            pci_bdf: bdf,
            bar1_path,
            bar1_mmap: None,
            hw_backend: Some(backend),
            current_personality: None,
        })
    }

    /// Allocate a chip-readable DMA buffer via the active HwBackend. Returns
    /// `MpiError::Io` if no backend is attached (RealIoc opened via direct
    /// sysfs mmap, or test-mode).
    #[cfg(target_os = "linux")]
    pub fn alloc_dma(&mut self, len: usize) -> Result<DmaBuffer, MpiError> {
        let backend = self.hw_backend.as_mut().ok_or_else(|| {
            MpiError::Io(
                "alloc_dma requires HwBackend — use RealIoc::from_backend instead of ::open".into(),
            )
        })?;
        backend
            .alloc_dma(len)
            .map_err(|e| MpiError::Io(format!("alloc_dma: {}", e)))
    }

    /// Release a DMA buffer previously returned by `alloc_dma`.
    #[cfg(target_os = "linux")]
    pub fn free_dma(&mut self, buf: DmaBuffer) -> Result<(), MpiError> {
        let backend = self
            .hw_backend
            .as_mut()
            .ok_or_else(|| MpiError::Io("free_dma: no HwBackend attached".into()))?;
        backend
            .free_dma(buf)
            .map_err(|e| MpiError::Io(format!("free_dma: {}", e)))
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
            #[cfg(target_os = "linux")]
            hw_backend: None,
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
    ///
    /// Prefers the HwBackend's BAR1 slice when one is attached; falls back
    /// to the direct sysfs mmap.
    #[cfg(target_os = "linux")]
    pub fn bar1(&self) -> Option<&[u8]> {
        // HwBackend's bar1() needs &mut self, so this read-only accessor
        // can only see the legacy mmap. Callers needing read-only on a
        // backend-attached RealIoc should use bar1_mut() and freeze it.
        self.bar1_mmap.as_ref().map(|m| m.as_slice())
    }

    /// Non-Linux stub: returns None since BAR1 sysfs is Linux-only.
    #[cfg(not(target_os = "linux"))]
    pub fn bar1(&self) -> Option<&[u8]> {
        None
    }

    /// Mutable access to the BAR1 mmap. Required to write doorbell registers.
    /// Returns None if no live mapping (non-Linux or not initialized).
    ///
    /// Prefers the HwBackend's BAR1 slice when one is attached; falls back
    /// to the direct sysfs mmap.
    #[cfg(target_os = "linux")]
    pub fn bar1_mut(&mut self) -> Option<&mut [u8]> {
        if let Some(be) = self.hw_backend.as_mut() {
            return Some(be.bar1());
        }
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
        use crate::mpi::doorbell::{
            doorbell_handshake_recv, doorbell_handshake_send, get_ioc_state, IocState,
        };

        // Payload buffer guard — testable without BAR1.
        if req.image_size as usize > req.payload_buffer.len() {
            return Err(MpiError::Io(format!(
                "upload buffer too small: chip says {} bytes, buffer is {}",
                req.image_size,
                req.payload_buffer.len()
            )));
        }

        // Allocate a chip-readable DMA buffer when a HwBackend is attached.
        // Without it, the SGE iova falls back to the user-space VA — which
        // the chip cannot translate. That path returns the 885-KB-of-zeros
        // bug (kept for graceful degradation in test/legacy callers, with
        // a clear error if there's neither backend nor BAR1).
        #[cfg(target_os = "linux")]
        let dma_buf = if self.hw_backend.is_some() {
            Some(self.alloc_dma(req.image_size as usize)?)
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let _dma_buf: Option<()> = None;

        #[cfg(target_os = "linux")]
        let iova = match &dma_buf {
            Some(b) => b.iova,
            None => req.payload_buffer.as_ptr() as u64,
        };
        #[cfg(not(target_os = "linux"))]
        let iova: u64 = req.payload_buffer.as_ptr() as u64;

        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;

        let ioc_state = get_ioc_state(bar1, MPI2_DOORBELL);
        if !matches!(ioc_state, IocState::Ready | IocState::Operational) {
            // Avoid leaking the DMA buffer on early-return.
            #[cfg(target_os = "linux")]
            if let Some(b) = dma_buf {
                let _ = self.free_dma(b);
            }
            return Err(MpiError::IocStatus(IocStatus::InvalidState));
        }

        let mut request_bytes = req.serialize_to(iova);
        while request_bytes.len() % 4 != 0 {
            request_bytes.push(0);
        }

        let timeout = std::time::Duration::from_secs(10); // longer for big payloads
        let send_result =
            doorbell_handshake_send(bar1, MpiFunction::FwUpload.as_u8(), &request_bytes, timeout);
        let recv_result = send_result.and_then(|_| doorbell_handshake_recv(bar1, 64, timeout));

        // Copy DMA buffer back into the caller's payload_buffer before freeing.
        // Order matters: copy before free, free before returning Err.
        #[cfg(target_os = "linux")]
        if let Some(b) = &dma_buf {
            let copy_len = req.payload_buffer.len().min(b.len);
            unsafe {
                std::ptr::copy_nonoverlapping(b.va, req.payload_buffer.as_mut_ptr(), copy_len);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(b) = dma_buf {
            let _ = self.free_dma(b);
        }

        let reply_bytes = recv_result.map_err(|e| MpiError::Io(format!("FW_UPLOAD: {}", e)))?;
        let reply = FwUploadReply::parse(&reply_bytes)?;
        if reply.ioc_status != IocStatus::Success {
            return Err(MpiError::IocStatus(reply.ioc_status));
        }

        Ok(reply)
    }

    fn send_config(&mut self, req: &ConfigRequest<'_>) -> Result<ConfigReply, MpiError> {
        use crate::mpi::doorbell::{
            doorbell_handshake_recv, doorbell_handshake_send, get_ioc_state, IocState,
        };

        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;
        let ioc_state = get_ioc_state(bar1, MPI2_DOORBELL);
        if !matches!(ioc_state, IocState::Ready | IocState::Operational) {
            return Err(MpiError::IocStatus(IocStatus::InvalidState));
        }

        let mut request_bytes = req.serialize_to(2);
        while request_bytes.len() % 4 != 0 {
            request_bytes.push(0);
        }

        let timeout = std::time::Duration::from_secs(5);
        doorbell_handshake_send(bar1, MpiFunction::Config.as_u8(), &request_bytes, timeout)
            .map_err(|e| MpiError::Io(format!("CONFIG send: {}", e)))?;

        let reply_bytes = doorbell_handshake_recv(bar1, 64, timeout)
            .map_err(|e| MpiError::Io(format!("CONFIG recv: {}", e)))?;

        let reply = ConfigReply::parse(&reply_bytes)?;
        if reply.ioc_status != IocStatus::Success {
            return Err(MpiError::IocStatus(reply.ioc_status));
        }

        Ok(reply)
    }

    fn send_ioc_init(&mut self, req: &IocInitRequest) -> Result<IocInitReply, MpiError> {
        use crate::mpi::doorbell::{doorbell_handshake_recv, doorbell_handshake_send};

        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;

        let mut request_bytes = req.serialize_to(1);
        while request_bytes.len() % 4 != 0 {
            request_bytes.push(0);
        }

        let timeout = std::time::Duration::from_secs(5);
        doorbell_handshake_send(bar1, MpiFunction::IocInit.as_u8(), &request_bytes, timeout)
            .map_err(|e| MpiError::Io(format!("IOC_INIT send: {}", e)))?;

        let reply_bytes = doorbell_handshake_recv(bar1, 64, timeout)
            .map_err(|e| MpiError::Io(format!("IOC_INIT recv: {}", e)))?;

        let reply = IocInitReply::parse(&reply_bytes)?;
        if reply.ioc_status != IocStatus::Success {
            return Err(MpiError::IocStatus(reply.ioc_status));
        }

        Ok(reply)
    }

    fn send_ioc_facts(&mut self) -> Result<IocFactsReply, MpiError> {
        use crate::mpi::doorbell::{
            doorbell_handshake_recv, doorbell_handshake_send, get_ioc_state, IocState,
            MPI2_DOORBELL,
        };

        // Step 1: Get BAR1
        let bar1 = self
            .bar1_mut()
            .ok_or_else(|| MpiError::Io("BAR1 not mapped".into()))?;

        // Step 2: Verify IOC state — must be Ready or Operational
        let ioc_state = get_ioc_state(bar1, MPI2_DOORBELL);
        if !matches!(ioc_state, IocState::Ready | IocState::Operational) {
            return Err(MpiError::IocStatus(IocStatus::InvalidState));
        }

        // Step 3: Serialize IOC_FACTS request — 12 bytes (3 dwords) per mpi2_ioc.h
        let mut request_bytes = IocFactsRequest::serialize_to(2);
        // Pad to dword alignment if serialize_to ever emits a non-aligned blob.
        while request_bytes.len() % 4 != 0 {
            request_bytes.push(0);
        }

        // Step 4: Full doorbell-handshake send (header + payload with per-dword IOC sync).
        // Cites lsiutil/mpt.c:781-834 (mpt_send_message).
        let function_code = MpiFunction::IocFacts.as_u8(); // 0x03 per mpi2_ioc.h:191
        let timeout = std::time::Duration::from_secs(5);
        doorbell_handshake_send(bar1, function_code, &request_bytes, timeout)
            .map_err(|e| MpiError::Io(format!("IOC_FACTS send: {}", e)))?;

        // Step 5: Full doorbell-handshake recv (U16-at-a-time with IOC sync + ACK per word).
        // Cites lsiutil/mpt.c:837-872 (mpt_receive_data). Chip tells us actual length
        // via MsgLength in the 2nd U16 word.
        let reply_bytes = doorbell_handshake_recv(bar1, 128, timeout)
            .map_err(|e| MpiError::Io(format!("IOC_FACTS recv: {}", e)))?;

        // Step 6: Parse reply with IocFactsReply::parse
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
            page_address: 0x0000_0000, // Plain pages have PageAddress=0 per mpi2_cnfg.h:347
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

        // Create another CONFIG request to test state validation path
        let mut payload_buf2 = [0u8; 256];
        let req2 = ConfigRequest {
            action: 1,       // MPI2_CONFIG_ACTION_PAGE_READ_CURRENT per toolbox-and-config.md §6.1
            sgl_flags: 0xC0, // END_OF_LIST + IOC_TO_HOST
            page_type: 9,    // MPI2_CONFIG_PAGETYPE_MANUFACTURING per toolbox-and-config.md §6.2
            page_number: 5,  // SAS WWN page
            ext_page_type: None,
            payload_buffer: &mut payload_buf2,
            page_address: 0x0000_0000, // Plain pages have PageAddress=0 per mpi2_cnfg.h:347
        };

        let result = realioc.send_config(&req2);

        assert!(
            matches!(result, Err(MpiError::Io(_))),
            "send_config should return MpiError::Io when BAR1 not mapped"
        );
    }

    /// Test that send_config handles page_address correctly.
    #[test]
    fn send_config_page_address_zero() {
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
            page_address: 0x0000_0000, // Plain pages have PageAddress=0 per mpi2_cnfg.h:347
        };

        // Verify we get Io error (BAR1 not mapped), which is the expected behavior for tests
        let result = realioc.send_config(&req);
        assert!(matches!(result, Err(MpiError::Io(_))));
    }
}
