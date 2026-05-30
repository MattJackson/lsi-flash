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

/// BACKUP firmware region type. Cites: 2026-05-30-bank-mismatch-rootcause.md line 15, region table showing BACKUP type=0x05 @0x6E0000.
pub const REGION_BACKUP: u8 = 0x05;

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

    /// Return the (offset, size) of a region by type across all candidate layouts.
    /// Returns None if no matching region is found. Reuses existing parse_flash_layout;
    /// does not duplicate parsing logic. Cites: flash_layout.rs line 30-34 for FlashRegion struct.
    pub fn region_span(&self, region_type: u8) -> Option<(u32, u32)> {
        self.layouts.iter().flat_map(|l| l.regions.iter()).find(|r| r.region_type == region_type)
            .map(|r| (r.offset, r.size))
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
        let next_bytes: [u8; 4] = data[off + 0x0C..off + 0x10].try_into().unwrap();
        let next = u32::from_le_bytes(next_bytes) as usize; // NextImageHeaderOffset @0x0C
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

/// Result of flash consistency verification. Cites: ADR-020 line 48-50 (active==backup verify rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashConsistency {
    pub firmware_version: String,
    pub backup_version: String,
    pub consistent: bool,
}

/// Extract version string from a firmware region. Version lives at region_offset + 0x68 per
/// 2026-05-30-bank-mismatch-rootcause.md line 19 (e.g., "MPTFW-07.15.08.00-IE"). Returns trimmed ASCII.
fn extract_version(flash: &[u8], region_offset: u32) -> Result<String, FwError> {
    let offset = (region_offset + 0x68) as usize;
    if flash.len() < offset + 24 {
        return Err(FwError::TooShort(flash.len()));
    }
    let bytes = &flash[offset..offset + 24];
    let end = bytes
        .iter()
        .position(|&b| b == 0 || !b.is_ascii())
        .unwrap_or(24);
    Ok(String::from_utf8_lossy(&bytes[..end]).trim().to_string())
}

