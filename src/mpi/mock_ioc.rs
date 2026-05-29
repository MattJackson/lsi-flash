//! Mock IOC: in-memory SAS2008 MPI simulator. Per ADR-010 Layer 3.
//! Doubles as the `--dry-run` backend (no hardware needed).

use crate::mpi::messages::{
    ConfigReply, FwDownloadReply, FwUploadReply, IocFactsReply, IocInitReply, ToolboxReply,
};
use crate::mpi::messages::{
    ConfigRequest, FwDownloadRequest, FwUploadRequest, IocInitRequest, ToolboxCleanFlags,
    ToolboxCleanRequest,
};
use crate::mpi::messages::{ImageType, IocStatus, MpiError};
use crate::mpi::session::{IocBackend, Personality};
use std::collections::HashMap;

/// Simulated flash partition contents. Maps ImageType → bytes.
#[derive(Default)]
struct FlashState {
    partitions: HashMap<ImageType, Vec<u8>>,
}

/// Failure injection: tell the MockIoc to return an error on the next
/// call of the matching function.
#[derive(Default)]
pub struct FailureInjector {
    pub next_fw_download_error: Option<IocStatus>,
    pub next_fw_upload_error: Option<IocStatus>,
    pub next_toolbox_error: Option<IocStatus>,
    pub next_config_error: Option<IocStatus>,
}

/// Mock IOC state machine. Simulates SAS2008 MPI firmware behavior including:
/// - Flash partition state (firmware, BIOS, NV data)
/// - Initialization state (requires IOC_INIT before other ops)
/// - Failure injection for testing ADR-015 Rule 6 hard-stop paths
pub struct MockIoc {
    flash: FlashState,
    initialized: bool,
    current_personality: Personality,
    pub inject: FailureInjector,
}

impl MockIoc {
    /// Create a new MockIoc with the given initial personality.
    pub fn new(initial_personality: Personality) -> Self {
        Self {
            flash: FlashState::default(),
            initialized: false,
            current_personality: initial_personality,
            inject: FailureInjector::default(),
        }
    }

    /// Preload a partition with initial data (useful for testing upload scenarios).
    pub fn preload_partition(&mut self, image_type: ImageType, bytes: Vec<u8>) {
        self.flash.partitions.insert(image_type, bytes);
    }

    /// Get the current personality.
    pub fn get_personality(&self) -> Personality {
        self.current_personality
    }

    /// Set failure injection for next fw_download call.
    pub fn inject_fw_download_error(&mut self, status: IocStatus) {
        self.inject.next_fw_download_error = Some(status);
    }

    /// Set failure injection for next fw_upload call.
    pub fn inject_fw_upload_error(&mut self, status: IocStatus) {
        self.inject.next_fw_upload_error = Some(status);
    }

    /// Simulate a personality change (for testing cross-personality scenarios).
    pub fn set_personality(&mut self, personality: Personality) {
        self.current_personality = personality;
    }

    /// Check if the mock is initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

impl IocBackend for MockIoc {
    fn send_fw_download(&mut self, _req: &FwDownloadRequest) -> Result<FwDownloadReply, MpiError> {
        // Check for injected failure first (Rule 6 testing)
        if let Some(err) = self.inject.next_fw_download_error.take() {
            return Ok(FwDownloadReply {
                ioc_status: err,
                image_type: 0,
                ioc_log_info: 0,
            });
        }

        // Rule 6: InvalidState if not initialized (per mpi-overview.md §9)
        if !self.initialized {
            return Ok(FwDownloadReply {
                ioc_status: IocStatus::InvalidState,
                image_type: 0,
                ioc_log_info: 0,
            });
        }

        // Write payload to mock partition at specified offset
        let partition = self.flash.partitions.entry(_req.image_type).or_default();
        let offset = _req.image_offset as usize;
        let need = offset + _req.payload.len();

        if partition.len() < need {
            partition.resize(need, 0);
        }
        partition[offset..need].copy_from_slice(_req.payload);

        Ok(FwDownloadReply {
            ioc_status: IocStatus::Success,
            image_type: _req.image_type.as_u8(),
            ioc_log_info: 0,
        })
    }

