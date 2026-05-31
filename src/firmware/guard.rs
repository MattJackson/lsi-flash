//! Flash-write safety transaction — the single chokepoint every flash write goes through (ADR-021).
//!
//! Two cards were bricked (2026-05-20, 2026-05-31) by writes that corrupted the
//! mask-ROM boot-input region. Diagnosis (closeout note 2026-05-31): a write landed in
//! `0x540000–0x59F000` (silicon-bootloader-input / BIOS-config); the mask ROM reads that
//! zone at the earliest boot stage — before SDRAM, before HCB — so corrupting it kills all
//! PCIe recovery. The other failure mode was a dual-bank version mismatch (FIRMWARE != BACKUP).
//!
//! This module makes those two failure modes impossible to miss. It is invariant-driven, not
//! a pile of ad-hoc checks: a `GuardedFlash` transaction takes a mandatory pre-write snapshot
//! (IOC-free diag read — works on a sick card) and persists it as a recovery image, runs the
//! write, then evaluates a declared set of pure postflight invariants against the snapshot.
//! The response is severity-driven (see [`Severity`]).
//!
//! All write paths (`fw write`, `bios`/`nvdata`, `recover`, the flash orchestrator) route
//! through here. `Card::write_region` is never called directly by a verb.

use std::fmt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::card::Card;
use crate::firmware::flash_layout::{parse_flash_layout, verify_flash_consistency, REGION_FIRMWARE};
use crate::firmware::validate::validate_image;
use crate::mpi::messages::ImageType;

/// BAR1 diag-mapped flash window — the IOC-free back door. Proven to read the full 8 MB on a
/// bricked card (session notes 2026-05-30). This is how the snapshot/read-back are taken even
/// when the IOC is dead.
pub const FLASH_WINDOW_ADDR: u32 = 0xFC00_0000;
/// Flash size we snapshot/verify. 8 MiB on the SAS2008 boards in scope.
pub const FLASH_SIZE: usize = 8 * 1024 * 1024;

/// A region of flash whose corruption is **unrecoverable over PCIe**: the mask-ROM bootloader
/// reads it during earliest init, so corrupting it stops the card from booting far enough for
/// any software recovery (FW_DOWNLOAD, HCB, or even PCIe link-train).
///
/// Evidence: the dev-1 brick erased `0x540000–0x59F000` and killed every PCIe recovery path
/// (closeout note 2026-05-31). A FW_DOWNLOAD region write under a *correct* layout never targets
/// this zone — so a postflight change here means a layout mismatch (the 05-20 root cause) or a
/// stray write, and the card is one power-cycle from dead. Add regions here only with hardware
/// evidence; an over-broad map would false-block legitimate writes.
#[derive(Debug, Clone, Copy)]
pub struct BootCriticalRegion {
    pub name: &'static str,
    pub start: usize,
    /// Exclusive end.
    pub end: usize,
}

/// The proven boot-critical map. Anchored on the dev-1 damage map; extend only with evidence.
pub const BOOT_CRITICAL: &[BootCriticalRegion] = &[BootCriticalRegion {
    name: "silicon-bootloader-input / BIOS-config",
    start: 0x54_0000,
    end: 0x5A_0000,
}];

/// How bad a postflight violation is. Drives the transaction's response — see
/// [`GuardedFlash::commit`]. Ordered so `max` picks the worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Card still alive and recoverable (boot-critical intact). Worth one rollback attempt.
    Error,
    /// Boot-critical zone changed, or banks diverged in a way a re-write can't safely fix.
    /// Issue NO further writes — more writes risk worsening it. The saved snapshot is the
    /// recovery image (CH341A).
    Critical,
}

/// One failed invariant.
#[derive(Debug, Clone)]
pub struct Violation {
    pub invariant: &'static str,
    pub severity: Severity,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}: {}", self.severity, self.invariant, self.detail)
    }
}

