//! 5-point firmware validity gate — ADR-015 Rule 11a + brick-safety.
//!
//! The risk model is exactly two failure modes (operator, 2026-05-29): the image
//! *doesn't fit* the card's FW region, or it *isn't right for the card* (wrong
//! chip / corrupt / garbage). Personality (IT vs IR) is NOT a risk axis — IT and
//! IR are byte-identical in FLASH_LAYOUT geometry across P07-P20 (corpus 2026-05-29).
//!
//! These checks are **reusable building blocks**: the same gate validates a
//! self-synthesized firmware. The image-only checks (1-3) need no hardware;
//! `check_fit` (4) adds the card-derived ceiling. Write-integrity (5) is the
//! post-write read-back, enforced by the caller. Every check fails closed.

use crate::firmware::flash_layout::{parse_flash_layout, FlashLayout};
use crate::firmware::inspect::{parse_fw_header, verify_file_checksum, FwHeader};

/// LSI / Broadcom PCI vendor ID (FW header @0x20).
pub const VENDOR_LSI: u16 = 0x1000;

/// Known SAS2008 firmware ProductIDs (FW header @0x22). From the
/// lsi-flash-firmware corpus scan (2026-05-29): every SAS2008 IT/IR image
/// carries 0x2713 or 0x2213, while SAS2208 is 0x2221 — one bit away. ProductID
/// is therefore *necessary but not sufficient*; the right-chip check corroborates
/// it with the MPI signatures (well-formed) and a parseable FLASH_LAYOUT.
pub const SAS2008_PRODUCT_IDS: &[u16] = &[0x2713, 0x2213];

/// One validation check and its outcome.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub pass: bool,
    pub detail: String,
}

/// Result of running the validity gate over an image (+ optionally a card).
#[derive(Debug)]
pub struct FwValidation {
    pub checks: Vec<Check>,
    pub header: Option<FwHeader>,
    pub layout: Option<FlashLayout>,
}

impl FwValidation {
    /// True iff every check passed.
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.pass)
    }
    /// The first failing check, if any.
    pub fn first_failure(&self) -> Option<&Check> {
        self.checks.iter().find(|c| !c.pass)
    }
}

/// Image-only checks (1-3): well-formed, checksum, right-chip. No hardware
/// needed — reused verbatim to validate self-synthesized firmware.
pub fn validate_image(image: &[u8]) -> FwValidation {
    let mut checks = Vec::new();
    let header = parse_fw_header(image).ok();

    // 1. Well-formed — valid MPI2 FW image (signatures).
    match &header {
        Some(h) => checks.push(Check {
            name: "well-formed",
            pass: true,
            detail: format!(
                "MPI2 FW image — vid=0x{:04X} did=0x{:04X} image_size={} B",
                h.vendor_id, h.device_id, h.image_size
            ),
        }),
        None => checks.push(Check {
            name: "well-formed",
            pass: false,
            detail: "not a valid MPI2 firmware image (bad signature)".into(),
        }),
    }

    // 2. Intact — file-level U32 checksum == 0.
    let cks = header.is_some() && verify_file_checksum(image);
    checks.push(Check {
        name: "checksum",
        pass: cks,
        detail: if cks {
            "file-level U32 checksum = 0".into()
        } else {
            "checksum mismatch — corrupt or truncated".into()
        },
    });

    // 3. Right chip — LSI vendor + known SAS2008 ProductID + parseable layout.
    let layout = parse_flash_layout(image).ok();
    let chip_ok = match &header {
        Some(h) => {
            h.vendor_id == VENDOR_LSI
                && SAS2008_PRODUCT_IDS.contains(&h.device_id)
                && layout.is_some()
        }
        None => false,
    };
    checks.push(Check {
        name: "right-chip",
        pass: chip_ok,
        detail: match &header {
            Some(h) if h.vendor_id != VENDOR_LSI => {
                format!("vendor 0x{:04X} != LSI 0x1000", h.vendor_id)
            }
            Some(h) if !SAS2008_PRODUCT_IDS.contains(&h.device_id) => format!(
                "ProductID 0x{:04X} not a known SAS2008 id {:04X?}",
                h.device_id, SAS2008_PRODUCT_IDS
            ),
            Some(_) if layout.is_none() => "FLASH_LAYOUT not parseable".into(),
            Some(_) => "SAS2008 firmware, FLASH_LAYOUT present".into(),
            None => "n/a (not well-formed)".into(),
        },
    });

    FwValidation {
        checks,
        header,
        layout,
    }
}

/// Add the card-dependent fit check (4): the new image must fit the FW region
/// active on THIS card. The active region can't be queried directly, but the
/// card's currently-running firmware already occupies it, so:
///   `ceiling = smallest candidate FW-region >= current running ImageSize`
/// This is conservative — it can only ever be too strict, never too loose — and
/// uses no guessed constant. Fail-closed if anything can't be read.
pub fn check_fit(val: &mut FwValidation, new_image: &[u8], current_fw: &[u8]) {
    let new_size = parse_fw_header(new_image).map(|h| h.image_size);
    let cur_size = parse_fw_header(current_fw).map(|h| h.image_size);
    let cur_layout = parse_flash_layout(current_fw).ok();

    let (pass, detail) = match (new_size, cur_size, cur_layout) {
        (Ok(ns), Ok(cs), Some(layout)) => {
            let candidates = layout.firmware_region_sizes();
            match candidates.iter().copied().filter(|r| *r >= cs).min() {
                Some(ceiling) => (
                    ns <= ceiling,
                    format!(
                        "image {} B vs FW region {} B (active layout inferred from running {} B; candidates {:X?})",
                        ns, ceiling, cs, candidates
                    ),
                ),
                None => (
                    false,
                    format!(
                        "cannot resolve active FW region: running {} B exceeds all candidates {:X?}",
                        cs, candidates
                    ),
                ),
            }
        }
        _ => (
            false,
            "cannot read new/current FW header or card FLASH_LAYOUT — refusing".into(),
        ),
    };
    val.checks.push(Check {
        name: "fit",
        pass,
        detail,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_not_well_formed() {
        let v = validate_image(&[0u8; 64]);
        assert!(!v.ok());
        assert_eq!(v.first_failure().unwrap().name, "well-formed");
    }

    #[test]
    fn product_id_allowlist_excludes_sas2208() {
        // SAS2208 is 0x2221 — must NOT be accepted as SAS2008.
        assert!(!SAS2008_PRODUCT_IDS.contains(&0x2221));
        assert!(SAS2008_PRODUCT_IDS.contains(&0x2713));
        assert!(SAS2008_PRODUCT_IDS.contains(&0x2213));
    }

    #[test]
    fn fit_fails_closed_on_unreadable_inputs() {
        let mut v = FwValidation {
            checks: vec![],
            header: None,
            layout: None,
        };
        check_fit(&mut v, &[0u8; 64], &[0u8; 64]);
        assert!(!v.checks.last().unwrap().pass);
    }
}