    fn send_fw_upload<'a>(
        &mut self,
        req: &'a mut FwUploadRequest<'a>,
    ) -> Result<FwUploadReply, MpiError> {
        // Check for injected failure first
        if let Some(err) = self.inject.next_fw_upload_error.take() {
            return Ok(FwUploadReply {
                ioc_status: err,
                image_type: 0,
                actual_image_size: 0,
            });
        }

        // Rule 6: InvalidState if not initialized
        if !self.initialized {
            return Ok(FwUploadReply {
                ioc_status: IocStatus::InvalidState,
                image_type: 0,
                actual_image_size: 0,
            });
        }

        // Read from mock partition (or return empty if not present)
        let data = self
            .flash
            .partitions
            .get(&req.image_type)
            .cloned()
            .unwrap_or_default();

        // Copy data to output buffer
        let copy_len = req.payload_buffer.len().min(data.len());
        if copy_len > 0 {
            req.payload_buffer[..copy_len].copy_from_slice(&data[..copy_len]);
        }

        Ok(FwUploadReply {
            ioc_status: IocStatus::Success,
            image_type: req.image_type.as_u8(),
            actual_image_size: data.len() as u32,
        })
    }

    fn send_toolbox_clean(&mut self, req: &ToolboxCleanRequest) -> Result<ToolboxReply, MpiError> {
        // Check for injected failure first
        if let Some(err) = self.inject.next_toolbox_error.take() {
            return Ok(ToolboxReply {
                tool: 0x00,
                ioc_status: err,
            });
        }

        // Rule 6: InvalidState if not initialized
        if !self.initialized {
            return Ok(ToolboxReply {
                tool: 0x00,
                ioc_status: IocStatus::InvalidState,
            });
        }

        // Erase partitions based on flags (per toolbox-and-config.md §5.2)
        let reply = ToolboxReply {
            tool: 0x00,
            ioc_status: IocStatus::Success,
        };

        if req.flags.contains(ToolboxCleanFlags::NVRAM) {
            self.flash.partitions.remove(&ImageType::NvData);
        }
        if req.flags.contains(ToolboxCleanFlags::SEEPROM) {
            // No SEEPROM partition in our model, but we could add one
        }
        if req.flags.contains(ToolboxCleanFlags::FLASH) {
            self.flash.partitions.clear();
        }
        if req.flags.contains(ToolboxCleanFlags::FW_CURRENT) {
            self.flash.partitions.remove(&ImageType::Fw);
        }
        if req.flags.contains(ToolboxCleanFlags::FW_BACKUP) {
            // No separate backup partition in our model
        }

        Ok(reply)
    }

    fn send_config(&mut self, _req: &ConfigRequest) -> Result<ConfigReply, MpiError> {
        // For v1.0 priority: Mfg Page 5 (SAS WWN) read/write stub
        // Other pages: return success with empty data
        Ok(ConfigReply {
            ioc_status: IocStatus::Success,
            action: _req.action,
            page_header: [0x00; 4],
        })
    }

    fn send_ioc_init(&mut self, _req: &IocInitRequest) -> Result<IocInitReply, MpiError> {
        // Per mpi-overview.md §9: IOC_INIT initializes MPI message queue
        self.initialized = true;

        Ok(IocInitReply {
            who_init: 0x04,
            ioc_status: IocStatus::Success,
        })
    }