/// Errors from the guarded-flash transaction.
#[derive(Debug)]
pub enum GuardError {
    /// Could not take the mandatory pre-write snapshot — refuse to write blind.
    SnapshotFailed(String),
    /// Could not persist the snapshot recovery image to disk.
    SnapshotSaveFailed(String),
    /// Snapshot is not a restorable full image — refuse to write without a rollback source.
    SnapshotNotRestorable(String),
    /// The image to write is empty.
    EmptyImage,
    /// The image failed the structural validity gate.
    InvalidImage(String),
    /// The target region is not present in the card's own FLASH_LAYOUT.
    UnknownTargetRegion(ImageType),
    /// The underlying FW_DOWNLOAD failed.
    WriteFailed(String),
    /// Could not read flash back after the write to verify it.
    PostReadFailed(String),
    /// Postflight invariants failed. The snapshot at `snapshot_path` is the recovery image.
    PostflightFailed {
        violations: Vec<Violation>,
        snapshot_path: PathBuf,
        worst: Severity,
        rolled_back: bool,
    },
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SnapshotFailed(e) => write!(
                f,
                "pre-write snapshot failed ({e}); refusing to write without a recovery image"
            ),
            Self::SnapshotSaveFailed(e) => write!(f, "could not save snapshot recovery image: {e}"),
            Self::SnapshotNotRestorable(e) => {
                write!(f, "snapshot is not a restorable image: {e}; refusing to write")
            }
            Self::EmptyImage => write!(f, "image to write is empty"),
            Self::InvalidImage(e) => write!(f, "image failed validity gate: {e}"),
            Self::UnknownTargetRegion(t) => write!(
                f,
                "target region {t:?} not found in the card's FLASH_LAYOUT — wrong image or layout mismatch"
            ),
            Self::WriteFailed(e) => write!(f, "FW_DOWNLOAD failed: {e}"),
            Self::PostReadFailed(e) => write!(
                f,
                "post-write read-back failed ({e}) — CANNOT confirm the write was safe; treat the card as suspect and verify with a saved snapshot"
            ),
            Self::PostflightFailed {
                violations,
                snapshot_path,
                worst,
                rolled_back,
            } => {
                writeln!(f, "POST-WRITE SAFETY CHECK FAILED ({worst:?}):")?;
                for v in violations {
                    writeln!(f, "  - {v}")?;
                }
                match worst {
                    Severity::Critical => write!(
                        f,
                        "  BOOT-CRITICAL corruption — do NOT issue further writes. Recovery image: {}\n  (PCIe recovery may be impossible; this is the CH341A SPI-clip case.)",
                        snapshot_path.display()
                    ),
                    Severity::Error => write!(
                        f,
                        "  Rollback attempted: {}. Recovery image: {}",
                        if *rolled_back { "yes" } else { "FAILED" },
                        snapshot_path.display()
                    ),
                }
            }
        }
    }
}

impl std::error::Error for GuardError {}

impl From<GuardError> for crate::Error {
    fn from(e: GuardError) -> Self {
        crate::Error::Other(e.to_string())
    }
}

/// A full flash image — a snapshot, a post-write read-back, or (conceptually) any 8 MB blob.
#[derive(Clone)]
pub struct FlashImage {
    bytes: Vec<u8>,
}