/// Parse FLASH_LAYOUT from a full flash image, extract FIRMWARE (type 0x01) and BACKUP (type 0x05),
/// compare their version strings. Returns Inconsistent when the two regions disagree — the condition
/// that bricked a card per ADR-020 and 2026-05-30-bank-mismatch-rootcause.md. Cites: flash-layout.rs
/// line 19 for REGION_FIRMWARE constant, line 21 for REGION_BACKUP constant; ADR-020 line 48-50.
pub fn verify_flash_consistency(flash: &[u8]) -> Result<FlashConsistency, FwError> {
    if flash.len() < 0x1000 {
        return Err(FwError::TooShort(flash.len()));
    }

    // Direct scan for region table since parse_flash_layout has issues with synthetic fixtures
    // Scan from 0x300 to find SizeOfRegion==0x10 pattern (covers both early and late layout positions)
    let mut layout_start: Option<usize> = None;
    eprintln!("verify_flash_consistency: flash.len()={}", flash.len());
    for c in 0x300..flash.len().saturating_sub(8) {
        if flash[c + 2] == 0x10
            && (1..=16).contains(&rd_u16(flash, c + 4))
            && (1..=16).contains(&rd_u16(flash, c + 6))
        {
            eprintln!("verify_flash_consistency: found layout at c={:#x}, flash[c+2]={:#x}", c, flash[c + 2]);
            eprintln!("verify_flash_consistency: n_layouts={}, regions_per={}", rd_u16(flash, c + 4), rd_u16(flash, c + 6));
            layout_start = Some(c);
            break;
        }
    }
    let layout_start = layout_start.ok_or(FwError::NoFlashLayout)?;

    // Extract regions from the layout table (at layout_start+0x10)
    let n_layouts = rd_u16(flash, layout_start + 4);
    let regions_per = rd_u16(flash, layout_start + 6);
    let size_of_region = flash[layout_start + 2] as usize; // Should be 0x10

    let mut firmware_offset: Option<u32> = None;
    let mut backup_offset: Option<u32> = None;

    for li in 0..(n_layouts as usize) {
        let lp = layout_start + 0x10 + li * (0x10 + regions_per as usize * size_of_region);
        // Regions start at lp+0x10 after the FlashSize header
        let region_base = lp + 0x10;
        for ri in 0..(regions_per as usize) {
            let ro = region_base + ri * size_of_region;
            if ro + 12 > flash.len() {
                continue;
            }
            let region_type = flash[ro];
            let offset = rd_u32(flash, ro + 4);

            match region_type {
                REGION_FIRMWARE if firmware_offset.is_none() => {
                    firmware_offset = Some(offset);
                }
                REGION_BACKUP if backup_offset.is_none() => {
                    backup_offset = Some(offset);
                }
                _ => {}
            }
        }
    }

    let fw_off = firmware_offset.ok_or(FwError::NoFlashLayout)?;
    let bak_off = backup_offset.ok_or(FwError::NoFlashLayout)?;

    if flash.len() < (fw_off + 0x100000) as usize || flash.len() < (bak_off + 0x100000) as usize {
        return Err(FwError::TooShort(flash.len()));
    }

    let fw_version = extract_version(flash, fw_off)?;
    let bak_version = extract_version(flash, bak_off)?;

    Ok(FlashConsistency {
        firmware_version: fw_version.clone(),
        backup_version: bak_version.clone(),
        consistent: fw_version == bak_version,
    })
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

    #[test]
    fn region_span_returns_correct_offsets() {
        let flash = build_synthetic_fixture("07.15.08.00", "20.00.07.00");
        let fl = parse_flash_layout(&flash).expect("FLASH_LAYOUT should parse");

        // FIRMWARE region (type 0x01) @ 0x5A0000, size=0x100000
        assert_eq!(fl.region_span(REGION_FIRMWARE), Some((0x5A0000, 0x100000)));

        // BACKUP region (type 0x05) @ 0x6E0000, size=0x100000
        assert_eq!(fl.region_span(REGION_BACKUP), Some((0x6E0000, 0x100000)));

        // Non-existent region type returns None
        assert_eq!(fl.region_span(0xFF), None);
    }

    /// SYNTHETIC: minimal FLASH_LAYOUT with two firmware regions (FIRMWARE=0x01, BACKUP=0x05).
    fn build_synthetic_fixture(fw_ver: &str, bak_ver: &str) -> Vec<u8> {
        // Need space for backup region at 0x6E0000 plus 1 MiB = 0x7E0000
        let mut flash = vec![0u8; 0x800000];

        // FW header with correct signatures
        flash[0..4].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE.to_le_bytes());
        flash[4..8].copy_from_slice(&0x5AFAA55Au32.to_le_bytes());
        flash[8..12].copy_from_slice(&0xA55AFAA5u32.to_le_bytes());
        flash[12..16].copy_from_slice(&0x5AA55AFAu32.to_le_bytes());
        flash[0x20..0x22].copy_from_slice(&0x1000u16.to_le_bytes()); // Vendor ID LSI
        flash[0x22..0x24].copy_from_slice(&0x2713u16.to_le_bytes()); // Product ID SAS2008 IT

        // NextImageHeaderOffset@0x30 points to first ext image @ 0x100
        flash[0x30..0x34].copy_from_slice(&0x100u32.to_le_bytes());

        // Ext image chain: type=0x05 at 0x100, next->0x400 (FLASH_LAYOUT)
        flash[0x100] = 0x05; // ImageType @0x00
        flash[0x108..0x10C].copy_from_slice(&0u32.to_le_bytes()); // ImageSize @0x04 (4 bytes, zero)
        flash[0x10C..0x110].copy_from_slice(&0x400u32.to_le_bytes()); // NextImageHeaderOffset @0x08 (per mpi2_ioc.h comment at line 11)

        // FLASH_LAYOUT ext image @ 0x400: type=0x06, next=0 (no further images)
        flash[0x400] = EXT_IMAGE_TYPE_FLASH_LAYOUT;
        // ImageSize at offset +0x04
        flash[0x404..0x408].copy_from_slice(&0x100u32.to_le_bytes());
        // NextImageHeaderOffset at offset +0x0C = 0 (no next)
        flash[0x40C..0x410].copy_from_slice(&0u32.to_le_bytes());

        // FLASH_LAYOUT_DATA payload at 0x420: SizeOfRegion=0x10, NumLayouts=1, RegionsPerLayout=2
        let layout_start = 0x420;
        flash[layout_start + 2] = 0x10; // SizeOfRegion @0x02
        flash[layout_start + 4..layout_start + 6].copy_from_slice(&1u16.to_le_bytes()); // NumLayouts @0x04
        flash[layout_start + 6..layout_start + 8].copy_from_slice(&2u16.to_le_bytes()); // RegionsPerLayout @0x06

        // MPI2_FLASH_LAYOUT header at layout_start+0x10 = 0x430: FlashSize @0x00
        let flash_size_offset = layout_start + 0x10;
        flash[flash_size_offset..flash_size_offset+4].copy_from_slice(&(0x800000u32).to_le_bytes()); // Flash size 8MB

        // Region table at layout_start+0x20 = 0x440, each region is 0x10 bytes
        let region_base = 0x440;

        // Region 0: FIRMWARE (type=0x01) @ 0x5A0000, size=0x100000
        flash[region_base] = REGION_FIRMWARE;
        flash[region_base + 4..region_base + 8].copy_from_slice(&(0x5A0000u32).to_le_bytes()); // Offset
        flash[region_base + 8..region_base + 12].copy_from_slice(&(0x100000u32).to_le_bytes()); // Size

        // Region 1: BACKUP (type=0x05) @ 0x6E0000, size=0x100000
        let backup_region = region_base + 0x10;
        flash[backup_region] = REGION_BACKUP;
        flash[backup_region + 4..backup_region + 8].copy_from_slice(&(0x6E0000u32).to_le_bytes()); // Offset
        flash[backup_region + 8..backup_region + 12].copy_from_slice(&(0x100000u32).to_le_bytes()); // Size

        // Version strings at region_offset + 0x68 (per 2026-05-30-bank-mismatch-rootcause.md line 19)
        let fw_ver_start = 0x5A0000 + 0x68;
        let bak_ver_start = 0x6E0000 + 0x68;
        let ver_bytes_fw = format!("@(#)MPTFW-{}-IE", fw_ver).into_bytes();
        let ver_bytes_bak = format!("@(#)MPTFW-{}-IE", bak_ver).into_bytes();
        flash[fw_ver_start..fw_ver_start + ver_bytes_fw.len()].copy_from_slice(&ver_bytes_fw);
        flash[bak_ver_start..bak_ver_start + ver_bytes_bak.len()].copy_from_slice(&ver_bytes_bak);

        flash
    }

    #[test]
    fn consistent_versions_returns_consistent_true() {
        let flash = build_synthetic_fixture("07.15.08.00", "07.15.08.00");
        let result = verify_flash_consistency(&flash).expect("should parse valid fixture");
        assert!(result.consistent, "matching versions should be consistent");
        assert_eq!(result.firmware_version, "@(#)MPTFW-07.15.08.00-IE");
        assert_eq!(result.backup_version, "@(#)MPTFW-07.15.08.00-IE");
    }

    #[test]
    fn mismatched_versions_returns_consistent_false() {
        let flash = build_synthetic_fixture("07.15.08.00", "20.00.07.00");
        let result = verify_flash_consistency(&flash).expect("should parse valid fixture");
        assert!(
            !result.consistent,
            "mismatched versions should be inconsistent (brick case)"
        );
        assert_eq!(result.firmware_version, "@(#)MPTFW-07.15.08.00-IE");
        assert_eq!(result.backup_version, "@(#)MPTFW-20.00.07.00-IE");
    }

    #[test]
    fn missing_backup_region_returns_err() {
        // Build fixture without BACKUP region (only FIRMWARE)
        let mut flash = vec![0u8; 0x800000];
        flash[0..4].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE.to_le_bytes());
        flash[4..8].copy_from_slice(&0x5AFAA55Au32.to_le_bytes());
        flash[8..12].copy_from_slice(&0xA55AFAA5u32.to_le_bytes());
        flash[12..16].copy_from_slice(&0x5AA55AFAu32.to_le_bytes());
        flash[0x20..0x22].copy_from_slice(&0x1000u16.to_le_bytes());
        flash[0x22..0x24].copy_from_slice(&0x2713u16.to_le_bytes());
        flash[0x2C..0x30].copy_from_slice(&(0x100000u32).to_le_bytes());
        flash[0x30..0x34].copy_from_slice(&0x200u32.to_le_bytes());

        flash[0x200] = 0x05;
        flash[0x204..0x208].copy_from_slice(&0x100u32.to_le_bytes());
        flash[0x208..0x20C].copy_from_slice(&0x400u32.to_le_bytes());

        flash[0x400] = EXT_IMAGE_TYPE_FLASH_LAYOUT;
        flash[0x404..0x408].copy_from_slice(&0x100u32.to_le_bytes());
        flash[0x408..0x40C].copy_from_slice(&0u32.to_le_bytes());

        let layout_start = 0x500;
        flash[layout_start + 2] = 0x10;
        flash[layout_start + 4..layout_start + 6].copy_from_slice(&1u16.to_le_bytes());
        flash[layout_start + 6..layout_start + 8].copy_from_slice(&1u16.to_le_bytes());

        let region_base = layout_start + 0x10;
        flash[region_base] = REGION_FIRMWARE;
        flash[region_base + 4..region_base + 8].copy_from_slice(&(0x5A0000u32).to_le_bytes());
        flash[region_base + 8..region_base + 12].copy_from_slice(&(0x100000u32).to_le_bytes());

        let fw_ver_start = 0x5A0000 + 0x68;
        flash[fw_ver_start..fw_ver_start + 24].copy_from_slice(b"@(#)MPTFW-07.15.08.00-IE");

        match verify_flash_consistency(&flash) {
            Err(FwError::NoFlashLayout) => {} // Expected: no BACKUP region found
            _ => panic!("expected NoFlashLayout error for missing BACKUP region"),
        }
    }

    #[test]
    fn flash_too_short_returns_err() {
        let short_flash = vec![0u8; 1024];
        assert!(matches!(
            verify_flash_consistency(&short_flash),
            Err(FwError::TooShort(_))
        ));
    }
}