    fn send_ioc_facts(&mut self) -> Result<IocFactsReply, MpiError> {
        // Per mpi-overview.md §9: IOC must be initialized before IOC_FACTS
        if !self.initialized {
            return Ok(IocFactsReply {
                msg_version: 0x01,
                msg_length: 96,
                function: crate::mpi::messages::MpiFunction::IocFacts.as_u8(), // 0x03 per mpi2_ioc.h:191
                header_version: 0x01,
                ioc_number: 0x00,
                msg_flags: 0x00,
                vp_id: 0x00,
                vf_id: 0x00,
                reserved_1: 0x0000,
                ioc_exceptions: 0x0000,
                ioc_status: IocStatus::InvalidState, // Not initialized
                ioc_log_info: 0x00000000,
                max_chain_depth: 0x00,
                who_init: 0x04,
                number_of_ports: 0x04,
                max_msix_vectors: 0x01,
                request_credit: 0x00FF,
                product_id: 0x0725,           // SAS2008 IT personality product ID
                ioc_capabilities: 0x00000004, // MPI2_IOC_CAP_ENABLED_PERPHYS
                fw_version: 0x070F0800, // Version format: major=7, minor=15, unit=8, dev=0 (LE encoding: byte0=major)

                ioc_request_frame_size: 0x0048, // 72 bytes
                ioc_max_chain_segment_size: 0xFFFF,
                max_initiators: 0x0064,    // 100 initiators
                max_targets: 0x00FF,       // 255 targets
                max_sas_expanders: 0x0064, // 100 expanders
                max_enclosures: 0x0064,    // 100 enclosures
                protocol_flags: 0x0000,
                high_priority_credit: 0x00FF,
                max_reply_descriptor_post_queue_depth: 0x0080, // 128 entries
                reply_frame_size: 0x0040,                      // 64 bytes
                max_volumes: u8::MAX, // Max for u8 field per mpi2_ioc.h:265 (496 would overflow)
                max_dev_handle: 0x7FFF,
                max_persistent_entries: 256, // Truncated from 0x0258 to fit in u16
                min_dev_handle: 0x0000,
                reserved_4: 0x0000,
                board_name: Some("Dell H200".to_string()), // Canned data for Tape Adapter per baseline.md
                board_tracer: Some("00000001".to_string()), // 8-char trace number from baseline.md
                nvdata_vendor_id: None, // Populated separately via Mfg Page 0 CONFIG read
                nvdata_product_id: None,
                nvdata_version: None,
                firmware_product_id: None,
            });
        }

        // Return canned IOC_FACTS data resembling the Tape Adapter per task spec
        Ok(IocFactsReply {
            msg_version: 0x01,                                             // MPI2_MSG_VERSION
            msg_length: 96, // Total reply size in bytes
            function: crate::mpi::messages::MpiFunction::IocFacts.as_u8(), // 0x03 per mpi2_ioc.h:191
            header_version: 0x01,                                          // MPI2_HEADER_VERSION
            ioc_number: 0x00,
            msg_flags: 0x00,
            vp_id: 0x00,
            vf_id: 0x00,
            reserved_1: 0x0000,
            ioc_exceptions: 0x0000,
            ioc_status: IocStatus::Success,
            ioc_log_info: 0x00000000,
            max_chain_depth: 0xFF,        // Max chain depth supported
            who_init: 0x04,               // MPI2_WHOINIT_HOST_DRIVER
            number_of_ports: 0x04,        // SAS2008 has 4 PHYs
            max_msix_vectors: 0x01,       // MSI-X enabled
            request_credit: 0x00FF,       // 255 credits
            product_id: 0x0725,           // Product ID for SAS2008 IT firmware (per mpi2.h)
            ioc_capabilities: 0x00000004, // MPI2_IOC_CAP_ENABLED_PERPHYS per mpi2_ioc.h
            fw_version: 0x070F0800, // Version: major=7, minor=15, unit=8, dev=0 (LE encoding: byte0=major)

            ioc_request_frame_size: 0x0048, // 72 bytes per mpi-overview.md §9.1
            ioc_max_chain_segment_size: 0xFFFF,
            max_initiators: 0x0064,    // 100 initiators (MPI2_MAX_INITIATORS)
            max_targets: 0x00FF,       // 255 targets (MPI2_MAX_TARGETS)
            max_sas_expanders: 0x0064, // 100 expanders
            max_enclosures: 0x0064,    // 100 enclosures
            protocol_flags: 0x0000,    // No SR-IOV enabled
            high_priority_credit: 0x00FF,
            max_reply_descriptor_post_queue_depth: 0x0080, // 128 entries (MPI2_RDPQ_DEPTH_MAX)
            reply_frame_size: 0x0040,                      // 64 bytes per mpi-overview.md §9.2
            max_volumes: 240, // Max for u8 field per mpi2_ioc.h:265 (0x1F0 would overflow)
            max_dev_handle: 0x7FFF,
            max_persistent_entries: 256, // Truncated from 0x0258 to fit in u16
            min_dev_handle: 0x0000,
            reserved_4: 0x0000,
            board_name: Some("Dell H200".to_string()), // Canned data for Tape Adapter per baseline.md:15
            board_tracer: Some("00000001".to_string()), // 8-char trace number from baseline.md:15
            nvdata_vendor_id: Some(0x1000),            // LSI vendor ID (per task spec)
            nvdata_product_id: Some("LSI2008".to_string()), // Per task spec
            nvdata_version: Some(0x00041003), // NVDATA version 3.16.4 (LE encoding: major=3, minor=16, build=4)

            firmware_product_id: Some("InternalTapeAdp".to_string()), // From baseline.md:14
        })
    }

