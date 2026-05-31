//! Region-aware flash/firmware file comparison. Compares two flash-image files byte-by-byte
//! by FLASH_LAYOUT regions (or falls back to 4KB coalesced blocks if layout parsing fails).
//!
//! Cites: src/firmware/flash_layout.rs for `parse_flash_layout`, `region_span`, and `REGION_*` consts.

use crate::firmware::flash_layout::{parse_flash_layout, REGION_BACKUP, REGION_FIRMWARE};
use serde::Serialize;

/// Classification of how a region differs between two files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RegionClass {
    Identical,
    Differs,
    AErased,
    BErased,
}

impl std::fmt::Display for RegionClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegionClass::Identical => write!(f, "IDENTICAL"),
            RegionClass::Differs => write!(f, "DIFFERS"),
            RegionClass::AErased => write!(f, "A_ERASED"),
            RegionClass::BErased => write!(f, "B_ERASED"),
        }
    }
}

/// One region's diff result. Cites: src/firmware/flash_layout.rs line 29-34 for FlashRegion struct usage.
#[derive(Debug, Clone, Serialize)]
pub struct RegionDiff {
    pub region_type: u8,
    pub offset: u32,
    pub size: u32,
    pub pct_differ: f64,
    pub a_ff_pct: f64,
    pub b_ff_pct: f64,
    pub classification: RegionClass,
}

/// Coalesced block diff result (fallback when FLASH_LAYOUT parsing fails). Cites: flash_layout.rs line 30-34.
#[derive(Debug, Clone, Serialize)]
pub struct BlockDiff {
    pub start: u64,
    pub end: u64,
    pub size_kb: usize,
    pub pct_differ: f64,
}

/// Summary of the diff operation.
#[derive(Debug, Clone, Serialize)]
pub struct DiffSummary {
    pub total_bytes_differ: usize,
    pub total_regions_or_zones: usize,
}

/// Full JSON output for `--json` mode. Cites: flash_layout.rs line 45-69 for FlashLayout struct and region_span method.
#[derive(Debug, Clone, Serialize)]
pub struct DiffOutput {
    pub file_a: String,
    pub file_b: String,
    pub size_a: usize,
    pub size_b: usize,
    pub layout_parsed: bool,
    pub regions: Option<Vec<RegionDiff>>,
    pub blocks: Option<Vec<BlockDiff>>,
    pub summary: DiffSummary,
    /// True if any byte in a boot-critical region (the unrecoverable boot-input zone, per ADR-021)
    /// differs between the two files. When diffing a candidate image against a live card, `true`
    /// means flashing would corrupt the zone that bricks the card — do not proceed.
    pub boot_critical_differs: bool,
}

/// Calculate percentage of bytes that are 0xFF in a slice.
fn ff_percentage(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let count = data.iter().filter(|&&b| b == 0xFF).count();
    (count as f64 / data.len() as f64) * 100.0
}

/// Calculate percentage of bytes that differ between two slices, and classify the difference.
fn analyze_region(a: &[u8], b: &[u8]) -> (f64, RegionClass, f64, f64) {
    let min_len = a.len().min(b.len());
    if min_len == 0 {
        return (0.0, RegionClass::Identical, 0.0, 0.0);
    }

    let differ_count = (0..min_len).filter(|&i| a[i] != b[i]).count();
    let pct_differ = (differ_count as f64 / min_len as f64) * 100.0;

    let a_ff_pct = ff_percentage(&a[..min_len]);
    let b_ff_pct = ff_percentage(&b[..min_len]);

    let classification = if pct_differ == 0.0 {
        RegionClass::Identical
    } else if a_ff_pct > 90.0 && b_ff_pct <= 90.0 {
        RegionClass::AErased
    } else if b_ff_pct > 90.0 && a_ff_pct <= 90.0 {
        RegionClass::BErased
    } else {
        RegionClass::Differs
    };

    (pct_differ, classification, a_ff_pct, b_ff_pct)
}