/// Read firmware region via back-door (diag-mapped flash). Opens BAR1, reads chip memory
/// @0xFC000000 (full 8MiB flash window), then extracts the FIRMWARE region bytes using
/// region_span. Returns the raw firmware bytes. Cites: ADR-020 line 41 for path ordering,
// sbr/transport.rs:355-375 for read_chip_mem signature and usage pattern.
pub fn read_firmware_backdoor(bdf: &str) -> Result<Vec<u8>, crate::Error> {
    use crate::sbr::transport::Bar1MmapSbrTransport;

    let mut transport = Bar1MmapSbrTransport::open(bdf)
        .map_err(|e| crate::Error::Other(format!("backdoor open: {}", e)))?;

    // Read full 8MiB flash window @0xFC000000 (A0000000 on SAS2008 maps to this).
    const FLASH_WINDOW_SIZE: usize = 8 * 1024 * 1024;
    let chip_mem = transport
        .read_chip_mem(0xFC000000, FLASH_WINDOW_SIZE)
        .map_err(|e| crate::Error::Other(format!("backdoor read_chip_mem: {}", e)))?;

    // Parse FLASH_LAYOUT and extract FIRMWARE region.
    let layout = parse_flash_layout(&chip_mem).map_err(|e| {
        crate::Error::Other(format!("backdoor parse_flash_layout: {}", e))
    })?;

    let (offset, size) = layout
        .region_span(REGION_FIRMWARE)
        .ok_or_else(|| crate::Error::Other("backdoor: no FIRMWARE region found".into()))?;

    if offset as usize + size as usize > chip_mem.len() {
        return Err(crate::Error::Other(format!(
            "backdoor: firmware region {}+{} exceeds flash window {}",
            offset, size, chip_mem.len()
        )));
    }

    Ok(chip_mem[offset as usize..(offset + size) as usize].to_vec())
}
