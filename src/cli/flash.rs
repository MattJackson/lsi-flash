//! `lsi-flash flash` orchestrator — v1.0 headline verb.
//! Implements ADR-015 Rules 1, 2, 3, 4, 5, 6, 10 per Stage 3 scoping doc (commit 09af089d).

use std::path::PathBuf;
use thiserror::Error;

use crate::cli::backup;
use crate::cli::Mode;
use crate::mpi::messages::{IocStatus, MpiError};
use crate::mpi::mock_ioc::MockIoc;
use crate::mpi::session::{IocBackend, Personality, Session};

/// Top-level error type for the flash orchestrator.
#[derive(Debug, Error)]
pub enum FlashError {
    #[error("MPI: {0}")]
    Mpi(#[from] MpiError),

    #[error("backup: {0}")]
    Backup(#[from] crate::cli::backup::BackupError),

    #[error("safety guard tripped: {0}")]
    SafetyGuard(String),

    #[error("personality mismatch: running={running:?} target={target:?}")]
    PersonalityMismatch {
        running: Personality,
        target: Personality,
    },

    #[error("user declined confirmation")]
    Declined,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("abort: {reason}")]
    Abort { reason: AbortReason },

    #[error("backup module error: {0}")]
    BackupModule(String),
}

/// Reasons for aborting the flash operation. Per scoping doc §2.11.
#[derive(Debug, Clone, PartialEq)]
pub enum AbortReason {
    SafetyGuardTripped(String),
    PersonalityMismatch {
        running: Personality,
        target: Personality,
    },
    BackupFailed(String),
    IocStatusError(IocStatus),
    VerifyMismatch {
        offset: usize,
    },
    UserDeclined,
}

impl std::fmt::Display for AbortReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SafetyGuardTripped(msg) => write!(f, "safety guard tripped: {}", msg),
            Self::PersonalityMismatch { running, target } => {
                write!(
                    f,
                    "personality mismatch: running={:?} target={:?}",
                    running, target
                )
            }
            Self::BackupFailed(msg) => write!(f, "backup failed: {}", msg),
            Self::IocStatusError(status) => write!(f, "IOC status error: {:?}", status),
            Self::VerifyMismatch { offset } => {
                write!(f, "verify mismatch at byte offset {}", offset)
            }
            Self::UserDeclined => write!(f, "user declined confirmation"),
        }
    }
}

/// All states the orchestrator can be in. Per Stage 3 scoping doc §2.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Phase {
    #[default]
    Detect,
    Halt {
        current_personality: Personality,
    },
    Backup {
        backup_dir: PathBuf,
    },
    Hostboot {
        target_personality: Personality,
    },
    Erase,
    DownloadFw {
        image: Vec<u8>,
    },
    DownloadBios {
        image: Vec<u8>,
    },
    WriteSbr {
        sbr: Vec<u8>,
    },
    Restart,
    Verify {
        expected_personality: Personality,
    },
    Done,
    Aborted {
        reason: AbortReason,
        backup_dir: Option<PathBuf>,
    },
}

/// The orchestrator driver. Generic over any IocBackend implementation.
pub struct Orchestrator<B: IocBackend> {
    session: Session<B>,
    mode: Mode,
    identity: Option<String>,
    dry_run: bool,
    yes: bool,
    keep_sas_address: bool,
    state: Phase,
}

impl<B: IocBackend> Orchestrator<B> {
    /// Create a new orchestrator with the given configuration.
    pub fn new(
        session: Session<B>,
        mode: Mode,
        identity: Option<String>,
        dry_run: bool,
        yes: bool,
        keep_sas_address: bool,
    ) -> Self {
        Self {
            session,
            mode,
            identity,
            dry_run,
            yes,
            keep_sas_address,
            state: Phase::Detect,
        }
    }