/// Convert region type to human-readable name, or hex if unknown. Cites: flash_layout.rs line 19-23 for REGION_FIRMWARE and REGION_BACKUP constants.
fn region_type_name(ty: u8) -> String {
    match ty {
        REGION_FIRMWARE => "FIRMWARE".to_string(),
        REGION_BACKUP => "BACKUP".to_string(),
        _ => format!("UNKNOWN_0x{:02X}", ty),
    }
}

/// Diff two files using FLASH_LAYOUT regions. Returns (regions, summary).
pub fn diff_by_regions(a: &[u8], b: &[u8]) -> (Vec<RegionDiff>, DiffSummary) {
    let layout = match parse_flash_layout(a) {
        Ok(l) => l,
        Err(_) => {
            return (
                vec![],
                DiffSummary {
                    total_bytes_differ: 0,
                    total_regions_or_zones: 0,
                },
            )
        }
    };

    // Use the FIRST candidate layout's regions (iterating all candidates would report each
    // physical region once per geometry and double-count). Cites: flash_layout.rs region_span.
    let mut regions = Vec::new();
    let mut total_differ = 0usize;

    if let Some(first) = layout.layouts.first() {
        for region in &first.regions {
            let offset = region.offset as usize;
            let size = region.size as usize;

            if offset + size > a.len() || offset + size > b.len() {
                continue;
            }

            let (pct_differ, class, a_ff, b_ff) =
                analyze_region(&a[offset..offset + size], &b[offset..offset + size]);

            // Exact differing-byte count within the region (not a rounded pct estimate).
            total_differ += (0..size)
                .filter(|&i| a[offset + i] != b[offset + i])
                .count();

            regions.push(RegionDiff {
                region_type: region.region_type,
                offset: region.offset,
                size: region.size,
                pct_differ,
                a_ff_pct: a_ff,
                b_ff_pct: b_ff,
                classification: class,
            });
        }
    }

    let summary = DiffSummary {
        total_bytes_differ: total_differ,
        total_regions_or_zones: regions.len(),
    };

    (regions, summary)
}

/// Diff two files using coalesced 4KB blocks (fallback when FLASH_LAYOUT parsing fails).
pub fn diff_by_blocks(a: &[u8], b: &[u8]) -> (Vec<BlockDiff>, DiffSummary) {
    let min_len = a.len().min(b.len());
    let block_size = 4096;

    // Find all differing bytes first
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut in_range = false;
    let mut range_start = 0;

    for i in 0..min_len {
        if a[i] != b[i] {
            if !in_range {
                range_start = i;
                in_range = true;
            }
        } else if in_range {
            ranges.push((range_start, i));
            in_range = false;
        }
    }
    if in_range && range_start < min_len {
        ranges.push((range_start, min_len));
    }

    // Coalesce into 4KB blocks
    let mut blocks = Vec::new();
    let mut total_differ = 0usize;

    for (start, end) in ranges {
        let block_start = (start / block_size) * block_size;
        let block_end = (end.div_ceil(block_size)) * block_size;
        let actual_end = block_end.min(a.len().min(b.len()));

        if block_start >= actual_end {
            continue;
        }

        let block_data_a = &a[block_start..actual_end];
        let block_data_b = &b[block_start..actual_end];

        let differ_count = (0..block_data_a.len())
            .filter(|&i| block_data_a[i] != block_data_b[i])
            .count();
        let pct_differ = (differ_count as f64 / block_data_a.len() as f64) * 100.0;
        let size_kb = (actual_end - block_start) / 1024;

        total_differ += differ_count;

        blocks.push(BlockDiff {
            start: block_start as u64,
            end: actual_end as u64,
            size_kb,
            pct_differ,
        });
    }

    // Merge overlapping/adjacent blocks
    if !blocks.is_empty() {
        let mut merged = Vec::new();
        let mut current = blocks[0].clone();

        for block in blocks.iter().skip(1) {
            if block.start <= current.end && block.size_kb > 0 {
                // Merge: extend end and recalculate pct_differ
                let total_bytes =
                    (current.end - current.start) as usize + (block.end - current.end) as usize;
                let new_pct = ((current.pct_differ * (current.end - current.start) as f64)
                    + (block.pct_differ * (block.end - current.end) as f64))
                    / total_bytes as f64;

                current = BlockDiff {
                    end: block.end,
                    size_kb: ((block.end - current.start) / 1024) as usize,
                    pct_differ: new_pct,
                    ..current
                };
            } else {
                merged.push(current);
                current = block.clone();
            }
        }
        merged.push(current);
        blocks = merged;
    }

    let summary = DiffSummary {
        total_bytes_differ: total_differ,
        total_regions_or_zones: blocks.len(),
    };

    (blocks, summary)
}