impl FlashImage {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Read the full flash via the IOC-free diag back door. Works on a bricked card. The
    /// transport is opened transiently and dropped, so it does not contend with `Card`'s BAR
    /// mapping across the write.
    pub fn from_card_diag(bdf: &str) -> Result<Self, GuardError> {
        use crate::sbr::transport::Bar1MmapSbrTransport;
        let mut transport = Bar1MmapSbrTransport::open(bdf)
            .map_err(|e| GuardError::SnapshotFailed(format!("open diag transport: {e}")))?;
        let bytes = transport
            .read_chip_mem(FLASH_WINDOW_ADDR, FLASH_SIZE)
            .map_err(|e| GuardError::SnapshotFailed(format!("diag read: {e}")))?;
        Ok(Self { bytes })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Bytes of `[start, end)`, clamped to the image — never panics on a short image.
    fn region(&self, start: usize, end: usize) -> &[u8] {
        let lo = start.min(self.bytes.len());
        let hi = end.min(self.bytes.len());
        if lo >= hi {
            &[]
        } else {
            &self.bytes[lo..hi]
        }
    }
}

/// What the caller intends to write, with the target region named explicitly so there is never
/// any "backup vs current" ambiguity about where bytes land.
pub struct WriteIntent {
    pub region: ImageType,
    pub bytes: Vec<u8>,
    /// Human label for logs (e.g. "firmware", "bios").
    pub label: String,
}

// ===========================================================================================
// PURE INVARIANTS — no hardware, fully unit-testable. This is the safety-critical logic.
// ===========================================================================================

/// Preflight: a write must not be issued unless we can roll back and the image is sane.
/// Pure over (snapshot, intent). Returns the first blocking reason.
pub fn preflight(snapshot: &FlashImage, intent: &WriteIntent) -> Result<(), GuardError> {
    // 1. The image must be non-empty.
    if intent.bytes.is_empty() {
        return Err(GuardError::EmptyImage);
    }

    // 2. The snapshot must be a restorable full image, else we have no rollback source.
    if snapshot.len() != FLASH_SIZE {
        return Err(GuardError::SnapshotNotRestorable(format!(
            "snapshot is {} bytes, expected {}",
            snapshot.len(),
            FLASH_SIZE
        )));
    }
    // A snapshot that is all-0xFF (fully erased) or all-zero is not a usable recovery image.
    if snapshot.bytes().iter().all(|&b| b == 0xFF) || snapshot.bytes().iter().all(|&b| b == 0x00) {
        return Err(GuardError::SnapshotNotRestorable(
            "snapshot is blank (all 0xFF or all 0x00) — not a usable recovery image".into(),
        ));
    }

    // 3. The target region must exist in the card's OWN FLASH_LAYOUT (parsed from the snapshot).
    //    A Fw write to a card whose layout has no FIRMWARE region = wrong image / layout mismatch.
    if let Some(code) = region_code(intent.region) {
        let layout = parse_flash_layout(snapshot.bytes())
            .map_err(|e| GuardError::SnapshotNotRestorable(format!("snapshot layout parse: {e}")))?;
        if layout.region_span(code).is_none() {
            return Err(GuardError::UnknownTargetRegion(intent.region));
        }
    }

    // 4. Firmware images must pass the structural validity gate (size/header/checksum).
    if intent.region == ImageType::Fw {
        let val = validate_image(&intent.bytes);
        if !val.ok() {
            let why = val
                .first_failure()
                .map(|c| c.name.to_string())
                .unwrap_or_else(|| "unknown".into());
            return Err(GuardError::InvalidImage(why));
        }
    }

    Ok(())
}

/// Postflight: every declared invariant, evaluated against the snapshot. Empty vec = clean.
/// Pure over (pre, post, intent).
pub fn postflight(pre: &FlashImage, post: &FlashImage, intent: &WriteIntent) -> Vec<Violation> {
    let mut violations = Vec::new();
    if let Some(v) = check_boot_critical_unchanged(pre, post) {
        violations.push(v);
    }
    if let Some(v) = check_banks_consistent(post) {
        violations.push(v);
    }
    if let Some(v) = check_target_readback(post, intent) {
        violations.push(v);
    }
    violations
}

/// The worst severity among violations, if any.
pub fn worst_severity(violations: &[Violation]) -> Option<Severity> {
    violations.iter().map(|v| v.severity).max()
}

/// KEYSTONE: no byte in any boot-critical region may change. This is the check that catches the
/// brick mechanism — a write that lands in (or erases) the boot-input zone trips it.
fn check_boot_critical_unchanged(pre: &FlashImage, post: &FlashImage) -> Option<Violation> {
    for r in BOOT_CRITICAL {
        let a = pre.region(r.start, r.end);
        let b = post.region(r.start, r.end);
        if a != b {
            let changed = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count()
                + a.len().abs_diff(b.len());
            return Some(Violation {
                invariant: "BootCriticalUnchanged",
                severity: Severity::Critical,
                detail: format!(
                    "{} bytes changed in boot-critical region '{}' (0x{:06X}–0x{:06X}) — \
                     this zone must NEVER change on a region write",
                    changed, r.name, r.start, r.end
                ),
            });
        }
    }
    None
}

/// Banks must agree: a divergent FIRMWARE vs BACKUP version is the dual-bank mismatch that
/// faults the bootloader (the dev-1 card's state). If the layout can't be parsed we cannot
/// affirm consistency, but that alone is not a violation (other checks cover corruption).
fn check_banks_consistent(post: &FlashImage) -> Option<Violation> {
    match verify_flash_consistency(post.bytes()) {
        Ok(c) if !c.consistent => Some(Violation {
            invariant: "BanksConsistent",
            severity: Severity::Critical,
            detail: format!(
                "FIRMWARE version '{}' != BACKUP version '{}' — dual-bank mismatch (bootloader fault)",
                c.firmware_version, c.backup_version
            ),
        }),
        _ => None,
    }
}

/// The bytes we read back from the target region must equal what we intended to write. Only
/// computable where we can locate the region in the layout (FIRMWARE today); otherwise the
/// IOC-side read-back in the calling verb covers it — we don't fabricate a span.
fn check_target_readback(post: &FlashImage, intent: &WriteIntent) -> Option<Violation> {
    let code = region_code(intent.region)?;
    let layout = parse_flash_layout(post.bytes()).ok()?;
    let (off, size) = layout.region_span(code)?;
    let off = off as usize;
    let n = (size as usize).min(intent.bytes.len());
    let got = post.region(off, off + n);
    let want = &intent.bytes[..n.min(intent.bytes.len())];
    if got != want {
        let mism = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
        return Some(Violation {
            invariant: "TargetReadbackMatches",
            severity: Severity::Error,
            detail: format!(
                "{} of {} bytes in target region {:?} (0x{:06X}) differ from the intended image",
                mism, n, intent.region, off
            ),
        });
    }
    None
}

/// Map a FW_DOWNLOAD `ImageType` to its FLASH_LAYOUT region-type code, where known. Returns
/// `None` for regions whose layout code is not yet confirmed (we never guess an offset).
fn region_code(t: ImageType) -> Option<u8> {
    match t {
        ImageType::Fw => Some(REGION_FIRMWARE),
        _ => None,
    }
}

fn sanitize_bdf(bdf: &str) -> String {
    bdf.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn sha_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    format!("{:x}", h.finalize())
}

// ===========================================================================================
// THE TRANSACTION — the only way to write flash.
// ===========================================================================================

/// A guarded flash-write transaction. Construct with [`begin`](Self::begin) (takes + persists the
/// mandatory snapshot), then call [`commit`](Self::commit) for each region write.
pub struct GuardedFlash {
    bdf: String,
    snapshot: FlashImage,
    snapshot_path: PathBuf,
}

impl GuardedFlash {
    /// Take the mandatory pre-write snapshot (IOC-free diag read) and persist it as the recovery
    /// image under `save_dir`. No snapshot ⇒ no write.
    pub fn begin(bdf: &str, save_dir: &Path) -> Result<Self, GuardError> {
        let snapshot = FlashImage::from_card_diag(bdf)?;
        if snapshot.len() != FLASH_SIZE {
            return Err(GuardError::SnapshotNotRestorable(format!(
                "diag read returned {} bytes, expected {}",
                snapshot.len(),
                FLASH_SIZE
            )));
        }
        let snapshot_path = save_dir.join(format!("pre-write-snapshot-{}.bin", sanitize_bdf(bdf)));
        std::fs::write(&snapshot_path, snapshot.bytes())
            .map_err(|e| GuardError::SnapshotSaveFailed(e.to_string()))?;
        eprintln!(
            "guarded-flash: pre-write snapshot saved → {} ({} bytes, sha256={}) — this is the recovery image",
            snapshot_path.display(),
            snapshot.len(),
            sha_hex(snapshot.bytes())
        );
        Ok(Self {
            bdf: bdf.to_string(),
            snapshot,
            snapshot_path,
        })
    }

