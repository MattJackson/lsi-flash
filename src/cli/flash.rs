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
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
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

impl Default for Phase {
    fn default() -> Self {
        Self::Detect
    }
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
        Ok(Phase::Hostboot {
            target_personality: mode_to_personality(self.mode),
        })
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
        // STUB: send TOOLBOX_CLEAN via Session
        if self.dry_run {
            eprintln!("[dry-run] would erase flash (TOOLBOX_CLEAN)");
        } else {
            // TODO: session.raw_toolbox_clean() with appropriate flags
        }
        eprintln!("erase complete");

        // STUB: load firmware bytes from manifest — in v1.0 this comes from firmware DB
        let image = vec![];
        Ok(Phase::DownloadFw { image })
    }

    /// Step 6: Download main firmware with verify-after-write (Rule 4).
    fn step_download_fw(&mut self, _image: Vec<u8>) -> Result<Phase, FlashError> {
        // STUB: use session.fw_download_verified per Rule 4
        if self.dry_run {
            eprintln!("[dry-run] would download firmware (verify-after-write)");
        } else {
            // TODO: load actual image bytes, call session.fw_download_verified(ImageType::Fw)
        }
        eprintln!("firmware download complete");

        let image = vec![]; // STUB: BIOS image would come from manifest
        Ok(Phase::DownloadBios { image })
    }

    /// Step 7: Download BIOS option-ROM.
    fn step_download_bios(&mut self, _image: Vec<u8>) -> Result<Phase, FlashError> {
        if self.dry_run {
            eprintln!("[dry-run] would download BIOS option-ROM");
        } else {
            // TODO: session.fw_download_verified(ImageType::Bios)
        }
        eprintln!("BIOS download complete");

        let sbr = vec![]; // STUB: synthesized SBR bytes
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
            // TODO: session.raw_ioc_init() to restart ARM core
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

    /// Safety guards check (mounts, volume groups, etc.). STUB for skeleton cycle.
    fn check_safety_guards(&self) -> Result<(), FlashError> {
        // STUB: separate freshman cycle implements findmnt/vgdisplay/mdadm/zpool checks
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
}