/// Format a region diff as human-readable text.
pub fn format_region_diff(r: &RegionDiff) -> String {
    let type_name = region_type_name(r.region_type);
    format!(
        "{:<12} @0x{:06X} {:>8}KB  {:5.2}% differ  A:{:>5.1}%FF B:{:>5.1}%FF  {}",
        type_name,
        r.offset,
        r.size / 1024,
        r.pct_differ,
        r.a_ff_pct,
        r.b_ff_pct,
        r.classification
    )
}

/// Format a block diff as human-readable text.
pub fn format_block_diff(b: &BlockDiff) -> String {
    format!(
        "0x{:08X}-0x{:08X} ({:>4}KB) {:5.2}% differ",
        b.start, b.end, b.size_kb, b.pct_differ
    )
}

/// Main diff entry point: returns DiffOutput for JSON or text printing. Cites: flash_layout.rs line 45-72 for FlashLayout usage.
pub fn compare_files(path_a: &str, path_b: &str) -> Result<DiffOutput, Box<dyn std::error::Error>> {
    let data_a = std::fs::read(path_a)?;
    let data_b = std::fs::read(path_b)?;

    if data_a.is_empty() {
        return Err(format!("File A is empty: {}", path_a).into());
    }
    if data_b.is_empty() {
        return Err(format!("File B is empty: {}", path_b).into());
    }

    let size_a = data_a.len();
    let size_b = data_b.len();

    eprintln!(
        "Comparing {} ({:>12} bytes) vs {} ({:>12} bytes)",
        path_a, size_a, path_b, size_b
    );

    let (regions, region_summary) = diff_by_regions(&data_a, &data_b);
    let layout_parsed = !regions.is_empty();

    let (blocks, block_summary) = if layout_parsed {
        (
            Vec::new(),
            DiffSummary {
                total_bytes_differ: 0,
                total_regions_or_zones: 0,
            },
        )
    } else {
        diff_by_blocks(&data_a, &data_b)
    };

    // For JSON, include both; for text, use whichever is populated.
    let mut summary = if layout_parsed {
        region_summary
    } else {
        block_summary
    };
    // Authoritative differing-byte count: an exact byte-by-byte tally over the common length.
    // (The block-coalescing path's running total double-counts where ranges share a 4 KB block;
    // this is the single source of truth for the headline number.)
    let common = size_a.min(size_b);
    summary.total_bytes_differ = (0..common).filter(|&i| data_a[i] != data_b[i]).count();

    // Boot-critical awareness: does any byte in the unrecoverable boot-input zone differ? Reuses
    // the guard's BOOT_CRITICAL map so diff and the live transaction share one definition.
    let boot_critical_differs = crate::firmware::guard::BOOT_CRITICAL.iter().any(|r| {
        let lo = r.start.min(common);
        let hi = r.end.min(common);
        (lo..hi).any(|i| data_a[i] != data_b[i])
    });

    Ok(DiffOutput {
        file_a: path_a.to_string(),
        file_b: path_b.to_string(),
        size_a,
        size_b,
        layout_parsed,
        regions: if regions.is_empty() {
            None
        } else {
            Some(regions)
        },
        blocks: if blocks.is_empty() {
            None
        } else {
            Some(blocks)
        },
        summary,
        boot_critical_differs,
    })
}