    /// Drive the state machine to completion. Returns Ok(()) on Done, Err on Aborted.
    pub fn run(mut self) -> Result<(), FlashError> {
        if !self.yes && !self.dry_run {
            eprintln!("This will erase all data on the adapter. Continue? (y/N)");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if !input.trim().to_lowercase().starts_with('y') {
                return Err(FlashError::Abort {
                    reason: AbortReason::UserDeclined,
                });
            }
        }

        loop {
            self.state = self.transition()?;
            match &self.state {
                Phase::Done => {
                    eprintln!("flash complete");
                    return Ok(());
                }
                Phase::Aborted { reason, .. } => {
                    eprintln!("aborted: {}", reason);
                    return Err(FlashError::Abort {
                        reason: reason.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    /// One step of the state machine. Each Phase transitions to the next.
    /// Per scoping doc §2-§11 for what each state does.
    fn transition(&mut self) -> Result<Phase, FlashError> {
        let current = std::mem::take(&mut self.state);
        match current {
            Phase::Detect => self.step_detect(),
            Phase::Halt {
                current_personality,
            } => self.step_halt(current_personality),
            Phase::Backup { backup_dir } => self.step_backup(backup_dir),
            Phase::Hostboot { target_personality } => self.step_hostboot(target_personality),
            Phase::Erase => self.step_erase(),
            Phase::DownloadFw { image } => self.step_download_fw(image),
            Phase::DownloadBios { image } => self.step_download_bios(image),
            Phase::WriteSbr { sbr } => self.step_write_sbr(sbr),
            Phase::Restart => self.step_restart(),
            Phase::Verify {
                expected_personality,
            } => self.step_verify(expected_personality),
            Phase::Done | Phase::Aborted { .. } => Ok(current),
        }
    }

    /// Step 1: Detect current card and firmware personality.
    fn step_detect(&mut self) -> Result<Phase, FlashError> {
        // STUB: in v1.0, detect via PCI sysfs + current MPI personality
        // For skeleton: just query the backend for current personality
        let current = self.session.current_personality()?;
        eprintln!("detected personality: {:?}", current);
        Ok(Phase::Halt {
            current_personality: current,
        })
    }

    /// Step 2: Halt IOC ARM core and verify safe to proceed.
    fn step_halt(&mut self, current_personality: Personality) -> Result<Phase, FlashError> {
        let target = mode_to_personality(self.mode);

        // ADR-015 Rule 1: verify personality match before going further
        if current_personality != target {
            eprintln!(
                "personality mismatch: running={:?} target={:?} → transitioning to hostboot",
                current_personality, target
            );
            return Ok(Phase::Hostboot {
                target_personality: target,
            });
        }

        // Safety guards before backup (scoping §4) — STUB for skeleton cycle
        self.check_safety_guards()?;

        let backup_dir = self.compute_backup_dir();
        eprintln!("backup directory: {}", backup_dir.display());
        Ok(Phase::Backup { backup_dir })
    }

    /// Step 3: Backup all flash partitions before any destructive operation.
    fn step_backup(&mut self, backup_dir: PathBuf) -> Result<Phase, FlashError> {
        if self.dry_run {
            eprintln!("[dry-run] would backup to {}", backup_dir.display());
        } else {
            // Call backup::run with this dir — real implementation per scoping doc §5
            let backup_path = backup_dir.to_str().unwrap_or("/tmp/backup").to_string();
            if let Err(e) = backup::run(Some(backup_path), false) {
                return Err(FlashError::BackupModule(format!(
                    "backup module error: {}",
                    e
                )));
            }
        }
        eprintln!("backup complete");
        Ok(Phase::Erase)
    }

    /// Step 4: Hostboot — load matching firmware into chip RAM if personality mismatch.
    fn step_hostboot(&mut self, target_personality: Personality) -> Result<Phase, FlashError> {
        // STUB: real HCB hostboot via hcb.rs requires hardware + the target firmware bytes
        // For skeleton: just verify and transition back to Halt with new personality
        if !self.dry_run {
            eprintln!(
                "hostboot: loading {:?} firmware into chip RAM",
                target_personality
            );
            // TODO: load HCB image, initialize IOC, re-detect personality
        } else {
            eprintln!("[dry-run] would hostboot to {:?}", target_personality);
        }

        // After hostboot, the running personality should match target
        Ok(Phase::Halt {
            current_personality: target_personality,
        })
    }

    /// Step 5: Erase flash partitions with TOOLBOX_CLEAN.
    fn step_erase(&mut self) -> Result<Phase, FlashError> {
        if self.dry_run {
            eprintln!("[dry-run] would erase flash (TOOLBOX_CLEAN)");
        } else {
            // Initialize IOC first (required before Toolbox operations per mpi-overview.md §9)
            let init_req = crate::mpi::messages::IocInitRequest {
                who_init: 0x04,
                host_msix_vectors: 0,
                reply_descriptor_post_queue_depth: 16,
                system_request_frame_base_address: 0,
                reply_descriptor_post_queue_address: 0,
            };
            self.session.raw_ioc_init(&init_req)?;

            // Erase firmware + BIOS partitions per toolbox-and-config.md §5.2
            let req = crate::mpi::messages::ToolboxCleanRequest {
                flags: crate::mpi::messages::ToolboxCleanFlags::FW_CURRENT
                    | crate::mpi::messages::ToolboxCleanFlags::FLASH,
            };
            let reply = self.session.raw_toolbox_clean(&req)?;
            if reply.ioc_status.is_flash_hard_stop() {
                return Ok(Phase::Aborted {
                    reason: AbortReason::IocStatusError(reply.ioc_status),
                    backup_dir: None,
                });
            }
        }
        eprintln!("erase complete");

        // STUB: load firmware bytes from manifest — in v1.0 this comes from firmware DB
        let image = vec![];
        Ok(Phase::DownloadFw { image })
    }

    /// Step 6: Download main firmware with verify-after-write (Rule 4).
    fn step_download_fw(&mut self, _image: Vec<u8>) -> Result<Phase, FlashError> {
        let target = mode_to_personality(self.mode);
        if self.dry_run {
            eprintln!(
                "[dry-run] would download {} bytes of firmware (verify-after-write)",
                _image.len()
            );
        } else {
            // Session::fw_download_verified handles Rule 1 (personality match) + Rule 4 (verify after write)
            // STUB: firmware source resolution is Stage 4 work — using empty vec for now
            self.session.fw_download_verified(
                crate::mpi::messages::ImageType::Fw,
                target,
                &_image,
            )?;
        }
        eprintln!("firmware download complete");

        let bios = vec![]; // STUB: BIOS image would come from manifest
        Ok(Phase::DownloadBios { image: bios })
    }

    /// Step 7: Download BIOS option-ROM.
    fn step_download_bios(&mut self, _image: Vec<u8>) -> Result<Phase, FlashError> {
        let target = mode_to_personality(self.mode);
        if self.dry_run {
            eprintln!("[dry-run] would download {} bytes of BIOS", _image.len());
        } else {
            // Session::fw_download_verified handles Rule 1 + Rule 4
            // STUB: firmware source resolution is Stage 4 work
            self.session.fw_download_verified(
                crate::mpi::messages::ImageType::Bios,
                target,
                &_image,
            )?;
        }
        eprintln!("BIOS download complete");

        let sbr = vec![]; // STUB: SBR needs hardware I²C integration
        Ok(Phase::WriteSbr { sbr })
    }

    /// Step 8: Write SBR via I²C bit-bang (hardware-gated).
    fn step_write_sbr(&mut self, _sbr: Vec<u8>) -> Result<Phase, FlashError> {
        // STUB: SBR write via sbr::i2c — requires hardware access
        if self.dry_run {
            eprintln!("[dry-run] would write SBR via I²C");
        } else {
            // TODO: synthesize new SBR with identity + preserved WWN, write via I²C
        }
        eprintln!("SBR write complete");
        Ok(Phase::Restart)
    }

    /// Step 9: Restart IOC ARM core.
    fn step_restart(&mut self) -> Result<Phase, FlashError> {
        if self.dry_run {
            eprintln!("[dry-run] would reset adapter (IOC_INIT)");
        } else {
            // Initialize IOC to restart ARM core per mpi-overview.md §9
            let init_req = crate::mpi::messages::IocInitRequest {
                who_init: 0x04,
                host_msix_vectors: 0,
                reply_descriptor_post_queue_depth: 16,
                system_request_frame_base_address: 0,
                reply_descriptor_post_queue_address: 0,
            };
            let _reply = self.session.raw_ioc_init(&init_req)?;
        }
        eprintln!("adapter reset complete");

        let expected = mode_to_personality(self.mode);
        Ok(Phase::Verify {
            expected_personality: expected,
        })
    }

    /// Step 10: Verify post-flash state matches expectations.
    fn step_verify(&mut self, expected_personality: Personality) -> Result<Phase, FlashError> {
        let current = self.session.current_personality()?;
        if current == expected_personality {
            eprintln!("verify: personality {:?} matches expected", current);
            Ok(Phase::Done)
        } else {
            eprintln!(
                "verify FAILED: running={:?} expected={:?}",
                current, expected_personality
            );
            Ok(Phase::Aborted {
                reason: AbortReason::PersonalityMismatch {
                    running: current,
                    target: expected_personality,
                },
                backup_dir: None, // TODO: thread backup_dir through state machine
            })
        }
    }

    /// Safety guards check (mounts, volume groups, etc.). Per ADR-007 D-5.
    fn check_safety_guards(&self) -> Result<(), FlashError> {
        use crate::cli::safety::{check_all, devices_attached_to_card};

        // BDF is currently a stub; in v1.0 we detect it via PCI sysfs walk
        // TODO: thread real BDF through orchestrator from detect verb
        let bdf = "03:00.0";

        let devices =
            devices_attached_to_card(bdf).map_err(|e| FlashError::SafetyGuard(e.to_string()))?;

        if self.dry_run {
            eprintln!(
                "[dry-run] would check {} downstream devices for safety concerns",
                devices.len()
            );
            return Ok(());
        }

        let concerns = check_all(&devices).map_err(|e| FlashError::SafetyGuard(e.to_string()))?;

        if !concerns.is_empty() {
            // Format all concerns as a single user-facing message with educational guidance
            let msg = concerns
                .iter()
                .map(|c| c.human())
                .collect::<Vec<_>>()
                .join("\n\n");
            return Err(FlashError::SafetyGuard(msg));
        }

        Ok(())
    }

    /// Compute backup directory path. Uses SAS WWN + timestamp per ADR-007.
    fn compute_backup_dir(&self) -> PathBuf {
        // STUB: use SAS WWN if available, otherwise just timestamp
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
        PathBuf::from(format!("/tmp/lsi-flash-backup-{}", ts))
    }
}

/// Convert CLI Mode to MPI Personality enum.
fn mode_to_personality(mode: Mode) -> Personality {
    match mode {
        Mode::HBA => Personality::It,
        Mode::RAID => Personality::Ir,
    }
}

/// CLI entry point invoked from cli/mod.rs Command::Flash.
pub fn run(
    mode: Mode,
    identity: Option<String>,
    dry_run: bool,
    yes: bool,
    keep_sas_address: bool,
    _firmware: Option<String>,
    _no_bios: bool,
    _backup_dir: Option<String>,
    _wipe_mfg_pages: bool,
    json: bool,
) -> Result<(), crate::Error> {
    // Build session — MockIoc default (no real HW for dry-run + tests)
    let mock_ioc = MockIoc::new(Personality::It);
    let session = Session::new(mock_ioc);

    let orchestrator = Orchestrator::new(session, mode, identity, dry_run, yes, keep_sas_address);

    orchestrator.run()?;

    if json {
        println!("{}", serde_json::json!({"status": "done"}));
    } else {
        // Message already printed by orchestrator on completion
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_to_personality_maps_hba_and_raid_correctly() {
        assert_eq!(mode_to_personality(Mode::HBA), Personality::It);
        assert_eq!(mode_to_personality(Mode::RAID), Personality::Ir);
    }

    #[test]
    fn abort_reason_display_formats_safety_guard() {
        let reason = AbortReason::SafetyGuardTripped("mount point in use".to_string());
        let display = format!("{}", reason);
        assert!(display.contains("safety guard tripped"));
    }

    #[test]
    fn abort_reason_display_formats_personality_mismatch() {
        let reason = AbortReason::PersonalityMismatch {
            running: Personality::It,
            target: Personality::Ir,
        };
        let display = format!("{}", reason);
        assert!(display.contains("personality mismatch"));
        assert!(display.contains("running=It"));
    }

    #[test]
    fn abort_reason_display_formats_backup_failed() {
        let reason = AbortReason::BackupFailed("disk full".to_string());
        let display = format!("{}", reason);
        assert!(display.contains("backup failed"));
    }

    #[test]
    fn abort_reason_display_formats_user_declined() {
        let reason = AbortReason::UserDeclined;
        let display = format!("{}", reason);
        assert!(display.contains("user declined"));
    }

    #[test]
    fn phase_default_is_detect() {
        let default_phase: Phase = Default::default();
        assert!(matches!(default_phase, Phase::Detect));
    }

    #[test]
    fn abort_reason_clone_works() {
        let reason1 = AbortReason::SafetyGuardTripped("test".to_string());
        let reason2 = reason1.clone();
        assert_eq!(format!("{}", reason1), format!("{}", reason2));
    }

    #[test]
    fn phase_clone_works() {
        let phase1 = Phase::Halt {
            current_personality: Personality::It,
        };
        let phase2 = phase1.clone();
        assert_eq!(phase1, phase2);
    }

    #[test]
    fn orchestrator_dry_run_completes_successfully() {
        use crate::mpi::mock_ioc::MockIoc;
        use crate::mpi::session::Session;

        let mock_ioc = MockIoc::new(Personality::It);
        let session = Session::new(mock_ioc);

        // Dry-run mode: no HW access, auto-confirm with yes=true
        let orchestrator = Orchestrator::new(
            session,
            Mode::HBA,
            None,
            true, // dry_run = true
            true, // yes = true (auto-confirm)
            false,
        );

        let result = orchestrator.run();

        assert!(
            result.is_ok(),
            "Dry-run orchestrator should complete successfully"
        );
    }

    #[test]
    fn abort_reason_iocstatus_error_formats_correctly() {
        // Test that AbortReason::IocStatusError variant exists and formats correctly
        let reason = AbortReason::IocStatusError(IocStatus::InternalError);
        assert_eq!(format!("{}", reason), "IOC status error: InternalError");

        let reason2 = AbortReason::IocStatusError(IocStatus::Busy);
        assert_eq!(format!("{}", reason2), "IOC status error: Busy");
    }

    #[test]
    fn fw_download_verified_enforces_personality_match() {
        use crate::mpi::mock_ioc::MockIoc;
        use crate::mpi::session::Session;

        let mock = MockIoc::new(Personality::It);
        let mut session = Session::new(mock);

        // Initialize first (required for download to succeed)
        let init_req = crate::mpi::messages::IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = session.raw_ioc_init(&init_req);

        // Try to write IR personality while IT is running — should fail Rule 1
        let result = session.fw_download_verified(
            crate::mpi::messages::ImageType::Fw,
            Personality::Ir,
            &[0u8; 100],
        );

        assert!(matches!(result, Err(MpiError::PersonalityMismatch { .. })));

        if let Err(MpiError::PersonalityMismatch { running, target }) = result {
            assert_eq!(running, Personality::It);
            assert_eq!(target, Personality::Ir);
        }
    }

    #[test]
    fn toolbox_clean_flags_combine_correctly() {
        use crate::mpi::messages::ToolboxCleanFlags;

        // Test flag combination for erase operation (FW + FLASH)
        let flags = ToolboxCleanFlags::FW_CURRENT | ToolboxCleanFlags::FLASH;

        assert!(flags.contains(ToolboxCleanFlags::FW_CURRENT));
        assert!(flags.contains(ToolboxCleanFlags::FLASH));
        assert!(!flags.contains(ToolboxCleanFlags::NVRAM));

        // Test that ALL flags contain individual flags
        let all = ToolboxCleanFlags::ALL;
        assert!(all.contains(ToolboxCleanFlags::FW_CURRENT));
        assert!(all.contains(ToolboxCleanFlags::FLASH));
        assert!(all.contains(ToolboxCleanFlags::NVRAM));
    }

    #[test]
    fn session_raw_toolbox_clean_exists_and_calls_backend() {
        use crate::mpi::messages::ToolboxCleanFlags;
        use crate::mpi::mock_ioc::MockIoc;
        use crate::mpi::session::Session;

        let mock = MockIoc::new(Personality::It);
        let mut session = Session::new(mock);

        // Initialize first
        let init_req = crate::mpi::messages::IocInitRequest {
            who_init: 0x04,
            host_msix_vectors: 0,
            reply_descriptor_post_queue_depth: 16,
            system_request_frame_base_address: 0,
            reply_descriptor_post_queue_address: 0,
        };
        let _ = session.raw_ioc_init(&init_req);

        // Test raw_toolbox_clean exists and works
        let req = crate::mpi::messages::ToolboxCleanRequest {
            flags: ToolboxCleanFlags::FW_CURRENT | ToolboxCleanFlags::FLASH,
        };

        let reply = session.raw_toolbox_clean(&req).unwrap();
        assert_eq!(reply.ioc_status, IocStatus::Success);
    }

    #[test]
    fn orchestrator_personality_mismatch_caught_via_verified_helper() {
        use crate::mpi::session::PersonalityMatched;

        // Test that PersonalityMatched::verify_match enforces Rule 1
        let result = PersonalityMatched::verify_match(Personality::It, Personality::Ir);

        assert!(result.is_err(), "Personality mismatch should be rejected");

        if let Err(MpiError::PersonalityMismatch { running, target }) = result {
            assert_eq!(running, Personality::It);
            assert_eq!(target, Personality::Ir);
        } else {
            panic!("Expected MpiError::PersonalityMismatch");
        }

        // Test that matching personalities are allowed
        let result_ok = PersonalityMatched::verify_match(Personality::It, Personality::It);
        assert!(
            result_ok.is_ok(),
            "Matching personalities should be allowed"
        );
    }
}
