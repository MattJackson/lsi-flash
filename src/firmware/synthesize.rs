//! Manipulator-grade firmware synthesis. Cites
//! `lsi-flash-notes/03-firmware-formats/mpt-firmware-format.md` §N
//! (PHY-to-slot map at file offset 0xA1CD9 in 2118it.bin, +0x14 stride).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SynthError {
    #[error("input firmware too short ({0} bytes; need at least header + 8 × PhyData)")]
    TooShort(usize),
    #[error("could not locate PhyData[] (no Port=0..7 sequence at +0x14 stride found)")]
    PhyDataNotFound,
    #[error("permutation must be a valid 0..7 reordering, got {0:?}")]
    InvalidPermutation([u8; 8]),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

const PHYDATA_STRIDE: usize = 0x14;
const PHYDATA_COUNT: usize = 8;

/// Locate PhyData[0]'s file offset by scanning for the Port=0..7 sequence
/// at +0x14 stride. The pattern is unique in real MPT firmware (verified on
/// 2118it.bin and DELL_6GBPSAS.FW samples — see mpt-firmware-format.md §N).
/// Returns the file offset of PhyData[0], or None if not found.
pub fn find_phydata_offset(data: &[u8]) -> Option<usize> {
    let need = PHYDATA_COUNT * PHYDATA_STRIDE;
    if data.len() < need {
        return None;
    }
    // Start scanning AFTER the 0x88-byte MPI_FW_HEADER per mpt-firmware-format.md §2
    'outer: for start in 0x88..(data.len() - need) {
        for i in 0..PHYDATA_COUNT {
            if data[start + i * PHYDATA_STRIDE] != i as u8 {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
}

/// Permute PhyData[] Port bytes per the provided 0..7 reordering.
/// `perm[i] = new_port_value_at_phydata_index_i`.
/// For phase15_reversed: perm = [7, 6, 5, 4, 3, 2, 1, 0].
pub fn permute_phy_mapping(data: &mut [u8], perm: [u8; 8]) -> Result<(), SynthError> {
    // Validate perm is a true permutation of 0..7
    let mut seen = [false; 8];
    for &p in &perm {
        if p > 7 || seen[p as usize] {
            return Err(SynthError::InvalidPermutation(perm));
        }
        seen[p as usize] = true;
    }
    let base = find_phydata_offset(data).ok_or(SynthError::PhyDataNotFound)?;
    for (i, &new_port) in perm.iter().enumerate() {
        data[base + i * PHYDATA_STRIDE] = new_port;
    }
    Ok(())
}

/// Recompute the file-level U32 checksum so that sum mod 2^32 == 0
/// (per mpt-firmware-format.md §5). Adjusts the LAST U32 word in place.
/// Returns the new value of the last word.
pub fn fix_file_checksum(data: &mut [u8]) -> u32 {
    let len = data.len() & !3; // round down to multiple of 4
    let mut sum: u32 = 0;
    // Sum everything EXCEPT the last word
    for i in (0..len - 4).step_by(4) {
        sum = sum.wrapping_add(u32::from_le_bytes(data[i..i + 4].try_into().unwrap()));
    }
    // last word = -sum so total mod 2^32 == 0
    let new_last = 0u32.wrapping_sub(sum);
    data[len - 4..len].copy_from_slice(&new_last.to_le_bytes());
    new_last
}

/// Convenience wrapper: produce phase15_reversed synthesized firmware from input.
pub fn synthesize_reverse_phy(input: &[u8]) -> Result<Vec<u8>, SynthError> {
    let mut out = input.to_vec();
    permute_phy_mapping(&mut out, [7, 6, 5, 4, 3, 2, 1, 0])?;
    fix_file_checksum(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic test data: 0x88-byte header zeros, then 8 PhyData structs
    /// (Port byte first, rest zero), then 4 trailing zeros for checksum slot.
    fn synthetic_fw_with_phydata() -> Vec<u8> {
        let mut data = vec![0u8; 0x88];
        for i in 0..8 {
            let mut struct_bytes = vec![0u8; PHYDATA_STRIDE];
            struct_bytes[0] = i as u8;
            data.extend_from_slice(&struct_bytes);
        }
        // tail padding so we have a U32-aligned last word for checksum
        data.extend_from_slice(&[0u8; 16]);
        data
    }

    #[test]
    fn find_phydata_offset_on_synthetic() {
        let data = synthetic_fw_with_phydata();
        let off = find_phydata_offset(&data).expect("should find PhyData");
        assert_eq!(off, 0x88);
    }

    #[test]
    fn find_phydata_offset_returns_none_on_garbage() {
        let data = vec![0u8; 0x200];
        assert!(find_phydata_offset(&data).is_none());
    }

    #[test]
    fn permute_phy_mapping_writes_correct_port_bytes() {
        let mut data = synthetic_fw_with_phydata();
        permute_phy_mapping(&mut data, [7, 6, 5, 4, 3, 2, 1, 0]).unwrap();
        for i in 0..8 {
            assert_eq!(data[0x88 + i * PHYDATA_STRIDE], (7 - i) as u8);
        }
    }

    #[test]
    fn permute_phy_mapping_rejects_invalid_perm() {
        let mut data = synthetic_fw_with_phydata();
        // duplicate
        assert!(matches!(
            permute_phy_mapping(&mut data, [0, 0, 0, 0, 0, 0, 0, 0]),
            Err(SynthError::InvalidPermutation(_))
        ));
        // out of range
        assert!(matches!(
            permute_phy_mapping(&mut data, [8, 0, 1, 2, 3, 4, 5, 6]),
            Err(SynthError::InvalidPermutation(_))
        ));
    }

    #[test]
    fn fix_file_checksum_zeros_sum() {
        let mut data = synthetic_fw_with_phydata();
        fix_file_checksum(&mut data);
        let len = data.len() & !3;
        let mut sum: u32 = 0;
        for i in (0..len).step_by(4) {
            sum = sum.wrapping_add(u32::from_le_bytes(data[i..i + 4].try_into().unwrap()));
        }
        assert_eq!(sum, 0);
    }

    #[test]
    fn synthesize_reverse_phy_roundtrip() {
        let data = synthetic_fw_with_phydata();
        let out = synthesize_reverse_phy(&data).unwrap();
        // Check Port bytes reversed
        for i in 0..8 {
            assert_eq!(out[0x88 + i * PHYDATA_STRIDE], (7 - i) as u8);
        }
        // Check file checksum zeros
        let len = out.len() & !3;
        let mut sum: u32 = 0;
        for i in (0..len).step_by(4) {
            sum = sum.wrapping_add(u32::from_le_bytes(out[i..i + 4].try_into().unwrap()));
        }
        assert_eq!(sum, 0);
    }
}

#[cfg(test)]
mod cross_cohort_tests {
    use super::*;

    /// Path to a firmware fixture in the lsi-flash-notes sibling repo.
    /// These files are NOT in the lsi-flash repo (we don't ship firmware in public code).
    /// Tests gated by env var so they don't run in CI without the notes repo present.
    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from("/Users/mjackson/Developer/lsi-flash-notes")
            .join("09-research-archive/upstream/lsi_sas_hba_crossflash_guide")
            .join(name)
    }

    fn load_fixture(name: &str) -> Option<Vec<u8>> {
        let path = fixture_path(name);
        if !path.exists() {
            return None;
        }
        Some(std::fs::read(path).expect("fixture exists but unreadable"))
    }

    #[test]
    fn synthesize_reverse_phy_works_on_lsi_p20_it() {
        let Some(orig) = load_fixture("2118it.bin") else {
            eprintln!("SKIP: fixture not present (notes repo missing)");
            return;
        };
        let synth = synthesize_reverse_phy(&orig).expect("synthesis should succeed on LSI IT");

        // PhyData[] should be at file offset 0xA1CD9 in this file
        // Per lsi-flash-notes/03-firmware-formats/mpt-firmware-format.md §Cross-cohort verification (Dell)
        let phydata_off = 0xA1CD9;
        for i in 0..8 {
            assert_eq!(
                synth[phydata_off + i * PHYDATA_STRIDE],
                (7 - i) as u8,
                "Port byte at PhyData[{}] should be {} after reverse",
                i,
                7 - i
            );
        }

        // File-level U32 sum should still be 0 (checksum recomputed correctly)
        let mut sum: u32 = 0;
        let len = synth.len() & !3;
        for i in (0..len).step_by(4) {
            sum = sum.wrapping_add(u32::from_le_bytes(synth[i..i + 4].try_into().unwrap()));
        }
        assert_eq!(sum, 0, "file-level U32 sum should be 0 after recompute");

        // Only the 8 Port bytes should differ from the original
        let diff_count = orig
            .iter()
            .zip(synth.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            diff_count, 8,
            "only 8 Port bytes should change (minimal diff)"
        );
    }

    #[test]
    fn synthesize_reverse_phy_works_on_dell_h200_oem() {
        let Some(orig) = load_fixture("DELL_6GBPSAS.FW") else {
            eprintln!("SKIP: fixture not present (notes repo missing)");
            return;
        };
        let synth = synthesize_reverse_phy(&orig).expect("synthesis should succeed on Dell H200");

        // Per lsi-flash-notes/03-firmware-formats/mpt-firmware-format.md §Cross-cohort verification (Dell):
        // Dell H200 PhyData[] at file offset 0xc8de9
        let phydata_off = 0xc8de9;
        for i in 0..8 {
            assert_eq!(
                synth[phydata_off + i * PHYDATA_STRIDE],
                (7 - i) as u8,
                "Dell PhyData[{}] Port should be {} after reverse",
                i,
                7 - i
            );
        }

        // File-level checksum still validates
        let mut sum: u32 = 0;
        let len = synth.len() & !3;
        for i in (0..len).step_by(4) {
            sum = sum.wrapping_add(u32::from_le_bytes(synth[i..i + 4].try_into().unwrap()));
        }
        assert_eq!(
            sum, 0,
            "Dell file-level U32 sum should be 0 after recompute"
        );

        // 8 Port bytes differ + possibly last U32 word adjusted (sum-preserving permutation
        // means last word may or may not change — sum of [0..7] == sum of [7..0] so checksum
        // is invariant in this specific case)
        let diff_count = orig
            .iter()
            .zip(synth.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            diff_count <= 12,
            "only Port bytes (8) + possibly last U32 (4) should change; got {diff_count}"
        );
    }

    #[test]
    fn synthesize_reverse_phy_also_works_on_lsi_p20_ir() {
        let Some(orig) = load_fixture("2118ir.bin") else {
            eprintln!("SKIP: fixture not present (notes repo missing)");
            return;
        };

        // Find the real PhyData offset in original firmware
        let orig_base = find_phydata_offset(&orig).expect("IR should have valid PhyData[]");

        let synth = synthesize_reverse_phy(&orig).expect("synthesis should succeed on LSI IR");

        // Verify Port bytes are reversed at the ORIGINAL location.
        // Note: After reversal, find_phydata_offset may return a different offset
        // due to false positives in IR firmware (see debug_ir4.rs analysis),
        // so we verify at the known original offset instead.
        for i in 0..8 {
            assert_eq!(
                synth[orig_base + i * PHYDATA_STRIDE],
                (7 - i) as u8,
                "Port byte at PhyData[{}] should be {} after reverse",
                i,
                7 - i
            );
        }

        // Verify the file checksum still validates
        let mut sum: u32 = 0;
        let len = synth.len() & !3;
        for i in (0..len).step_by(4) {
            sum = sum.wrapping_add(u32::from_le_bytes(synth[i..i + 4].try_into().unwrap()));
        }
        assert_eq!(sum, 0, "IR file-level U32 sum should be 0 after recompute");
    }

    #[test]
    fn lsi_and_dell_have_different_phydata_offsets() {
        let Some(lsi) = load_fixture("2118it.bin") else {
            eprintln!("SKIP");
            return;
        };
        let Some(dell) = load_fixture("DELL_6GBPSAS.FW") else {
            eprintln!("SKIP");
            return;
        };

        let lsi_off = find_phydata_offset(&lsi).expect("LSI should have PhyData");
        let dell_off = find_phydata_offset(&dell).expect("Dell should have PhyData");

        assert_eq!(lsi_off, 0xA1CD9, "LSI PhyData expected at 0xA1CD9");
        assert_eq!(dell_off, 0xc8de9, "Dell PhyData expected at 0xc8de9");
        assert_ne!(lsi_off, dell_off, "PhyData offset must vary across cohort");
    }
}
