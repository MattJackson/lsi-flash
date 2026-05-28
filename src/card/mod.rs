//! Card trait scaffold — pluggable abstraction over flash-capable cards.
//!
//! Implements ADR-017: Card trait and pluggable transport layer
//! (see `/Users/mjackson/Developer/lsi-flash-notes/01-architecture/adr/017-card-trait-and-pluggable-transport.md`).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub mod mpt;
pub mod tests;

pub use mpt::MptCard;

use std::path::Path;

// Re-export Personality for convenient access at the card module level
#[allow(unused_imports)]
pub use crate::mpi::session::Personality;

/// SAS chip family enum — maps VID:DID to chip families per card-database.toml.
///
/// Card database entries (src/card_database.rs) define:
/// - Sas2008: LSI 9211-8i, Dell H200/H310, IBM M1015, Fujitsu D2607 (all DID=0x0072)
/// - Sas2208/Sas3008: Future targets (SAS2208 DID=0x0084, SAS3008 DID=0x00C0) — OUT OF SCOPE
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ChipFamily {
    Sas2008,
    Sas2208,
    Sas3008,
    Unknown,
}

/// Identity of a flash-capable card — populated at discovery time, never changes.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CardIdentity {
    pub bdf: String,    // "0000:03:00.0"
    pub vendor_id: u16, // 0x1000
    pub device_id: u16, // 0x0072
    pub subsystem_vid: Option<u16>,
    pub subsystem_did: Option<u16>,
    pub chip_family: ChipFamily,
    pub friendly_name: Option<String>, // from card-database lookup
}

/// Error type for Card operations. Follows the project's thiserror pattern (src/error.rs).
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum CardError {
    #[error("no cards found on PCI bus")]
    NoCardsFound,

    #[error("unsupported card: VID:DID {0:04x}:{1:04x}")]
    UnsupportedCard(u16, u16),

    #[error("pci enumeration: {0}")]
    PciEnumeration(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("transport: {0}")]
    Transport(String), // wraps future MptTransport / MfiTransport errors

    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),
}

/// Report from Card::detect — mirrors fields emitted by cli/detect.rs.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DetectReport {
    /// BDF of the card (e.g., "0000:03:00.0")
    pub bdf: String,
    /// Chip family derived from VID:DID lookup
    pub chip_family: ChipFamily,
}

/// Report from Card::backup — mirrors BackupManifest shape from cli/backup.rs.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BackupReport {
    /// Timestamp of backup in RFC3339 format
    pub timestamp: String,
    /// Artifacts written (firmware.bin, bios.rom, nvdata.bin)
    pub artifacts_count: usize,
    /// List of artifacts with their metadata for reporting
    pub artifacts: Vec<BackupArtifact>,
}

/// Artifact information from Card::backup — mirrors BackupArtifact in cli/backup.rs.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupArtifact {
    pub path: String,
    pub image_type: String,
    pub sha256: String,
    pub size: u64,
}

/// Top-level trait for flash-capable cards.
///
/// Per ADR-017, CLI verbs speak through this trait uniformly, enabling future
/// chip families (MfiCard for MegaRAID, NvmeCard, etc.) to plug in without
/// rewriting verbs. Each impl decides its own transport strategy.
pub trait Card: Send {
    /// Identity of this card — populated at discovery time, never changes after.
    fn identity(&self) -> &CardIdentity;

    /// Run detect — sysfs + chip-state probe. Read-only. Brick-safe.
    fn detect(&mut self) -> Result<DetectReport, CardError>;

    /// Capture full backup per ADR-015 Rule 10 (FW + BIOS + NVDATA + SBR + Mfg pages).
    /// Read-only. Brick-safe.
    fn backup(&mut self, out_dir: &Path) -> Result<BackupReport, CardError>;

    /// Query the chip's currently-running personality (IT / IR / IMR).
    /// Read-only. Brick-safe.
    fn current_personality(&mut self) -> Result<Personality, CardError>;

    // NOTE: flash(), recover(), sbr_read(), sbr_write() land in a follow-up.
    // Scope this cycle to detect + backup + current_personality so a freshman
    // can complete + ship in one session.
}

/// Discover all supported cards on the PCI bus.
///
/// Walks `/sys/bus/pci/devices/` via `crate::pci::Platform` (LinuxSysfs in prod).
/// For each device, reads vendor/device IDs and dispatches to MptCard::discover_one()
/// per BDF found via `pci::discover_sas2008_devices_linux()`. Cards that can't be
/// opened (mpt3sas not loaded) are skipped.
pub fn discover() -> Result<Vec<Box<dyn Card>>, CardError> {
    let devs = crate::pci::discover_sas2008_devices_linux()
        .map_err(|e| CardError::PciEnumeration(format!("{}", e)))?;

    let mut cards: Vec<Box<dyn Card>> = Vec::new();
    for dev in devs {
        match MptCard::discover_one(&dev.bdf) {
            Ok(card) => cards.push(Box::new(card)),
            Err(_) => continue, // skip cards we can't talk to (mpt3sas not loaded etc.)
        }
    }

    if cards.is_empty() {
        Err(CardError::NoCardsFound)
    } else {
        Ok(cards)
    }
}

/// Discover a single card by BDF (used by --pci <bdf>).
///
/// Like `discover()` but for a specific BDF. Returns the right Card impl based on
/// VID:DID lookup, or an error if the device doesn't exist or isn't supported.
pub fn discover_one(bdf: &str) -> Result<Box<dyn Card>, CardError> {
    Ok(Box::new(MptCard::discover_one(bdf)?))
}
