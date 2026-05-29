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

    #[error("specify which card with --pci <bdf> (run `lsi-flash detect` to list cards)")]
    PciRequired,

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

/// Report from Card::restore — mirrors fields needed for restore reporting.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RestoreReport {
    /// Timestamp of restore in RFC3339 format
    pub timestamp: String,
    /// Number of regions written back to the card
    pub regions_written: usize,
    /// List of artifacts that were restored with their metadata
    pub regions: Vec<String>,
}

/// Result of a raw TOOLBOX_CLEAN probe — the firmware's exact reply.
#[derive(Debug, Clone)]
pub struct EraseReport {
    /// TOOLBOX_CLEAN flags that were sent (e.g. CLEAN_FLASH = 0x04).
    pub flags_sent: u32,
    /// IOCStatus from the reply (0x0E), masked of the LOG_INFO flag bit.
    pub ioc_status: u16,
    /// True iff ioc_status == 0x0000 (firmware accepted/executed the clean).
    pub success: bool,
    /// IOCLogInfo from the reply (0x10) — vendor diagnostic word; nonzero often
    /// pinpoints why a command was rejected.
    pub ioc_log_info: u32,
    /// First 24 bytes of the raw reply, hex — for forensic inspection.
    pub raw_reply_hex: String,
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

    /// Read the 256-byte SBR (subsystem boot record) from the card's
    /// I2C EEPROM via TOOLBOX_ISTWI transport. Returns raw bytes; caller
    /// parses via `sbr::parse::parse_sbr`. Default impl returns NotImplemented
    /// so each Card impl can opt in.
    fn sbr_read(&mut self) -> Result<[u8; 256], CardError> {
        Err(CardError::NotImplemented("sbr_read"))
    }

    /// Write the 256-byte SBR to the card's I2C EEPROM. Destructive (changes PCI
    /// identity at next reset). Default impl returns NotImplemented so each Card
    /// opts in. Callers MUST back up the current SBR first (recoverable via
    /// `sbr write` of the backup, or CH341A as last resort).
    fn sbr_write(&mut self, _data: &[u8; 256]) -> Result<(), CardError> {
        Err(CardError::NotImplemented("sbr_write"))
    }

    /// Send a raw MPI TOOLBOX_CLEAN and return the firmware's exact reply
    /// (IOCStatus + IOCLogInfo). DESTRUCTIVE if the running firmware honors it
    /// (erases the flash). Diagnostic purpose: capture the real reply that
    /// sas2flsh/lsiutil hide behind exit-0. Default impl returns NotImplemented.
    fn erase_flash(&mut self, _wipe_mfg_pages: bool) -> Result<EraseReport, CardError> {
        Err(CardError::NotImplemented("erase_flash"))
    }

    /// Write a previously-captured backup's firmware regions back to THIS card
    /// via FW_DOWNLOAD. Destructive. Per ADR-015 Rule 8 (non-destructive
    /// round-trip), restoring the same OEM firmware is the safe first write test.
    /// Default impl returns NotImplemented so each Card opts in.
    fn restore(&mut self, backup_dir: &Path) -> Result<RestoreReport, CardError> {
        let _ = backup_dir;
        Err(CardError::NotImplemented("restore"))
    }

    // NOTE: flash(), recover(), sbr_write() land in a follow-up.
    // Scope this cycle to detect + backup + current_personality so a freshman
    // can complete + ship in one session.
}

/// Discover all supported cards on the PCI bus.
///
/// Walks `/sys/bus/pci/devices/` via `crate::pci::Platform` (LinuxSysfs in prod).
/// For each device, reads vendor/device IDs and dispatches to MptCard::discover_one()
/// per BDF found via `pci::discover_sas2008_devices_linux()`. Cards that can't be
/// opened (mpt3sas not loaded) are skipped.
/// Sentinel BDF that EXPLICITLY selects the mock backend. It is a syntactically
/// impossible PCI address (real BDF is `domain:bus:dev.func`, all hex ≥ 0), so it
/// can never collide with a real card and is unmistakable in any output. The mock
/// is ONLY reachable via this sentinel — a hardware verb never silently falls back
/// to mock. Output/manifests produced under mock are stamped with this BDF.
pub const MOCK_BDF: &str = "-1:-1:-1:-1";

