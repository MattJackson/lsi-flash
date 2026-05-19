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
