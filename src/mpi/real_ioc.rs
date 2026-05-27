//! Real MPI backend for production hardware access.
//!
//! # Cycle 1 / Cycle 2 Split
//!
//! **Cycle 1 (this file):** Scaffolding only. All `IocBackend` trait method bodies are
//! `todo!()` placeholders. This struct stores the necessary fields and paths so senior's
//! cycle 2 code can wire up real MPI message-mode operations via BAR1 mmap + doorbell + reply queue.
//!
//! **Cycle 2 (senior):** Senior will replace each `todo!()` body with actual hardware-touching code:
//! - Real BAR1 mmap via `/sys/bus/pci/devices/<bdf>/resource1`
//! - MPI message serialization and doorbell posting
//! - Reply queue polling and deserialization
//! - Personality detection from chip registers
//!
//! **DO NOT ADD HARDWARE CODE HERE.** This is provably non-destructive scaffolding.

use std::path::{Path, PathBuf};

use crate::mpi::messages::{
    ConfigReply, FwDownloadReply, FwUploadReply, IocInitReply, ToolboxReply,
};
use crate::mpi::messages::{
    ConfigRequest, FwDownloadRequest, FwUploadRequest, IocInitRequest, ToolboxCleanRequest,
};
use crate::mpi::messages::MpiError;
use crate::pci::Platform;
use crate::mpi::session::{IocBackend, Personality};

/// Real MPI backend for talking to actual SAS2008 hardware.
///
/// Stores PCI device paths and Platform abstraction for cycle 1 scaffolding.
/// Senior cycle 2 will use these fields to mmap BAR1 and post MPI messages via doorbell.
#[derive(Debug)]
pub struct RealIoc<P: Platform> {
    /// Platform trait impl — LinuxSysfs in production, MockPlatform in tests.
    /// Used for sysfs file reads (e.g., /sys/bus/pci/devices/<bdf>/resource1) and mmap.
    pub platform: P,

    /// PCI bus/device/function identifier, e.g., "0000:03:00.0"
    pub pci_bdf: String,

    /// Path to BAR1 resource file in sysfs: /sys/bus/pci/devices/<bdf>/resource1
    /// Senior cycle 2 will mmap this path for MPI message-mode register access.
    pub bar1_path: PathBuf,

    /// Current personality detected from chip (IT=0x13, IR=0x17).
    /// Populated by open() via chip query in senior's cycle 2 implementation.
    /// None until queried — requires real hardware access to detect.
    pub current_personality: Option<Personality>,

    // OPEN: senior cycle 2 confirms additional fields needed for doorbell address,
    // reply queue base address, BAR1 mapping handle, etc.
}

impl<P: Platform> RealIoc<P> {
    /// Create a new RealIoc instance without touching hardware.
    ///
    /// Stores the platform abstraction and BDF, computes bar1_path from sysfs convention.
    /// Sets current_personality = None — senior cycle 2 will populate this via chip probe.
    ///
    /// This constructor is CALLABLE in unit tests against MockPlatform for scaffolding verification.
    /// Senior cycle 2 may extend this to actually probe the chip and set initial personality.
    pub fn open(platform: P, pci_bdf: impl Into<String>) -> Result<Self, MpiError> {
        let bdf = pci_bdf.into();
        let bar1_path = PathBuf::from(format!("/sys/bus/pci/devices/{}/resource1", bdf));

        Ok(Self {
            platform,
            pci_bdf: bdf.clone(),
            bar1_path,
            current_personality: None,
        })
    }

    /// Get the BAR1 sysfs path that senior cycle 2 will mmap for register access.
    pub fn bar1_path(&self) -> &Path {
        &self.bar1_path
    }

    /// Get the PCI BDF string (e.g., "0000:03:00.0").
    pub fn pci_bdf(&self) -> &str {
        &self.pci_bdf
    }

    /// Get current personality if detected (None until chip probed in cycle 2).
    pub fn get_personality(&self) -> Option<Personality> {
        self.current_personality
    }
}

impl<P: Platform> IocBackend for RealIoc<P> {
    fn send_fw_download<'a>(
        &mut self,
        _req: &FwDownloadRequest<'a>,
    ) -> Result<FwDownloadReply, MpiError> {
        todo!("senior cycle 2: real MPI FwDownload via doorbell/reply-queue")
    }

    fn send_fw_upload<'a>(
        &mut self,
        _req: &'a mut FwUploadRequest<'a>,
    ) -> Result<FwUploadReply, MpiError> {
        todo!("senior cycle 2: real MPI FwUpload via doorbell/reply-queue")
    }

    fn send_toolbox_clean(&mut self, _req: &ToolboxCleanRequest) -> Result<ToolboxReply, MpiError> {
        todo!("senior cycle 2: real MPI ToolboxClean via doorbell/reply-queue")
    }

    fn send_config<'a>(&mut self, _req: &ConfigRequest<'a>) -> Result<ConfigReply, MpiError> {
        todo!("senior cycle 2: real MPI Config via doorbell/reply-queue")
    }

    fn send_ioc_init(&mut self, _req: &IocInitRequest) -> Result<IocInitReply, MpiError> {
        todo!("senior cycle 2: real MPI IocInit via doorbell/reply-queue")
    }

    fn current_personality(&self) -> Result<Personality, MpiError> {
        todo!("senior cycle 2: real MPI personality query from chip registers")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpi::messages::ImageType;
    use crate::mpi::session::{IocBackend as _, Personality};
    use crate::pci::MockPlatform;

    /// Test that RealIoc can be constructed against MockPlatform without hardware.
    #[test]
    fn realioc_open_succeeds_against_mock_platform() {
        let mock = MockPlatform::new();
        let result = RealIoc::open(mock, "0000:03:00.0");

        assert!(result.is_ok(), "RealIoc::open should succeed against MockPlatform");

        let realioc = result.unwrap();
        assert_eq!(realioc.pci_bdf(), "0000:03:00.0");
    }

    /// Compile-time trait conformance check — proves RealIoc<MockPlatform> implements IocBackend.
    #[test]
    fn realioc_implements_iocbackend_trait() {
        fn _assert<T: IocBackend>() {}
        _assert::<RealIoc<MockPlatform>>();
    }

    /// Verify that all trait methods are `todo!()` placeholders by catching their panics.
    #[test]
    fn realioc_send_methods_are_todo() {
        let mock = MockPlatform::new();
        let mut realioc: RealIoc<MockPlatform> = RealIoc::open(mock, "0000:03:00.0").unwrap();

        // Test send_fw_download panics with todo! message
        let download_req = FwDownloadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 256,
            total_image_size: 256,
            last_segment: true,
            payload: &[0u8; 256],
        };

        let download_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            realioc.send_fw_download(&download_req)
        }));
        assert!(download_result.is_err(), "send_fw_download should panic with todo!");

        // Test send_ioc_init panics with todo! message
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };

        let init_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            realioc.send_ioc_init(&init_req)
        }));
        assert!(init_result.is_err(), "send_ioc_init should panic with todo!");
    }

    /// Verify BAR1 path follows canonical sysfs convention.
    #[test]
    fn realioc_bar1_path_is_canonical_sysfs() {
        let mock = MockPlatform::new();
        let realioc = RealIoc::open(mock, "0000:03:00.0").unwrap();

        let expected = Path::new("/sys/bus/pci/devices/0000:03:00.0/resource1");
        assert_eq!(realioc.bar1_path(), expected);
    }
}
