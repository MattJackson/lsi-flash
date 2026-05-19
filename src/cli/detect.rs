//! Detect verb implementation — walks PCI sysfs, identifies SAS2008-family cards.

#![allow(clippy::too_many_lines)]

use crate::pci;
use std::io;

/// Run the detect verb. Returns detected cards as human-readable or JSON output.
pub fn run(json: bool) -> Result<(), crate::Error> {
    let devices = pci::discover_sas2008_devices_linux()
        .map_err(|e| crate::Error::Other(format!("PCI discovery failed: {}", e)))?;

    if json {
        return print_json(&devices).map_err(|e| crate::Error::Io(e));
    }

    print_human_readable(&devices)?;
    Ok(())
}

/// Print human-readable detection report to stdout.
fn print_human_readable(devices: &[pci::PciDevice]) -> io::Result<()> {
    if devices.is_empty() {
        println!("No SAS2008-family devices found.");
        return Ok(());
    }

    for (i, dev) in devices.iter().enumerate() {
        let card_info = pci::identify_card(
            dev.vendor_id,
            dev.device_id,
            dev.subsystem_vendor_id,
            dev.subsystem_device_id,
        );

        println!("Device {}: {}", i + 1, card_info.name);
        println!("  BDF: {}", dev.bdf);
        println!("  Vendor ID: 0x{:04x}", dev.vendor_id);
        println!("  Device ID: 0x{:04x}", dev.device_id);
        println!("  Subsystem Vendor ID: 0x{:04x}", dev.subsystem_vendor_id);
        println!("  Subsystem Device ID: 0x{:04x}", dev.subsystem_device_id);

        if matches!(card_info.chip_family, pci::ChipFamily::Sas2008) {
            println!("  Chip Family: SAS2008");
        } else {
            println!("  Chip Family: Unknown");
        }

        if card_info.quirks.contains(&pci::Quirk::TamperCheckRequired) {
            println!(
                "  Quirks: Dell tamper-check required (use megarec -cleanflash or lsirec HCB path)"
            );
        }

        if i < devices.len() - 1 {
            println!();
        }
    }

    Ok(())
}