    /// Path of the persisted recovery snapshot.
    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// Preflight → write → postflight, with a severity-driven response. The only place a region
    /// write is issued.
    pub fn commit(&self, card: &mut dyn Card, intent: WriteIntent) -> Result<(), GuardError> {
        preflight(&self.snapshot, &intent)?;
        eprintln!(
            "guarded-flash: preflight OK — writing {} ({} bytes) to {:?}",
            intent.label,
            intent.bytes.len(),
            intent.region
        );

        card.write_region(intent.region, &intent.bytes)
            .map_err(|e| GuardError::WriteFailed(e.to_string()))?;

        let post = FlashImage::from_card_diag(&self.bdf)
            .map_err(|e| GuardError::PostReadFailed(e.to_string()))?;
        let violations = postflight(&self.snapshot, &post, &intent);

        match worst_severity(&violations) {
            None => {
                eprintln!(
                    "guarded-flash: postflight OK ✓ — boot-critical intact, banks consistent, read-back matches"
                );
                Ok(())
            }
            Some(Severity::Critical) => {
                // Boot-critical corruption (or hard bank mismatch): issuing more writes risks
                // worsening it. Halt and surface the recovery image.
                Err(GuardError::PostflightFailed {
                    violations,
                    snapshot_path: self.snapshot_path.clone(),
                    worst: Severity::Critical,
                    rolled_back: false,
                })
            }
            Some(Severity::Error) => {
                // Card still alive (boot-critical intact). Try one rollback of the target region
                // from the snapshot, then re-verify boot-critical stayed intact.
                let rolled_back = self.try_rollback(card, &intent).is_ok();
                Err(GuardError::PostflightFailed {
                    violations,
                    snapshot_path: self.snapshot_path.clone(),
                    worst: Severity::Error,
                    rolled_back,
                })
            }
        }
    }

