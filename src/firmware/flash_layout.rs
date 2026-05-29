//! FLASH_LAYOUT extended-image parser for SAS2008 firmware.
//!
//! `FLASH_LAYOUT` is an MPI extended image (`MPI2_EXT_IMAGE_TYPE_FLASH_LAYOUT = 0x06`)
//! embedded *inside* a firmware blob — NOT a config page. It is located by walking
//! the extended-image linked list from the FW header's `NextImageHeaderOffset`, and
//! defines the flash partition map (region offsets/sizes) for each candidate chip
//! geometry. The bootloader selects the layout matching the detected physical chip.
//!
//! Cites: references/upstream/lsiutil/lsi/mpi2_ioc.h —
//! `MPI2_FW_IMAGE_HEADER` (NextImageHeaderOffset@0x30),
//! `MPI2_EXT_IMAGE_HEADER` (ImageType@0x00, ImageSize@0x08, NextImageHeaderOffset@0x0C),
//! `MPI2_FLASH_LAYOUT_DATA` / `MPI2_FLASH_LAYOUT` / `MPI2_FLASH_REGION` (1469-1518).
//!
//! Validated against the lsi-flash-firmware corpus (2026-05-29): see
//! lsi-flash-notes/02-hardware/flash-regions.md.

use crate::firmware::inspect::{FwError, MPI2_FW_HEADER_SIGNATURE};

/// `MPI2_FLASH_REGION_FIRMWARE` — the region a `fw` image lands in (mpi2_ioc.h:1507).
pub const REGION_FIRMWARE: u8 = 0x01;

const EXT_IMAGE_TYPE_FLASH_LAYOUT: u8 = 0x06;
const FW_HDR_NEXT_IMAGE_OFFSET: usize = 0x30; // U32 in MPI2_FW_IMAGE_HEADER

/// One flash region within a layout (`MPI2_FLASH_REGION`, 16 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashRegion {
    pub region_type: u8,
    pub offset: u32,
    pub size: u32,
}

/// One candidate flash geometry, keyed by physical chip size (`MPI2_FLASH_LAYOUT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub flash_size: u32,
    pub regions: Vec<FlashRegion>,
}

/// Parsed FLASH_LAYOUT: all candidate geometries the firmware supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashLayout {
    pub layouts: Vec<Layout>,
}

