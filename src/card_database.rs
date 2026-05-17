/// Error type for card database operations. Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum CardDatabaseError {
    #[error("Parse TOML error: {0}")]
    ParseToml(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Chip family enum. Cites references/oems/card-database.md tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipFamily {
    Sas2008, // SAS2008 ROC (most common target)
    Sas2108, // IBM ServeRAID M5014/M5015 — OUT OF SCOPE
    Sas2208, // Lenovo ServeRAID M5110 — OUT OF SCOPE
    Sas2308, // HP H220/H221/H222 — OUT OF SCOPE
    Sas3008, // LSI 9300 series — OUT OF SCOPE
    Unknown,
}

/// Card quirk flags. Cites references/oems/card-database.md "Quirks / status" column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quirk {
    /// Dell/IBM cards requiring megarec or hostboot path for erase.
    TamperCheckRequired,

    /// Fujitsu D2607 A11 variant (standard SBR).
    FujitsuSbrVariantA11,

    /// Fujitsu D2607 A21 variant (preserves wiring byte at offset 0x2A).
    FujitsuSbrVariantA21,

    /// Dell H310 Mini Mono iDRAC validation gate.
    DellH310MiniMonoIdracLock,

    /// Dell H200 Internal Tape Adapter — smaller flash size.
    DellH200TapeAdapterSmallerFlash,
}

/// Card identification info. Cites references/oems/card-database.md.
#[derive(Debug, Clone)]
pub struct CardInfo {
    pub name: String,             // Human-readable name (e.g., "Dell PERC H200e")
    pub vendor_id: u16,           // PCI Vendor ID
    pub device_id: u16,           // PCI Device ID
    pub subsystem_vendor_id: u16, // Subsystem Vendor ID (OEM-specific)
    pub subsystem_device_id: u16, // Subsystem Device ID (card model within OEM)
    pub chip_family: ChipFamily,
    pub flash_size: Option<usize>, // Optional exact flash size in bytes
    pub quirks: Vec<Quirk>,
}

/// Match VID:DID:SSVID:SSID against known cards. Cites references/oems/card-database.md tables.
pub fn identify_card(vid: u16, did: u16, ssid_vid: u16, ssid_did: u16) -> CardInfo {
    // LSI 9211-8i (stock). Cites card-database.md:72.
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1000 && ssid_did == 0x3020 {
        return CardInfo {
            name: "LSI 9211-8i".to_string(),
            vendor_id: vid,
            device_id: did,
            subsystem_vendor_id: ssid_vid,
            subsystem_device_id: ssid_did,
            chip_family: ChipFamily::Sas2008,
            flash_size: Some(722708), // Cites lsi_sas_hba_crossflash_guide/README.md listing
            quirks: vec![],
        };
    }

    // Dell PERC H200e (full-height). Cites card-database.md:81.
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1028 && ssid_did == 0x1f1c {
        return CardInfo {
            name: "Dell PERC H200e".to_string(),
            vendor_id: vid,
            device_id: did,
            subsystem_vendor_id: ssid_vid,
            subsystem_device_id: ssid_did,
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN: no local evidence for exact size
            quirks: vec![Quirk::TamperCheckRequired],
        };
    }

    // Fujitsu D2607. Cites card-database.md:115.
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1734 && ssid_did == 0x1177 {
        return CardInfo {
            name: "Fujitsu D2607".to_string(),
            vendor_id: vid,
            device_id: did,
            subsystem_vendor_id: ssid_vid,
            subsystem_device_id: ssid_did,
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN: no local evidence for exact size
            quirks: vec![Quirk::TamperCheckRequired, Quirk::FujitsuSbrVariantA11],
        };
    }

    // IBM M1015 — SSID UNKNOWN per card-database.md:99. Mark as OPEN.
    if vid == 0x1000 && did == 0x0072 {
        return CardInfo {
            name: "Unknown SAS2008 card (VID:DID=1000:0072)".to_string(),
            vendor_id: vid,
            device_id: did,
            subsystem_vendor_id: ssid_vid,
            subsystem_device_id: ssid_did,
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN
            quirks: vec![Quirk::TamperCheckRequired],
        };
    }

    CardInfo {
        name: format!("Unknown card (VID:0x{:04x}, DID:0x{:04x})", vid, did),
        vendor_id: vid,
        device_id: did,
        subsystem_vendor_id: ssid_vid,
        subsystem_device_id: ssid_did,
        chip_family: ChipFamily::Unknown,
        flash_size: None, // OPEN
        quirks: vec![],
    }
}

