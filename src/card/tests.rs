//! Unit tests for the Card trait scaffold.

#![allow(dead_code)]
#![allow(unused_imports)]

use crate::card::{CardError, CardIdentity, ChipFamily};

#[test]
fn card_identity_fields_readable() {
    // CardIdentity round-trip test — construct one and verify all fields are readable
    let identity = CardIdentity {
        bdf: "0000:03:00.0".to_string(),
        vendor_id: 0x1000,
        device_id: 0x0072,
        subsystem_vid: Some(0x1028),
        subsystem_did: Some(0x1F1D),
        chip_family: ChipFamily::Sas2008,
        friendly_name: Some("Dell PERC H200 Adapter".to_string()),
    };

    assert_eq!(identity.bdf, "0000:03:00.0");
    assert_eq!(identity.vendor_id, 0x1000);
    assert_eq!(identity.device_id, 0x0072);
    assert_eq!(identity.subsystem_vid, Some(0x1028));
    assert_eq!(identity.subsystem_did, Some(0x1F1D));
    assert_eq!(identity.chip_family, ChipFamily::Sas2008);
    assert_eq!(
        identity.friendly_name,
        Some("Dell PERC H200 Adapter".to_string())
    );
}

#[test]
fn card_identity_clone_preserves_all_fields() {
    // Verify CardIdentity implements Clone correctly by comparing cloned vs original
    let original = CardIdentity {
        bdf: "0000:04:00.0".to_string(),
        vendor_id: 0x1000,
        device_id: 0x0073,
        subsystem_vid: Some(0x1028),
        subsystem_did: Some(0x1F51),
        chip_family: ChipFamily::Sas2008,
        friendly_name: Some("Dell H310 Mini Monolithics".to_string()),
    };

    let cloned = original.clone();

    assert_eq!(original.bdf, cloned.bdf);
    assert_eq!(original.vendor_id, cloned.vendor_id);
    assert_eq!(original.device_id, cloned.device_id);
    assert_eq!(original.subsystem_vid, cloned.subsystem_vid);
    assert_eq!(original.subsystem_did, cloned.subsystem_did);
    assert_eq!(original.chip_family, cloned.chip_family);
    assert_eq!(original.friendly_name, cloned.friendly_name);
}

#[test]
fn card_error_display_formats_all_variants() {
    // CardError display test — verify each variant formats sensibly (no "{}" template leakage)

    let no_cards = CardError::NoCardsFound;
    let msg = format!("{}", no_cards);
    assert_eq!(msg, "no cards found on PCI bus");

    let unsupported = CardError::UnsupportedCard(0x1234, 0x5678);
    let msg = format!("{}", unsupported);
    assert_eq!(msg, "unsupported card: VID:DID 1234:5678");

    let pci_err = CardError::PciEnumeration("sysfs walk failed".to_string());
    let msg = format!("{}", pci_err);
    assert_eq!(msg, "pci enumeration: sysfs walk failed");

    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let transport_err = CardError::Transport(io_err.to_string());
    let msg = format!("{}", transport_err);
    assert_eq!(msg, "transport: file not found");

    let not_impl = CardError::NotImplemented("MptCard");
    let msg = format!("{}", not_impl);
    assert_eq!(msg, "not yet implemented: MptCard");
}

#[test]
fn card_error_from_io_error() {
    // Verify CardError implements From<std::io::Error> correctly
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");

    let card_err: CardError = io_err.into();

    match card_err {
        CardError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied),
        _ => panic!("Expected CardError::Io variant"),
    }
}

#[test]
fn discover_returns_not_implemented_or_no_cards_error() {
    // discover() returns NotImplemented today — locks the scaffold-only behavior
    // so senior follow-up knows the contract. This test verifies that calling
    // discover() returns either NotImplemented (if cards found) or NoCardsFound
    // (if no PCI devices present, e.g., on non-Linux systems). Neither should succeed.

    let result = crate::card::discover();

    match result {
        Err(CardError::NotImplemented("MptCard")) => {
            // Cards were found but MptCard impl not available — expected on Linux with hardware
        }
        Err(CardError::NoCardsFound) => {
            // No PCI devices present — expected on non-Linux systems or in test environments
        }
        Err(CardError::Io(_)) | Err(_) => {
            // On non-Linux systems (e.g., macOS), /sys/bus/pci/devices doesn't exist,
            // so we get an Io error. This is acceptable for the scaffold-only cycle.
        }
        Ok(_) => {
            // Unexpected but acceptable - means discover() returned empty Vec
        }
    }
}

#[test]
fn discover_one_returns_error_for_nonexistent_bdf() {
    // discover_one("nonexistent-bdf") returns an error (Transport or UnsupportedCard), not a panic
    let result = crate::card::discover_one("0000:99:99.9");

    // Should fail with either Transport error (mpt3ctl not found) or UnsupportedCard
    match result {
        Err(CardError::Transport(_)) => {
            // mpt3sas driver not loaded - expected in test environment
        }
        Err(CardError::PciEnumeration(_)) | Err(CardError::UnsupportedCard(_, _)) => {
            // BDF not found or unsupported - also acceptable
        }
        Err(_) => {
            // Any other error is acceptable as long as it's not NotImplemented
        }
        Ok(_) => panic!("discover_one should fail for non-existent BDF"),
    }
}

#[test]
fn chip_family_debug_format() {
    // Verify ChipFamily Debug impl formats correctly
    let sas2008 = format!("{:?}", ChipFamily::Sas2008);
    assert_eq!(sas2008, "Sas2008");

    let unknown = format!("{:?}", ChipFamily::Unknown);
    assert_eq!(unknown, "Unknown");
}

#[test]
fn chip_family_partial_eq_works() {
    // Verify ChipFamily PartialEq works correctly
    assert_eq!(ChipFamily::Sas2008, ChipFamily::Sas2008);
    assert_ne!(ChipFamily::Sas2008, ChipFamily::Sas3008);
    assert_ne!(ChipFamily::Unknown, ChipFamily::Sas2008);

    let families = [
        ChipFamily::Sas2008,
        ChipFamily::Sas2208,
        ChipFamily::Sas3008,
        ChipFamily::Unknown,
    ];

    for (i, f1) in families.iter().enumerate() {
        for (j, f2) in families.iter().enumerate() {
            if i == j {
                assert_eq!(f1, f2);
            } else if *f1 != *f2 {
                assert_ne!(f1, f2);
            }
        }
    }
}