impl FlashLayout {
    /// Distinct FIRMWARE-region sizes across all candidate layouts, ascending.
    /// (1 MiB on >=4 MB chips, 640 KiB on the 2 MB chip layout — corpus 2026-05-29.)
    pub fn firmware_region_sizes(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .layouts
            .iter()
            .flat_map(|l| l.regions.iter())
            .filter(|r| r.region_type == REGION_FIRMWARE)
            .map(|r| r.size)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn rd_u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn rd_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// Walk the extended-image chain and parse the embedded FLASH_LAYOUT.
///
/// The ext-image offset is image-specific (a linked list), never hardcoded.
pub fn parse_flash_layout(data: &[u8]) -> Result<FlashLayout, FwError> {
    if data.len() < 0x40 || rd_u32(data, 0) != MPI2_FW_HEADER_SIGNATURE {
        return Err(FwError::TooShort(data.len()));
    }

    // 1. Find the FLASH_LAYOUT ext-image by walking the chain.
    let mut off = rd_u32(data, FW_HDR_NEXT_IMAGE_OFFSET) as usize;
    let mut seen = std::collections::HashSet::new();
    let mut base = None;
    while off != 0 && off + 0x10 <= data.len() && seen.insert(off) {
        let itype = data[off]; // MPI2_EXT_IMAGE_HEADER.ImageType @0x00
        let next = rd_u32(data, off + 0x0C) as usize; // NextImageHeaderOffset @0x0C
        if itype == EXT_IMAGE_TYPE_FLASH_LAYOUT {
            base = Some(off);
            break;
        }
        if next == 0 || next == off {
            break;
        }
        off = next;
    }
    let base = base.ok_or(FwError::NoFlashLayout)?;

    // 2. Locate MPI2_FLASH_LAYOUT_DATA within the ext-image payload. A vendor
    //    identify-string region precedes it; the struct begins where
    //    SizeOfRegion==0x10 with sane NumberOfLayouts / RegionsPerLayout counts.
    let payload = base + 0x10;
    let scan_end = (payload + 0x40).min(data.len().saturating_sub(0x20));
    let mut d = None;
    for c in payload..scan_end {
        let size_of_region = data[c + 0x02];
        let n_layouts = rd_u16(data, c + 0x04);
        let regions_per = rd_u16(data, c + 0x06);
        if size_of_region == 0x10
            && data[c] == 0x00
            && (1..=16).contains(&n_layouts)
            && (1..=16).contains(&regions_per)
        {
            d = Some(c);
            break;
        }
    }
    let d = d.ok_or(FwError::NoFlashLayout)?;

    let size_of_region = data[d + 0x02] as usize; // 0x10
    let n_layouts = rd_u16(data, d + 0x04) as usize;
    let regions_per = rd_u16(data, d + 0x06) as usize;
    let stride = 0x10 + regions_per * size_of_region; // MPI2_FLASH_LAYOUT size

    let mut layouts = Vec::with_capacity(n_layouts);
    for li in 0..n_layouts {
        let lp = d + 0x10 + li * stride;
        if lp + stride > data.len() {
            break;
        }
        let flash_size = rd_u32(data, lp); // MPI2_FLASH_LAYOUT.FlashSize @0x00
        let mut regions = Vec::with_capacity(regions_per);
        for ri in 0..regions_per {
            let ro = lp + 0x10 + ri * size_of_region;
            regions.push(FlashRegion {
                region_type: data[ro],           // @0x00
                offset: rd_u32(data, ro + 0x04), // @0x04
                size: rd_u32(data, ro + 0x08),   // @0x08
            });
        }
        layouts.push(Layout {
            flash_size,
            regions,
        });
    }
    if layouts.is_empty() {
        return Err(FwError::NoFlashLayout);
    }
    Ok(FlashLayout { layouts })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse FLASH_LAYOUT from the golden 2118it.bin (skips if fixture absent).
    /// Every SAS2008 layout must expose a 1 MiB FIRMWARE region; the geometry
    /// is corpus-validated in lsi-flash-notes/02-hardware/flash-regions.md.
    #[test]
    fn parses_2118it_flash_layout() {
        let base = std::env::var("LSI_FLASH_FIXTURES").unwrap_or_else(|_| {
            "/Users/mjackson/Developer/lsi-flash-notes/09-research-archive/upstream/lsi_sas_hba_crossflash_guide".to_string()
        });
        let fixture = std::path::PathBuf::from(base).join("2118it.bin");
        let Ok(data) = std::fs::read(&fixture) else {
            eprintln!("skipping: fixture {} not present", fixture.display());
            return;
        };
        let fl = parse_flash_layout(&data).expect("FLASH_LAYOUT should parse");
        assert!(!fl.layouts.is_empty());
        let fw_sizes = fl.firmware_region_sizes();
        assert!(
            fw_sizes.contains(&0x100000),
            "expected a 1 MiB FW region, got {:X?}",
            fw_sizes
        );
        // Every layout must contain exactly one FIRMWARE region.
        for l in &fl.layouts {
            let n = l
                .regions
                .iter()
                .filter(|r| r.region_type == REGION_FIRMWARE)
                .count();
            assert_eq!(n, 1, "each layout has one FIRMWARE region");
        }
    }

    #[test]
    fn rejects_non_firmware_blob() {
        assert!(matches!(
            parse_flash_layout(&[0u8; 0x80]),
            Err(FwError::TooShort(_))
        ));
    }
}
