//! Card identity database — loaded from `resources/card-database.toml` at
//! build time via `include_str!`.
//!
//! Per ADR-014 (lsi-flash-notes/01-architecture/adr/014-sbr-verb-and-card-database.md):
//! the CLI ships a strict embedded database; users can override with their own
//! TOML at `~/.config/lsi-flash/card-database.toml`. Both files use the schema
//! documented in `resources/card-database.toml` (the canonical mirror of
//! `lsi-flash-firmware/card-database.toml`).

use serde::Deserialize;
use std::collections::HashMap;

/// Embedded canonical TOML — strict-tier, byte-confirmed identities only.
const EMBEDDED_DB: &str = include_str!("../resources/card-database.toml");

/// Error type for card database operations.
#[derive(thiserror::Error, Debug)]
pub enum CardDatabaseError {
    #[error("Parse TOML error: {0}")]
    ParseToml(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// SAS chip family. SAS2008 is the only in-scope target; the rest are recorded
/// so the CLI can fail loudly if presented with a sibling chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ChipFamily {
    #[serde(rename = "SAS2008")]
    Sas2008,
    #[serde(rename = "SAS2108")]
    Sas2108, // IBM ServeRAID M5014/M5015 — OUT OF SCOPE
    #[serde(rename = "SAS2208")]
    Sas2208, // Lenovo ServeRAID M5110 — OUT OF SCOPE
    #[serde(rename = "SAS2308")]
    Sas2308, // HP H220/H221/H222 — OUT OF SCOPE
    #[serde(rename = "SAS3008")]
    Sas3008, // LSI 9300 series — OUT OF SCOPE
    #[serde(other)]
    Unknown,
}

/// Firmware personality (factory default for the card).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Personality {
    IT,
    IR,
    IMR,
    IE,
}

/// Card-specific quirks. String keys come from the TOML `quirks` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quirk {
    /// Dell/IBM cards with tamper-check that survives normal flash erase.
    /// Workaround: megarec -cleanflash 0 or lsirec HCB hostboot path.
    DellTamperCheck,
    /// Fujitsu D2607 wiring byte at SBR offset 0x2A = 0x10 (A21 variant).
    FujitsuA21Wiring,
    /// 4 MB megarec composite (`.imr`) firmware format.
    ImrComposite,
    /// Dell H310 Mini Mono iDRAC validation gate.
    DellH310MiniMonoIdracLock,
    /// Dell H200 Internal Tape Adapter — smaller flash size.
    DellH200TapeAdapterSmallerFlash,
    /// Unknown quirk string from TOML — passed through for forward compat.
    Unknown(String),
}

impl From<&str> for Quirk {
    fn from(s: &str) -> Self {
        match s {
            "dell-tamper-check" => Quirk::DellTamperCheck,
            "fujitsu-a21-wiring" => Quirk::FujitsuA21Wiring,
            "imr-composite" => Quirk::ImrComposite,
            "dell-h310-mini-mono-idrac-lock" => Quirk::DellH310MiniMonoIdracLock,
            "dell-h200-tape-adapter-smaller-flash" => Quirk::DellH200TapeAdapterSmallerFlash,
            other => Quirk::Unknown(other.to_string()),
        }
    }
}

/// One card identity. Fields mirror the TOML schema in
/// `resources/card-database.toml`.
#[derive(Debug, Clone)]
pub struct CardInfo {
    /// Stable slug (TOML table key, e.g. `dell-h200-adapter`).
    pub slug: String,
    /// Human-readable name (e.g. "Dell PERC H200 Adapter").
    pub display: String,
    pub vid: u16,
    pub did: u16,
    pub subsys_vid: u16,
    pub subsys_pid: u16,
    pub chip_family: ChipFamily,
    pub default_personality: Personality,
    /// SAS WWN OUI prefix (typically `0x500605b0` for LSI). Stored as u32 to
    /// preserve the 3-byte OUI without padding ambiguity.
    pub sas_oui: u32,
    pub interface_byte: u8,
    pub factory_firmware: Option<String>,
    pub compatible_firmware: Vec<String>,
    pub quirks: Vec<Quirk>,
    pub form_factor: Option<String>,
    pub confirmed_via: Option<String>,
    pub notes: Option<String>,
}