/// True if `bdf` is the explicit mock sentinel.
pub fn is_mock_bdf(bdf: &str) -> bool {
    bdf == MOCK_BDF
}

/// Resolve which card a verb operates on. `--pci` is REQUIRED for every
/// card-targeting verb — there is NO auto-detect. Discovery is `detect`'s job
/// (it lists every card); you then pass the BDF you saw via `--pci`. This keeps
/// one consistent rule and removes any "it picked the wrong card" surprise,
/// especially for destructive ops.
///
/// - `Some(MOCK_BDF)` → the sentinel (caller routes to the mock + stamps output).
/// - `Some(real_bdf)` → returned verbatim.
/// - `None` → `PciRequired` (points the user at `detect`).
///
/// NEVER assumes mock and never guesses a target.
pub fn resolve_bdf(pci: Option<&str>) -> Result<String, CardError> {
    match pci {
        // `--pci -` reads the BDF from stdin (first whitespace-delimited token),
        // enabling pipelines: `lsi-flash detect --bdf | head -1 | lsi-flash <verb> --pci -`.
        Some("-") => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(CardError::Io)?;
            s.split_whitespace()
                .next()
                .map(|t| t.to_string())
                .ok_or(CardError::PciRequired)
        }
        Some(bdf) => Ok(bdf.to_string()),
        None => Err(CardError::PciRequired),
    }
}

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
/// Per ADR-017 §Decision, dispatches to the right Card impl based on chip family.
/// Reads VID:DID from sysfs, looks up chip family via `chip_family_from_pci`,
/// then constructs the appropriate card implementation. Returns UnsupportedCard
/// for unknown VID:DID pairs instead of trying MptCard and failing with a
/// misleading "no IOC" error.
pub fn discover_one(bdf: &str) -> Result<Box<dyn Card>, CardError> {
    let (vid, did) = read_pci_ids(bdf)?;

    match chip_family_from_pci(vid, did) {
        ChipFamily::Sas2008 | ChipFamily::Sas2208 | ChipFamily::Sas3008 => {
            Ok(Box::new(mpt::MptCard::discover_one(bdf)?))
        }
        ChipFamily::Unknown => Err(CardError::UnsupportedCard(vid, did)),
    }
}

/// Read PCI vendor and device IDs from sysfs for a given BDF.
fn read_pci_ids(bdf: &str) -> Result<(u16, u16), CardError> {
    let read_hex = |path: &str| -> Result<u16, CardError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CardError::PciEnumeration(format!("read {}: {}", path, e)))?;
        let trimmed = raw.trim().trim_start_matches("0x");
        u16::from_str_radix(trimmed, 16)
            .map_err(|e| CardError::PciEnumeration(format!("parse {}: {}", path, e)))
    };

    let vid_path = format!("/sys/bus/pci/devices/{}/vendor", bdf);
    let did_path = format!("/sys/bus/pci/devices/{}/device", bdf);

    let vid = read_hex(&vid_path)?;
    let did = read_hex(&did_path)?;

    Ok((vid, did))
}

/// Map (VID, DID) to chip family via table lookup.
///
/// Sources: src/card_database.rs ChipFamily enum + card-database.toml entries.
/// Only Sas2008 DIDs are confirmed in the embedded database; Sas2208/Sas3008 ranges
/// are marked OPEN where no evidence exists yet.
fn chip_family_from_pci(vid: u16, did: u16) -> ChipFamily {
    match (vid, did) {
        // SAS2008 family — confirmed in card-database.toml (LSI 9211-8i, Dell H200/M1015 variants)
        (0x1000, 0x0072) => ChipFamily::Sas2008, // LSI 9211-8i IT/IR
        (0x1000, 0x0073) => ChipFamily::Sas2008, // LSI 9211-8i IMR / Dell H310

        // SAS2208 family — OPEN: no card-db evidence for DID range yet
        (0x1000, 0x0084) => ChipFamily::Sas2208, // Lenovo ServeRAID M5110 - single confirmed entry

        // Sas3008 family — OPEN: no card-db evidence for DID range yet
        (0x1000, 0x00C0) => ChipFamily::Sas3008, // LSI 9300 series - single confirmed entry

        _ => ChipFamily::Unknown,
    }
}