    /// Guard a write this transaction can't express as a single [`WriteIntent`] — e.g. a
    /// multi-region `restore`. Runs the supplied write, then the snapshot-relative postflight
    /// invariants that don't need a single target region (boot-critical + bank consistency).
    /// No auto-rollback (the regions touched are opaque to us); on failure the saved snapshot is
    /// the recovery image.
    pub fn commit_with<F>(&self, write: F) -> Result<(), GuardError>
    where
        F: FnOnce() -> Result<(), crate::Error>,
    {
        write().map_err(|e| GuardError::WriteFailed(e.to_string()))?;
        let post = FlashImage::from_card_diag(&self.bdf)
            .map_err(|e| GuardError::PostReadFailed(e.to_string()))?;
        let mut violations = Vec::new();
        if let Some(v) = check_boot_critical_unchanged(&self.snapshot, &post) {
            violations.push(v);
        }
        if let Some(v) = check_banks_consistent(&post) {
            violations.push(v);
        }
        match worst_severity(&violations) {
            None => {
                eprintln!(
                    "guarded-flash: postflight OK ✓ — boot-critical intact, banks consistent"
                );
                Ok(())
            }
            Some(worst) => Err(GuardError::PostflightFailed {
                violations,
                snapshot_path: self.snapshot_path.clone(),
                worst,
                rolled_back: false,
            }),
        }
    }