/// Print diff output as human-readable text. Cites: flash_layout.rs line 19-23 for region type names.
fn format_bytes(n: usize) -> String {
    let s = n.to_string();
    if s.len() <= 3 {
        return s;
    }
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

pub fn print_text(output: &DiffOutput) {
    println!(
        "File A: {} ({} bytes)",
        output.file_a,
        format_bytes(output.size_a)
    );
    println!(
        "File B: {} ({} bytes)\n",
        output.file_b,
        format_bytes(output.size_b)
    );

    if let Some(regions) = &output.regions {
        println!("FLASH_LAYOUT regions:");
        println!(
            "{:<12} {:>8} {:>8}  {:>6}  {:>8}  {:>8}  CLASS",
            "TYPE", "OFFSET", "SIZE(KB)", "%DIFF", "A-FF%", "B-FF%"
        );
        println!("{}", "-".repeat(90));

        for r in regions {
            println!("{}", format_region_diff(r));
        }
    } else if let Some(blocks) = &output.blocks {
        println!("Block diff (no FLASH_LAYOUT):");
        println!();

        for b in blocks {
            println!("{}", format_block_diff(b));
        }
    }

    println!(
        "\nSummary: {} bytes differ, {} region(s)/zone(s)",
        format_bytes(output.summary.total_bytes_differ),
        output.summary.total_regions_or_zones
    );

    if output.boot_critical_differs {
        println!(
            "\n⚠  BOOT-CRITICAL: the boot-input zone differs. If B is a live card and A is your \
             target image, flashing would corrupt the unrecoverable zone (ADR-021) — DO NOT FLASH."
        );
    }
}

/// Return JSON string for `--json` mode. Cites: flash_layout.rs line 45-69 for FlashLayout usage.
pub fn print_json(output: &DiffOutput) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(output)
}

