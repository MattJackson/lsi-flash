//! Card trait scaffold — pluggable abstraction over flash-capable cards.
//!
//! Implements ADR-017: Card trait and pluggable transport layer
//! (see `/Users/mjackson/Developer/lsi-flash-notes/01-architecture/adr/017-card-trait-and-pluggable-transport.md`).

#![allow(dead_code)]

pub mod tests;

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
    // TODO: senior to flesh out with full manifest fields per ADR-015 Rule 10
    // Fields to add later: sas_wwn, artifacts[].{path,image_type,sha256,size}, source_card info
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
/// For each device, reads vendor/device IDs and dispatches to the right Card impl
/// by VID:DID lookup against the card database. Today only MptCard is planned for
/// SAS2008/SAS2208/SAS3008 chips, but since MptCard doesn't exist yet, this returns
/// `CardError::NotImplemented("MptCard")` for each discovered device.
///
/// The senior follow-up plugs in the MptCard impl here — this scaffold-only cycle
/// ensures the contract is stable so future cycles can build against it.
pub fn discover() -> Result<Vec<Box<dyn Card>>, CardError> {
    let platform = crate::pci::LinuxSysfs;

    // Use existing PCI enumeration path from pci.rs
    let devices = crate::pci::discover_sas2008_devices(&platform)
        .map_err(|e| CardError::PciEnumeration(e.to_string()))?;

    if devices.is_empty() {
        return Err(CardError::NoCardsFound);
    }

    // For each discovered device, attempt to build the right Card impl.
    // Since MptCard doesn't exist yet, we return NotImplemented for all entries.
    let num_devices = devices.len();
    #[allow(unused_variables)]
    for dev in &devices {
        // Look up chip family from VID:DID via card_database module
        let _chip_family = match (dev.vendor_id, dev.device_id) {
            (0x1000, 0x0072) => ChipFamily::Sas2008,
            (0x1000, 0x0084) => ChipFamily::Sas2208, // Future target
            (0x1000, 0x00C0) => ChipFamily::Sas3008, // Future target
            _ => {
                // Check subsystem IDs for known variants
                let card_db = crate::card_database::load_embedded()
                    .map_err(|e| CardError::PciEnumeration(e.to_string()))?;

                if let Some(info) = crate::card_database::identify_card(
                    &card_db,
                    dev.vendor_id,
                    dev.device_id,
                    dev.subsystem_vendor_id,
                    dev.subsystem_device_id,
                ) {
                    // Map card-database ChipFamily to our Card module enum
                    match info.chip_family {
                        crate::card_database::ChipFamily::Sas2008 => ChipFamily::Sas2008,
                        crate::card_database::ChipFamily::Sas2108
                        | crate::card_database::ChipFamily::Sas2208
                        | crate::card_database::ChipFamily::Sas2308 => ChipFamily::Sas2208,
                        crate::card_database::ChipFamily::Sas3008 => ChipFamily::Sas3008,
                        crate::card_database::ChipFamily::Unknown => ChipFamily::Unknown,
                    }
                } else {
                    ChipFamily::Unknown
                }
            }
        };

        // TODO: senior follow-up — construct MptCard here based on chip_family
        // For now, we just iterate over devices to consume them
    }

    // Since we can't push errors into Vec<Box<dyn Card>>, return NotImplemented error
    if num_devices == 0 {
        Err(CardError::NoCardsFound)
    } else {
        // Return NotImplemented to lock the scaffold-only behavior
        Err(CardError::NotImplemented("MptCard"))
    }
}

/// Discover a single card by BDF (used by --pci <bdf>).
///
/// Like `discover()` but for a specific BDF. Returns the right Card impl based on
/// VID:DID lookup, or an error if the device doesn't exist or isn't supported.
pub fn discover_one(_bdf: &str) -> Result<Box<dyn Card>, CardError> {
    // TODO: senior follow-up — implement single-card discovery with BDF-specific lookup
    // For now, return NotImplemented to match discover() behavior
    Err(CardError::NotImplemented("discover_one"))
}