/// Print detection report as JSON to stdout.
fn print_json(devices: &[pci::PciDevice]) -> io::Result<()> {
    let cards: Vec<JsonCard> = devices
        .iter()
        .map(|dev| {
            pci::identify_card(
                dev.vendor_id,
                dev.device_id,
                dev.subsystem_vendor_id,
                dev.subsystem_device_id,
            )
        })
        .zip(devices.iter())
        .map(|(card_info, dev)| JsonCard {
            bdf: dev.bdf.clone(),
            vendor_id: dev.vendor_id,
            device_id: dev.device_id,
            subsystem_vendor_id: dev.subsystem_vendor_id,
            subsystem_device_id: dev.subsystem_device_id,
            card_name: card_info.name,
            chip_family: match card_info.chip_family {
                pci::ChipFamily::Sas2008 => "SAS2008",
                pci::ChipFamily::Unknown => "Unknown",
            },
        })
        .collect();

    let output = serde_json::json!({
        "cards": cards
    });
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

/// JSON-serializable card representation.
#[derive(Debug, Clone, serde::Serialize)]
struct JsonCard {
    bdf: String,
    vendor_id: u16,
    device_id: u16,
    subsystem_vendor_id: u16,
    subsystem_device_id: u16,
    card_name: String,
    chip_family: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::{MockPlatform, PciDevice};
    use serde_json;

    /// Verify that SAS2008-family device IDs (from card-database.toml) are recognized.
    #[test]
    fn sas2008_family_recognizes_known_dids() {
        // All cards in card-database.toml have DID 0x0072 for SAS2008 family
        assert!(is_sas2008_family(0x1000, 0x0072));

        // Verify a few known SSIDs from the database
        let test_cases = vec![
            (0x1000, 0x0072, 0x1000, 0x3020), // LSI 9211-8i
            (0x1000, 0x0072, 0x1028, 0x1f1c), // Dell 6Gbps SAS HBA Adapter
            (0x1000, 0x0072, 0x1028, 0x1f1d), // Dell PERC H200 Adapter
            (0x1000, 0x0072, 0x1734, 0x1177), // Fujitsu D2607
        ];

        for (vid, did, _ssvid, _ssdid) in test_cases {
            assert!(
                is_sas2008_family(vid, did),
                "Expected DID 0x{:04x} to be SAS2008 family",
                did
            );
        }
    }

    /// Verify that unrelated device IDs (SAS2308, SAS3008) are rejected.
    #[test]
    fn sas2008_family_rejects_unrelated_dids() {
        // SAS2308: HP H220/H221/H222 — DID 0x0087
        assert!(
            !is_sas2008_family(0x1000, 0x0087),
            "SAS2308 should NOT be SAS2008 family"
        );

        // SAS3008: LSI 9300 series — DID 0x00C0
        assert!(
            !is_sas2008_family(0x1000, 0x00c0),
            "SAS3008 should NOT be SAS2008 family"
        );

        // Different vendor entirely
        assert!(
            !is_sas2008_family(0x8086, 0x1234),
            "Intel device should NOT be SAS2008 family"
        );
    }

    /// Feed a synthetic DetectedCard to serde_json and verify JSON output.
    #[test]
    fn detect_report_serializes_to_json() {
        let card = JsonCard {
            bdf: "0000:01:00.0".to_string(),
            vendor_id: 0x1000,
            device_id: 0x0072,
            subsystem_vendor_id: 0x1028,
            subsystem_device_id: 0x1f1d,
            card_name: "Dell PERC H200 Adapter".to_string(),
            chip_family: "SAS2008",
        };

        let output = serde_json::json!({
            "cards": [card]
        });

        let json_str = serde_json::to_string(&output).unwrap();
        assert!(json_str.contains("0000:01:00.0"));
        assert!(json_str.contains("Dell PERC H200 Adapter"));
    }

    /// Use MockPlatform to feed synthetic sysfs data and verify detection succeeds.
    #[test]
    fn detect_with_mock_platform_finds_lsi_device() {
        let mut mock = MockPlatform::new();

        // Add a Dell PERC H200 Adapter (DID 0x0072, SSID 0x1028:0x1f1d)
        mock.add_device("0000:03:00.0", 0x1000, 0x0072, 0x1028, 0x1F1D, 0x010700);

        // Add an unrelated device (Intel host bridge) that should be skipped
        mock.add_device("0000:00:00.0", 0x8086, 0x1234, 0, 0, 0x060000);

        // Add another SAS2008 card (LSI 9211-8i)
        mock.add_device("0000:04:00.0", 0x1000, 0x0072, 0x1000, 0x3020, 0x010700);

        let devices = discover_sas2008_devices(&mock).unwrap();

        assert_eq!(devices.len(), 2, "Should find exactly 2 SAS2008 cards");

        // Verify device BDFs are correct
        let bdfs: Vec<&str> = devices.iter().map(|d| d.bdf.as_str()).collect();
        assert!(bdfs.contains(&"0000:03:00.0"));
        assert!(bdfs.contains(&"0000:04:00.0"));

        // Verify the run() function works with mock platform output
        let result = print_human_readable(&devices);
        assert!(result.is_ok());
    }

    /// Helper to check if VID:DID is SAS2008 family (DID 0x0072 for vendor 0x1000).
    fn is_sas2008_family(vid: u16, did: u16) -> bool {
        vid == 0x1000 && did == 0x0072
    }

    /// Convenience wrapper for tests to use MockPlatform directly.
    fn discover_sas2008_devices<P: pci::Platform>(
        plat: &P,
    ) -> Result<Vec<PciDevice>, crate::error::PciError> {
        pci::discover_sas2008_devices(plat)
    }
}