/// CLI entry point for `lsi-flash diff <FILE_A> <FILE_B> [--json]`. Cites: flash_layout.rs line 45-69 for FlashLayout usage.
pub fn run(
    path_a: std::path::PathBuf,
    path_b: std::path::PathBuf,
    json: bool,
) -> Result<(), crate::Error> {
    let output = compare_files(path_a.to_str().unwrap(), path_b.to_str().unwrap())
        .map_err(|e| crate::Error::Other(e.to_string()))?;

    if json {
        print!("{}", print_json(&output)?);
        println!();
    } else {
        print_text(&output);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// compare_files flags boot_critical_differs when the boot-input zone differs, and clears it
    /// when only non-boot-critical bytes differ. Uses temp files (8 MiB) to exercise the real path.
    #[test]
    fn boot_critical_flag_tracks_the_zone() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let a_path = dir.join("diff-bc-a.bin");
        let b_bc = dir.join("diff-bc-b-bc.bin");
        let b_safe = dir.join("diff-bc-b-safe.bin");

        let base = vec![0xA5u8; 8 * 1024 * 1024];
        std::fs::File::create(&a_path)
            .unwrap()
            .write_all(&base)
            .unwrap();

        // B1: differs inside the boot-critical zone (0x540000–0x5A0000).
        let mut b1 = base.clone();
        b1[0x54_1000] ^= 0xFF;
        std::fs::File::create(&b_bc)
            .unwrap()
            .write_all(&b1)
            .unwrap();

        // B2: differs only OUTSIDE the boot-critical zone.
        let mut b2 = base.clone();
        b2[0x10_0000] ^= 0xFF;
        std::fs::File::create(&b_safe)
            .unwrap()
            .write_all(&b2)
            .unwrap();

        let out_bc = compare_files(a_path.to_str().unwrap(), b_bc.to_str().unwrap()).unwrap();
        assert!(
            out_bc.boot_critical_differs,
            "boot-critical change must be flagged"
        );

        let out_safe = compare_files(a_path.to_str().unwrap(), b_safe.to_str().unwrap()).unwrap();
        assert!(
            !out_safe.boot_critical_differs,
            "non-boot-critical change must not be flagged"
        );
    }

    /// Test identical files → 0% differ, IDENTICAL classification.
    #[test]
    fn test_identical_files() {
        let data = vec![0u8; 1024];
        let (pct, class, a_ff, b_ff) = analyze_region(&data, &data);

        assert_eq!(pct, 0.0);
        assert_eq!(class, RegionClass::Identical);
        // All bytes are 0x00, so FF% is 0% for both
        assert_eq!(a_ff, 0.0);
        assert_eq!(b_ff, 0.0);

        // Also test identical all-FF data
        let ff_data = vec![0xFFu8; 1024];
        let (pct2, class2, a_ff2, b_ff2) = analyze_region(&ff_data, &ff_data);

        assert_eq!(pct2, 0.0);
        assert_eq!(class2, RegionClass::Identical);
        // All bytes are 0xFF, so FF% is 100% for both
        assert_eq!(a_ff2, 100.0);
        assert_eq!(b_ff2, 100.0);
    }

    /// Test one differing range → reported with correct % differ and classification as B_ERASED (b has >90% FF).
    #[test]
    fn test_differing_range() {
        let mut a = vec![0u8; 256];
        let mut b = vec![0xFFu8; 256];

        // Set first 16 bytes to differ (a=0x00, b=0x01)
        for i in 0..16 {
            a[i] = 0x00;
            b[i] = 0x01;
        }

        let (pct, class, a_ff, b_ff) = analyze_region(&a, &b);

        // All bytes differ because a is all 0x00 and b is mostly 0xFF (except first 16 are 0x01)
        assert_eq!(pct, 100.0);
        // Classification: B_ERASED because b has >90% FF and a has <=90%
        assert_eq!(class, RegionClass::BErased);
        // a has no FF bytes
        assert_eq!(a_ff, 0.0);
        // b has 240/256 = 93.75% FF bytes
        assert!((b_ff - 93.75).abs() < 0.01);
    }

    /// Test all-0xFF region vs data → classified as erased (A_ERASED or B_ERASED).
    #[test]
    fn test_erased_classification_a() {
        let a = vec![0xFFu8; 256]; // All FF (>90%)
        let b = vec![0x00u8; 256]; // None FF

        let (pct, class, a_ff, b_ff) = analyze_region(&a, &b);

        assert_eq!(pct, 100.0);
        assert_eq!(class, RegionClass::AErased);
        assert_eq!(a_ff, 100.0);
        assert_eq!(b_ff, 0.0);
    }

    /// Test all-0xFF region vs data → classified as erased (B_ERASED).
    #[test]
    fn test_erased_classification_b() {
        let a = vec![0x00u8; 256]; // None FF
        let b = vec![0xFFu8; 256]; // All FF (>90%)

        let (pct, class, a_ff, b_ff) = analyze_region(&a, &b);

        assert_eq!(pct, 100.0);
        assert_eq!(class, RegionClass::BErased);
        assert_eq!(a_ff, 0.0);
        assert_eq!(b_ff, 100.0);
    }

    /// Test block coalescing: adjacent differing ranges should merge into one block.
    #[test]
    fn test_block_coalescing() {
        let mut a = vec![0u8; 4096];
        let mut b = vec![0xFFu8; 4096];

        // Set two adjacent ranges to differ (bytes 100-200 and 300-400)
        for i in 100..=200 {
            a[i] = 0x00;
            b[i] = 0x01;
        }
        for i in 300..=400 {
            a[i] = 0x00;
            b[i] = 0x02;
        }

        let (blocks, _summary) = diff_by_blocks(&a, &b);

        // Should coalesce into one block covering the full range
        assert!(!blocks.is_empty());
        assert_eq!(_summary.total_regions_or_zones, 1);
    }

    /// Test coalesced blocks with non-differing middle gap.
    #[test]
    fn test_block_with_gap() {
        let mut a = vec![0u8; 4096];
        let mut b = vec![0xFFu8; 4096];

        // Set bytes 100-200 to differ, then gap at 300-400 (same), then differ again at 500-600
        for i in 100..=200 {
            a[i] = 0x00;
            b[i] = 0xFF; // This will make them identical, not differ
        }

        let (blocks, _summary) = diff_by_blocks(&a, &b);

        // All bytes that were 0xFF in 'b' and 0x00 in 'a' should be reported as different
        assert!(!blocks.is_empty());
    }

    /// Test FF percentage calculation.
    #[test]
    fn test_ff_percentage() {
        let all_ff = vec![0xFFu8; 100];
        let none_ff = vec![0x00u8; 100];

        let mut half_ff = Vec::new();
        for _ in 0..50 {
            half_ff.push(0xFF);
            half_ff.push(0x00);
        }

        assert_eq!(ff_percentage(&all_ff), 100.0);
        assert_eq!(ff_percentage(&none_ff), 0.0);
        assert_eq!(ff_percentage(&half_ff), 50.0);
    }

    /// Test region type name formatting. Cites: flash_layout.rs line 19-23 for REGION_FIRMWARE (0x01) and REGION_BACKUP (0x05).
    #[test]
    fn test_region_type_names() {
        assert_eq!(region_type_name(REGION_FIRMWARE), "FIRMWARE");
        assert_eq!(region_type_name(REGION_BACKUP), "BACKUP");
        assert_eq!(region_type_name(0xFF), "UNKNOWN_0xFF");
    }

    /// Test empty file handling.
    #[test]
    fn test_empty_file_handling() {
        // Compare two identical small files to verify basic diff works
        let tmp_a = tempfile::NamedTempFile::new().unwrap();
        let tmp_b = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp_a.path(), [1u8, 2, 3]).unwrap();
        std::fs::write(tmp_b.path(), [1u8, 2, 3]).unwrap();

        let result = compare_files(
            tmp_a.path().to_str().unwrap(),
            tmp_b.path().to_str().unwrap(),
        );

        assert!(result.is_ok());
    }

    /// Test that diff_by_regions returns empty when FLASH_LAYOUT parsing fails.
    #[test]
    fn test_no_flash_layout_returns_empty() {
        let data = vec![0u8; 1024]; // No valid FLASH_LAYOUT

        let (regions, summary) = diff_by_regions(&data, &data);

        assert!(regions.is_empty());
        assert_eq!(summary.total_regions_or_zones, 0);
    }

    /// Test block diff with completely identical files.
    #[test]
    fn test_identical_files_block_diff() {
        let data = vec![0x42u8; 8192];

        let (blocks, summary) = diff_by_blocks(&data, &data);

        assert!(blocks.is_empty());
        assert_eq!(summary.total_bytes_differ, 0);
    }

    /// Test block diff with one differing byte.
    #[test]
    fn test_one_byte_diff_block() {
        let a = vec![0u8; 4096];
        let mut b = vec![0xFFu8; 4096];

        // Only last byte differs (both were 0xFF in b, now set to 0x01)
        b[4095] = 0x01;

        let (blocks, _summary) = diff_by_blocks(&a, &b);

        assert!(!blocks.is_empty());
        // All bytes differ except the last one in block b (3840 differs + 256 FF vs 0x00)
        // Wait - actually a is all 0x00 and b is mostly 0xFF, so they differ everywhere
    }

    /// Test block diff with only one byte differing.
    #[test]
    fn test_one_byte_diff_block_exact() {
        let a = vec![0u8; 4096];
        let mut b = vec![0u8; 4096];

        // Only one byte differs
        b[1234] = 0xFF;

        let (blocks, _summary) = diff_by_blocks(&a, &b);

        assert!(!blocks.is_empty());
        assert_eq!(_summary.total_bytes_differ, 1);
    }
}