// ---- TOML deserialization shape ---------------------------------------------

#[derive(Debug, Deserialize)]
struct Database {
    #[allow(dead_code)]
    schema_version: u32,
    #[allow(dead_code)]
    last_updated: String,
    #[allow(dead_code)]
    tier: String,
    card: HashMap<String, CardEntry>,
}

#[derive(Debug, Deserialize)]
struct CardEntry {
    display: String,
    vid: u16,
    did: u16,
    subsys_vid: u16,
    subsys_pid: u16,
    chip_family: ChipFamily,
    default_personality: Personality,
    sas_oui: u32,
    interface_byte: u8,
    factory_firmware: Option<String>,
    #[serde(default)]
    compatible_firmware: Vec<String>,
    #[serde(default)]
    quirks: Vec<String>,
    form_factor: Option<String>,
    confirmed_via: Option<String>,
    notes: Option<String>,
}

impl CardEntry {
    fn into_card_info(self, slug: String) -> CardInfo {
        CardInfo {
            slug,
            display: self.display,
            vid: self.vid,
            did: self.did,
            subsys_vid: self.subsys_vid,
            subsys_pid: self.subsys_pid,
            chip_family: self.chip_family,
            default_personality: self.default_personality,
            sas_oui: self.sas_oui,
            interface_byte: self.interface_byte,
            factory_firmware: self.factory_firmware,
            compatible_firmware: self.compatible_firmware,
            quirks: self
                .quirks
                .iter()
                .map(|s| Quirk::from(s.as_str()))
                .collect(),
            form_factor: self.form_factor,
            confirmed_via: self.confirmed_via,
            notes: self.notes,
        }
    }
}

// ---- Public API -------------------------------------------------------------

/// Parse a TOML database string into a sorted `Vec<CardInfo>`.
pub fn parse_database(toml_str: &str) -> Result<Vec<CardInfo>, CardDatabaseError> {
    let db: Database =
        toml::from_str(toml_str).map_err(|e| CardDatabaseError::ParseToml(e.to_string()))?;

    let mut cards: Vec<CardInfo> = db
        .card
        .into_iter()
        .map(|(slug, entry)| entry.into_card_info(slug))
        .collect();

    // Stable order for snapshot-friendly listings.
    cards.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(cards)
}

/// Load the embedded strict-tier database. Always succeeds for valid releases.
pub fn load_embedded() -> Result<Vec<CardInfo>, CardDatabaseError> {
    parse_database(EMBEDDED_DB)
}

/// Load with user-override fallback. If `override_path` exists, parse it
/// instead of the embedded database. Per ADR-014 §3.
pub fn load_with_fallback(
    override_path: Option<&std::path::Path>,
) -> Result<Vec<CardInfo>, CardDatabaseError> {
    if let Some(path) = override_path {
        if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            return parse_database(&contents);
        }
    }
    load_embedded()
}