/// Check if card is known (vs unknown/OPEN). Cites card-database.md.
pub fn is_known_card(vid: u16, did: u16, ssid_vid: u16, ssid_did: u16) -> bool {
    let info = identify_card(vid, did, ssid_vid, ssid_did);
    matches!(info.chip_family, ChipFamily::Sas2008) && !info.name.contains("Unknown")
}

/// Get all known cards (for detection UI). Cites card-database.md tables.
pub fn list_known_cards() -> Vec<CardInfo> {
    vec![
        CardInfo {
            name: "LSI 9211-8i".to_string(),
            vendor_id: 0x1000,
            device_id: 0x0072,
            subsystem_vendor_id: 0x1000,
            subsystem_device_id: 0x3020,
            chip_family: ChipFamily::Sas2008,
            flash_size: Some(722708),
            quirks: vec![],
        },
        CardInfo {
            name: "Dell PERC H200e".to_string(),
            vendor_id: 0x1000,
            device_id: 0x0072,
            subsystem_vendor_id: 0x1028,
            subsystem_device_id: 0x1f1c,
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN
            quirks: vec![Quirk::TamperCheckRequired],
        },
        CardInfo {
            name: "Fujitsu D2607".to_string(),
            vendor_id: 0x1000,
            device_id: 0x0072,
            subsystem_vendor_id: 0x1734,
            subsystem_device_id: 0x1177,
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN
            quirks: vec![Quirk::TamperCheckRequired, Quirk::FujitsuSbrVariantA11],
        },
    ]
}

/// Embedded cards.toml content. Cites references/oems/card-database.md as source.
const CARDS_TOML: &str = r#"
[[card]]
name = "LSI 9211-8i"
vendor_id = 0x1000
device_id = 0x72
subsystem_vendor_id = 0x1000
subsystem_device_id = 0x3020
chip_family = "Sas2008"
flash_size = 722708

[[card]]
name = "Dell PERC H200e"
vendor_id = 0x1000
device_id = 0x72
subsystem_vendor_id = 0x1028
subsystem_device_id = 0x1f1c
chip_family = "Sas2008"
quirks = ["TamperCheckRequired"]

[[card]]
name = "Fujitsu D2607"
vendor_id = 0x1000
device_id = 0x72
subsystem_vendor_id = 0x1734
subsystem_device_id = 0x1177
chip_family = "Sas2008"
quirks = ["TamperCheckRequired", "FujitsuSbrVariantA11"]
"#;

/// TOML entry struct (for serde parsing). Cites serde usage.
#[derive(serde::Deserialize, Debug)]
struct CardTomlEntry {
    name: String,
    vendor_id: u16,
    device_id: u16,
    subsystem_vendor_id: u16,
    subsystem_device_id: u16,
    chip_family: String,
    flash_size: Option<u32>,
    quirks: Vec<String>,
}

impl From<CardTomlEntry> for CardInfo {
    fn from(entry: CardTomlEntry) -> Self {
        let chip_family = match entry.chip_family.as_str() {
            "Sas2008" => ChipFamily::Sas2008,
            "Sas2108" => ChipFamily::Sas2108,
            "Sas2208" => ChipFamily::Sas2208,
            "Sas2308" => ChipFamily::Sas2308,
            "Sas3008" => ChipFamily::Sas3008,
            _ => ChipFamily::Unknown,
        };

        let quirks: Vec<Quirk> = entry
            .quirks
            .iter()
            .filter_map(|s| match s.as_str() {
                "TamperCheckRequired" => Some(Quirk::TamperCheckRequired),
                "FujitsuSbrVariantA11" => Some(Quirk::FujitsuSbrVariantA11),
                "FujitsuSbrVariantA21" => Some(Quirk::FujitsuSbrVariantA21),
                "DellH310MiniMonoIdracLock" => Some(Quirk::DellH310MiniMonoIdracLock),
                "DellH200TapeAdapterSmallerFlash" => Some(Quirk::DellH200TapeAdapterSmallerFlash),
                _ => None,
            })
            .collect();

        CardInfo {
            name: entry.name,
            vendor_id: entry.vendor_id,
            device_id: entry.device_id,
            subsystem_vendor_id: entry.subsystem_vendor_id,
            subsystem_device_id: entry.subsystem_device_id,
            chip_family,
            flash_size: entry.flash_size.map(|s| s as usize),
            quirks,
        }
    }
}

/// Parse cards from embedded TOML string. Cites serde/toml usage if available.
pub fn load_cards_from_toml() -> Result<Vec<CardInfo>, CardDatabaseError> {
    let cards: Vec<CardTomlEntry> =
        toml::from_str(CARDS_TOML).map_err(|e| CardDatabaseError::ParseToml(e.to_string()))?;

    Ok(cards.into_iter().map(|entry| entry.into()).collect())
}