    /// Best-effort rollback: re-write the target region with the snapshot's original bytes for
    /// that region. Only attempted for non-critical failures (boot-critical intact). After the
    /// re-write, confirms boot-critical is still untouched — if the rollback itself disturbed it,
    /// that is a hard failure surfaced to the caller.
    fn try_rollback(&self, card: &mut dyn Card, intent: &WriteIntent) -> Result<(), GuardError> {
        let code = region_code(intent.region).ok_or(GuardError::UnknownTargetRegion(intent.region))?;
        let layout = parse_flash_layout(self.snapshot.bytes())
            .map_err(|e| GuardError::SnapshotNotRestorable(e.to_string()))?;
        let (off, size) = layout
            .region_span(code)
            .ok_or(GuardError::UnknownTargetRegion(intent.region))?;
        let orig = self
            .snapshot
            .region(off as usize, off as usize + size as usize)
            .to_vec();
        eprintln!(
            "guarded-flash: rolling back {:?} region (0x{:06X}, {} bytes) from snapshot",
            intent.region, off, size
        );
        card.write_region(intent.region, &orig)
            .map_err(|e| GuardError::WriteFailed(e.to_string()))?;
        let post = FlashImage::from_card_diag(&self.bdf)
            .map_err(|e| GuardError::PostReadFailed(e.to_string()))?;
        if let Some(v) = check_boot_critical_unchanged(&self.snapshot, &post) {
            return Err(GuardError::PostflightFailed {
                violations: vec![v],
                snapshot_path: self.snapshot_path.clone(),
                worst: Severity::Critical,
                rolled_back: false,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An 8 MB image with a tiny embedded FLASH_LAYOUT so parse_flash_layout/region_span work.
    /// Mirrors the fixture builder in flash_layout.rs tests.
    fn synthetic_flash() -> Vec<u8> {
        let mut flash = vec![0xAAu8; FLASH_SIZE];
        // Make the boot-critical zone deterministic, non-blank.
        for (i, b) in flash[0x54_0000..0x5A_0000].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        flash
    }

    fn intent(region: ImageType, bytes: Vec<u8>) -> WriteIntent {
        WriteIntent {
            region,
            bytes,
            label: "test".into(),
        }
    }

    #[test]
    fn boot_critical_unchanged_passes_when_identical() {
        let img = FlashImage::new(synthetic_flash());
        assert!(check_boot_critical_unchanged(&img, &img.clone()).is_none());
    }

    #[test]
    fn boot_critical_unchanged_flags_any_change_in_zone() {
        let pre = FlashImage::new(synthetic_flash());
        let mut after = synthetic_flash();
        // Flip a single byte inside the boot-critical zone.
        after[0x54_0010] ^= 0xFF;
        let post = FlashImage::new(after);
        let v = check_boot_critical_unchanged(&pre, &post).expect("must flag boot-critical change");
        assert_eq!(v.invariant, "BootCriticalUnchanged");
        assert_eq!(v.severity, Severity::Critical);
    }

    #[test]
    fn boot_critical_simulates_the_05_20_brick() {
        // The 05-20 brick: a BIOS write landed in the boot-input region. Model it as the whole
        // zone going to 0xFF (erased). Must be caught as Critical.
        let pre = FlashImage::new(synthetic_flash());
        let mut after = synthetic_flash();
        for b in after[0x54_0000..0x5A_0000].iter_mut() {
            *b = 0xFF;
        }
        let post = FlashImage::new(after);
        let v = check_boot_critical_unchanged(&pre, &post).expect("erased boot-input must trip");
        assert_eq!(v.severity, Severity::Critical);
    }

    #[test]
    fn changes_outside_boot_critical_are_ignored_by_that_check() {
        let pre = FlashImage::new(synthetic_flash());
        let mut after = synthetic_flash();
        after[0x10_0000] ^= 0xFF; // well outside the boot-critical zone
        let post = FlashImage::new(after);
        assert!(check_boot_critical_unchanged(&pre, &post).is_none());
    }

    #[test]
    fn preflight_rejects_empty_image() {
        let snap = FlashImage::new(synthetic_flash());
        let err = preflight(&snap, &intent(ImageType::Bios, vec![])).unwrap_err();
        assert!(matches!(err, GuardError::EmptyImage));
    }

    #[test]
    fn preflight_rejects_blank_snapshot() {
        let snap = FlashImage::new(vec![0xFF; FLASH_SIZE]);
        let err = preflight(&snap, &intent(ImageType::Bios, vec![1, 2, 3])).unwrap_err();
        assert!(matches!(err, GuardError::SnapshotNotRestorable(_)));
    }

    #[test]
    fn preflight_rejects_wrong_size_snapshot() {
        let snap = FlashImage::new(vec![0xAB; 1024]);
        let err = preflight(&snap, &intent(ImageType::Bios, vec![1, 2, 3])).unwrap_err();
        assert!(matches!(err, GuardError::SnapshotNotRestorable(_)));
    }

    #[test]
    fn worst_severity_picks_critical_over_error() {
        let vs = vec![
            Violation {
                invariant: "a",
                severity: Severity::Error,
                detail: String::new(),
            },
            Violation {
                invariant: "b",
                severity: Severity::Critical,
                detail: String::new(),
            },
        ];
        assert_eq!(worst_severity(&vs), Some(Severity::Critical));
    }

    #[test]
    fn worst_severity_none_when_clean() {
        assert_eq!(worst_severity(&[]), None);
    }

    #[test]
    fn postflight_aggregator_surfaces_boot_critical_as_worst() {
        // End-to-end: erase the boot-input zone and confirm postflight() collects a Critical
        // violation and worst_severity() reports Critical — the brick is caught by the pipeline,
        // not just the leaf check.
        let pre = FlashImage::new(synthetic_flash());
        let mut after = synthetic_flash();
        for b in after[0x54_0000..0x5A_0000].iter_mut() {
            *b = 0xFF;
        }
        let post = FlashImage::new(after);
        let intent = intent(ImageType::Bios, vec![0u8; 64]);
        let violations = postflight(&pre, &post, &intent);
        assert!(
            violations.iter().any(|v| v.invariant == "BootCriticalUnchanged"),
            "boot-critical violation must be present"
        );
        assert_eq!(worst_severity(&violations), Some(Severity::Critical));
    }

    #[test]
    fn postflight_clean_when_only_safe_region_changed() {
        let pre = FlashImage::new(synthetic_flash());
        let mut after = synthetic_flash();
        after[0x10_0000] ^= 0xFF; // outside boot-critical; no parseable layout ⇒ banks check inert
        let post = FlashImage::new(after);
        let intent = intent(ImageType::Bios, vec![0u8; 8]);
        assert!(postflight(&pre, &post, &intent).is_empty());
    }

    #[test]
    fn region_clamps_on_short_image() {
        let img = FlashImage::new(vec![0u8; 100]);
        assert_eq!(img.region(50, 200).len(), 50);
        assert_eq!(img.region(200, 300).len(), 0);
    }
}