/// Match VID:DID:SubsysVID:SubsysPID against the embedded database. Returns
/// the first match (slug-sorted) or `None`.
pub fn identify_card(
    cards: &[CardInfo],
    vid: u16,
    did: u16,
    subsys_vid: u16,
    subsys_pid: u16,
) -> Option<&CardInfo> {
    cards.iter().find(|c| {
        c.vid == vid && c.did == did && c.subsys_vid == subsys_vid && c.subsys_pid == subsys_pid
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_database_parses() {
        let cards = load_embedded().expect("embedded card-database.toml must parse");
        assert!(
            !cards.is_empty(),
            "embedded database must have at least one card"
        );
    }

    #[test]
    fn embedded_contains_lsi_9211() {
        let cards = load_embedded().unwrap();
        let lsi = identify_card(&cards, 0x1000, 0x0072, 0x1000, 0x3020)
            .expect("LSI 9211-8i must be in the embedded database");
        assert_eq!(lsi.slug, "lsi-9211-8i");
        assert!(matches!(lsi.chip_family, ChipFamily::Sas2008));
        assert!(matches!(lsi.default_personality, Personality::IT));
    }

    #[test]
    fn embedded_contains_dell_h200_variants() {
        // PIDs per canonical pci.ids + real-hardware lspci on dev-1 (corrected
        // from earlier off-by-one error — see 02-hardware/unconfirmed-identities-research.md).
        let cards = load_embedded().unwrap();

        // 0x1F1C is the non-PERC Dell 6Gbps SAS HBA, NOT H200 Adapter.
        let six_gbps = identify_card(&cards, 0x1000, 0x0072, 0x1028, 0x1f1c)
            .expect("Dell 6Gbps SAS HBA Adapter must be in the embedded database");
        assert_eq!(six_gbps.slug, "dell-6gbps-sas-hba-adapter");

        // 0x1F1D is the actual H200 Adapter.
        let adapter = identify_card(&cards, 0x1000, 0x0072, 0x1028, 0x1f1d)
            .expect("Dell H200 Adapter must be in the embedded database");
        assert_eq!(adapter.slug, "dell-h200-adapter");
        assert!(adapter.quirks.contains(&Quirk::DellTamperCheck));

        // 0x1F1E is Integrated.
        let integrated = identify_card(&cards, 0x1000, 0x0072, 0x1028, 0x1f1e)
            .expect("Dell H200 Integrated must be in the embedded database");
        assert_eq!(integrated.slug, "dell-h200-integrated");

        // 0x1F1F is Modular.
        let modular = identify_card(&cards, 0x1000, 0x0072, 0x1028, 0x1f1f)
            .expect("Dell H200 Modular must be in the embedded database");
        assert_eq!(modular.slug, "dell-h200-modular");
    }

    #[test]
    fn embedded_contains_dell_h310_family() {
        let cards = load_embedded().unwrap();
        // H310 Mini Monolithics — has a local sample SBR.
        let mini_mono = identify_card(&cards, 0x1000, 0x0073, 0x1028, 0x1f51)
            .expect("Dell H310 Mini Monolithics must be in the embedded database");
        assert_eq!(mini_mono.slug, "dell-h310-mini-monolithics");
        assert!(matches!(mini_mono.default_personality, Personality::IMR));

        // H310 full-size — has a local "modded" sample SBR.
        let full_size = identify_card(&cards, 0x1000, 0x0073, 0x1028, 0x1f78)
            .expect("Dell H310 full-size must be in the embedded database");
        assert_eq!(full_size.slug, "dell-h310-full-size");
    }

    #[test]
    fn embedded_contains_ibm_m1015() {
        let cards = load_embedded().unwrap();
        let ibm = identify_card(&cards, 0x1000, 0x0073, 0x1014, 0x03b1)
            .expect("IBM M1015 must be in the embedded database (IMR personality)");
        assert_eq!(ibm.slug, "ibm-m1015");
        assert!(matches!(ibm.default_personality, Personality::IMR));
    }

    #[test]
    fn embedded_contains_fujitsu_d2607() {
        let cards = load_embedded().unwrap();
        let fuj = identify_card(&cards, 0x1000, 0x0072, 0x1734, 0x1177)
            .expect("Fujitsu D2607 must be in the embedded database");
        assert_eq!(fuj.slug, "fujitsu-d2607");
        assert!(fuj.quirks.contains(&Quirk::FujitsuA21Wiring));
    }

    #[test]
    fn identify_unknown_card_returns_none() {
        let cards = load_embedded().unwrap();
        assert!(identify_card(&cards, 0x1234, 0x5678, 0xabcd, 0xef01).is_none());
    }

    #[test]
    fn load_with_fallback_no_override_returns_embedded() {
        let cards = load_with_fallback(None).unwrap();
        let embedded = load_embedded().unwrap();
        assert_eq!(cards.len(), embedded.len());
    }
}