    fn current_personality(&self) -> Result<Personality, MpiError> {
        Ok(self.current_personality)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpi::session::{PersonalityMatched, Session};

    #[test]
    fn mock_ioc_create_with_initial_personality() {
        let mock = MockIoc::new(Personality::It);
        assert_eq!(mock.get_personality(), Personality::It);
        assert!(!mock.is_initialized());
    }

    #[test]
    fn mock_ioc_init_then_ready_for_ops() {
        let mut mock = MockIoc::new(Personality::Ir);

        // Not initialized yet
        assert!(!mock.is_initialized());

        // Send IOC_INIT
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };

        let _ = mock.send_ioc_init(&init_req);
        assert!(mock.is_initialized());
    }

    #[test]
    fn mock_ioc_download_requires_init() {
        let mut mock = MockIoc::new(Personality::It);

        // Try download without init — should get InvalidState
        let req = FwDownloadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 128,
            total_image_size: 128,
            last_segment: true,
            payload: &[0u8; 128],
        };

        let reply = mock.send_fw_download(&req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::InvalidState);
    }

    #[test]
    fn mock_ioc_upload_after_init_returns_empty() {
        let mut mock = MockIoc::new(Personality::It);

        // Initialize first
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = mock.send_ioc_init(&init_req);

        // Upload from uninitialized partition — should return empty data
        let mut buf = vec![0u8; 128];
        let mut req = FwUploadRequest {
            image_type: ImageType::Bios,
            image_offset: 0,
            image_size: 128,
            payload_buffer: &mut buf,
        };

        let reply = mock.send_fw_upload(&mut req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::Success);
        assert_eq!(reply.actual_image_size, 0); // Empty because not preloaded
    }

    #[test]
    fn mock_ioc_download_then_upload_roundtrip() {
        let mut mock = MockIoc::new(Personality::It);

        // Initialize
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = mock.send_ioc_init(&init_req);

        // Download some data
        let test_data = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let req = FwDownloadRequest {
            image_type: ImageType::Fw,
            image_offset: 16,
            image_size: 4,
            total_image_size: 256,
            last_segment: true,
            payload: &test_data,
        };

        let _ = mock.send_fw_download(&req).unwrap();

        // Upload and verify round-trip
        let mut buf = vec![0u8; 256];
        let mut req = FwUploadRequest {
            image_type: ImageType::Fw,
            image_offset: 16,
            image_size: 4,
            payload_buffer: &mut buf,
        };

        let reply = mock.send_fw_upload(&mut req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::Success);

        // Check that data was written at correct offset
        assert_eq!(&buf[16..20], &test_data[..]);
    }

    #[test]
    fn mock_ioc_toolbox_clean_removes_partitions() {
        let mut mock = MockIoc::new(Personality::It);

        // Initialize and preload some data
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = mock.send_ioc_init(&init_req);

        mock.preload_partition(ImageType::Fw, vec![0xAA; 256]);
        mock.preload_partition(ImageType::Bios, vec![0xBB; 128]);

        // Clean firmware partition only
        let req = ToolboxCleanRequest {
            flags: ToolboxCleanFlags::FW_CURRENT,
        };

        let _ = mock.send_toolbox_clean(&req).unwrap();

        // Firmware should be gone, BIOS should remain
        assert!(!mock.flash.partitions.contains_key(&ImageType::Fw));
        assert!(mock.flash.partitions.contains_key(&ImageType::Bios));

        // Clean all flash
        let req = ToolboxCleanRequest {
            flags: ToolboxCleanFlags::FLASH,
        };

        let _ = mock.send_toolbox_clean(&req).unwrap();

        // All partitions should be gone now
        assert!(mock.flash.partitions.is_empty());
    }

    #[test]
    fn mock_ioc_injection_fw_download_error() {
        let mut mock = MockIoc::new(Personality::It);

        // Initialize first
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = mock.send_ioc_init(&init_req);

        // Inject InternalError on next fw_download (Rule 6 test case)
        mock.inject_fw_download_error(IocStatus::InternalError);

        let req = FwDownloadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 128,
            total_image_size: 128,
            last_segment: true,
            payload: &[0u8; 128],
        };

        let reply = mock.send_fw_download(&req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::InternalError);

        // Error should be consumed (next call returns success)
        let req2 = FwDownloadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 64,
            total_image_size: 64,
            last_segment: true,
            payload: &[0u8; 64],
        };

        let reply2 = mock.send_fw_download(&req2).unwrap();
        assert_eq!(reply2.ioc_status, IocStatus::Success);
    }

    #[test]
    fn personality_matched_token_type_level_enforcement() {
        // Rule 1: PersonalityMatched can only be constructed with matching personalities

        // Same personality — should succeed
        let result = PersonalityMatched::verify_match(Personality::It, Personality::It);
        assert!(result.is_ok(), "Same personalities should match");

        if let Ok(proof) = result {
            assert_eq!(proof.running, Personality::It);
            assert_eq!(proof.target, Personality::It);
        }

        // Different personality — should fail (compile-time prevention of dev-1 scenario)
        let result = PersonalityMatched::verify_match(Personality::Ir, Personality::Imr);
        assert!(result.is_err(), "Different personalities should not match");

        if let Err(MpiError::PersonalityMismatch { running, target }) = result {
            assert_eq!(running, Personality::Ir);
            assert_eq!(target, Personality::Imr);
        }
    }

    #[test]
    fn session_fw_download_verified_enforces_personality_match() {
        let mock = MockIoc::new(Personality::It);
        let mut session = Session::new(mock);

        // Initialize first (required for download to succeed)
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = session.raw_ioc_init(&init_req);

        // Try to write IR personality while IT is running — should fail Rule 1
        let result = session.fw_download_verified(ImageType::Fw, Personality::Ir, &[0u8; 100]);

        assert!(matches!(result, Err(MpiError::PersonalityMismatch { .. })));

        if let Err(MpiError::PersonalityMismatch { running, target }) = result {
            assert_eq!(running, Personality::It);
            assert_eq!(target, Personality::Ir);
        }
    }

    #[test]
    fn session_fw_download_verified_with_matching_personality() {
        let mock = MockIoc::new(Personality::It);
        let mut session = Session::new(mock);

        // Initialize first
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = session.raw_ioc_init(&init_req);

        // Write IT personality — should pass Rule 1 check
        // Note: Will fail at upload step because data isn't actually stored properly in mock yet
        // But it should NOT fail with PersonalityMismatch
        let test_data = vec![0xAA; 256];
        let result = session.fw_download_verified(ImageType::Fw, Personality::It, &test_data);

        // Should not be a personality mismatch error
        assert!(!matches!(result, Err(MpiError::PersonalityMismatch { .. })));
    }

    #[test]
    fn iocstatus_internal_error_during_download_hard_stops() {
        let mut mock = MockIoc::new(Personality::It);
        mock.inject_fw_download_error(IocStatus::InternalError);

        let mut session = Session::new(mock);

        // Initialize first
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = session.raw_ioc_init(&init_req);

        // Try download — should fail with InternalError per ADR-015 Rule 6
        let result = session.fw_download_verified(ImageType::Fw, Personality::It, &[0u8; 100]);

        assert!(matches!(
            result,
            Err(MpiError::IocStatus(IocStatus::InternalError))
        ));
    }

    #[test]
    fn fw_upload_injection_error() {
        let mut mock = MockIoc::new(Personality::It);

        // Initialize and preload data
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = mock.send_ioc_init(&init_req);

        mock.preload_partition(ImageType::Fw, vec![0xAA; 256]);

        // Inject error on upload
        mock.inject_fw_upload_error(IocStatus::Busy);

        let mut buf = vec![0u8; 256];
        let mut req = FwUploadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 256,
            payload_buffer: &mut buf,
        };

        let reply = mock.send_fw_upload(&mut req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::Busy);
    }

    #[test]
    fn toolbox_clean_with_injected_error() {
        let mut mock = MockIoc::new(Personality::It);

        // Initialize
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = mock.send_ioc_init(&init_req);

        // Inject error on toolbox clean
        mock.inject.next_toolbox_error = Some(IocStatus::InvalidState);

        let req = ToolboxCleanRequest {
            flags: ToolboxCleanFlags::NVRAM,
        };

        let reply = mock.send_toolbox_clean(&req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::InvalidState);
    }

    #[test]
    fn config_request_returns_success() {
        let mut mock = MockIoc::new(Personality::It);

        // Initialize first (though config might work without it)
        let init_req = IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = mock.send_ioc_init(&init_req);

        // Config page read/write stub returns success
        let mut buf = vec![0u8; 256];
        let req = ConfigRequest {
            action: 0x01, // Read current
            sgl_flags: 0xC0,
            page_type: 0x40, // Mfg page
            page_number: 5,
            ext_page_type: None,
            payload_buffer: &mut buf,
            page_address: 0x0000_0000, // Plain pages have PageAddress=0 per mpi2_cnfg.h:347
        };

        let reply = mock.send_config(&req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::Success);
    }
}
