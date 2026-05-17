//! SBR parsing and building module.
//! Port of sbrtool.py: parse/build/checksum operations.

use std::io;

/// MFG block length in bytes (76 bytes including checksum). Cites sbrtool.py:40.
pub const MFG_BLOCK_LEN: usize = 0x4c;

/// Total SBR size. Cites sbrtool.py:92.
pub const SBR_SIZE: usize = 256;

/// WWID block offset within SBR. Cites sbrtool.py:54-58.
pub const WWID_OFFSET: usize = 0xd8; // 216 bytes from start

/// WWID checksum offset. Cites sbrtool.py:57.
pub const WWID_CHECKSUM_OFFSET: usize = 0xef; // 239 bytes from start

/// Checksum magic constant. Cites sbrtool.py:35.
pub const CHECKSUM_MAGIC: u8 = 0x5b;

/// SBR manufacturing data fields. Cites sbrtool.py:4-30.
#[derive(Debug, Clone)]
pub struct MfgFields {
    pub unk00: u32,      // 0x00 - 4 bytes, I (sbrtool.py:5) **OPEN: undocumented**
    pub unk04: u32,      // 0x04 - 4 bytes, I (sbrtool.py:6) **OPEN: undocumented**
    pub unk08: u32,      // 0x08 - 4 bytes, I (sbrtool.py:7) **OPEN: undocumented**
    pub pcivid: u16,     // 0x0C - 2 bytes, H (sbrtool.py:8) PCI Vendor ID
    pub pcipid: u16,     // 0x0E - 2 bytes, H (sbrtool.py:9) PCI Product ID
    pub unk10: u16,      // 0x10 - 2 bytes, H (sbrtool.py:10) **OPEN: undocumented**
    pub hwconfig: u16,   // 0x12 - 2 bytes, H (sbrtool.py:11) **OPEN: undocumented**
    pub subsys_vid: u16, // 0x14 - 2 bytes, H (sbrtool.py:12) Subsystem Vendor ID
    pub subsys_pid: u16, // 0x16 - 2 bytes, H (sbrtool.py:13) Subsystem Device ID
    pub unk18: u32,      // 0x18 - 4 bytes, I (sbrtool.py:14) **OPEN: undocumented**
    pub unk1c: u32,      // 0x1C - 4 bytes, I (sbrtool.py:15) **OPEN: undocumented**
    pub unk20: u32,      // 0x20 - 4 bytes, I (sbrtool.py:16) **OPEN: undocumented**
    pub unk24: u32,      // 0x24 - 4 bytes, I (sbrtool.py:17) **OPEN: undocumented**
    pub unk28: u32,      // 0x28 - 4 bytes, I (sbrtool.py:18) **OPEN: undocumented**
    pub unk2c: u32,      // 0x2C - 4 bytes, I (sbrtool.py:19) **OPEN: undocumented**
    pub unk30: u32,      // 0x30 - 4 bytes, I (sbrtool.py:20) **OPEN: undocumented**
    pub unk34: u32,      // 0x34 - 4 bytes, I (sbrtool.py:21) **OPEN: undocumented**
    pub unk38: u32,      // 0x38 - 4 bytes, I (sbrtool.py:22) **OPEN: undocumented**
    pub unk3c: u32,      // 0x3C - 4 bytes, I (sbrtool.py:23) **OPEN: undocumented**
    pub interface: u8,   // 0x40 - 1 byte, B (sbrtool.py:24) Interface type (0x00=IT/IR, 0x10=iMR)
    pub unk41: u8,       // 0x41 - 1 byte, B (sbrtool.py:25) **OPEN: undocumented**
    pub unk42: u16,      // 0x42 - 2 bytes, H (sbrtool.py:26) **OPEN: undocumented**
    pub unk44: u32,      // 0x44 - 4 bytes, I (sbrtool.py:27) **OPEN: undocumented**
    pub unk48: u16,      // 0x48 - 2 bytes, H (sbrtool.py:28) **OPEN: undocumented**
    pub unk4a: u8,       // 0x4A - 1 byte, B (sbrtool.py:29) **OPEN: undocumented**
}

/// Error type for SBR operations. Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum SbrError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("SBR too short (got {0} bytes, need 256)")]
    TooShort(usize),

    #[error("MFG block too short (got {0} bytes, need 75)")]
    MfgTooShort(usize),

    #[error("Invalid hex value: {0}")]
    ParseHex(#[from] std::num::ParseIntError),

    #[error("Array conversion error: {0}")]
    ArrayConversion(#[from] std::array::TryFromSliceError),
}

/// Compute MFG block checksum. Cites sbrtool.py:35.
pub fn compute_mfg_checksum(mfg_bytes: &[u8]) -> u8 {
    let sum: u16 = mfg_bytes.iter().map(|b| *b as u16).sum();
    let result = (CHECKSUM_MAGIC as u16).wrapping_sub(sum);
    (result & 0xff) as u8
}

/// Compute WWID block checksum. Cites sbrtool.py:57 pattern.
pub fn compute_wwid_checksum(wwid_bytes: &[u8]) -> u8 {
    let sum: u16 = wwid_bytes.iter().map(|b| *b as u16).sum();
    let result = (CHECKSUM_MAGIC as u16).wrapping_sub(sum);
    (result & 0xff) as u8
}

/// Parse SBR manufacturing fields from raw bytes. Cites sbrtool.py:50-52.
fn parse_mfg_fields(data: &[u8]) -> Result<MfgFields, SbrError> {
    if data.len() < 75 {
        return Err(SbrError::MfgTooShort(data.len()));
    }

    Ok(MfgFields {
        unk00: u32::from_le_bytes(data[0..4].try_into().unwrap()),
        unk04: u32::from_le_bytes(data[4..8].try_into().unwrap()),
        unk08: u32::from_le_bytes(data[8..12].try_into().unwrap()),
        pcivid: u16::from_le_bytes(data[12..14].try_into().unwrap()),
        pcipid: u16::from_le_bytes(data[14..16].try_into().unwrap()),
        unk10: u16::from_le_bytes(data[16..18].try_into().unwrap()),
        hwconfig: u16::from_le_bytes(data[18..20].try_into().unwrap()),
        subsys_vid: u16::from_le_bytes(data[20..22].try_into().unwrap()),
        subsys_pid: u16::from_le_bytes(data[22..24].try_into().unwrap()),
        unk18: u32::from_le_bytes(data[24..28].try_into().unwrap()),
        unk1c: u32::from_le_bytes(data[28..32].try_into().unwrap()),
        unk20: u32::from_le_bytes(data[32..36].try_into().unwrap()),
        unk24: u32::from_le_bytes(data[36..40].try_into().unwrap()),
        unk28: u32::from_le_bytes(data[40..44].try_into().unwrap()),
        unk2c: u32::from_le_bytes(data[44..48].try_into().unwrap()),
        unk30: u32::from_le_bytes(data[48..52].try_into().unwrap()),
        unk34: u32::from_le_bytes(data[52..56].try_into().unwrap()),
        unk38: u32::from_le_bytes(data[56..60].try_into().unwrap()),
        unk3c: u32::from_le_bytes(data[60..64].try_into().unwrap()),
        interface: data[64],
        unk41: data[65],
        unk42: u16::from_le_bytes(data[66..68].try_into().unwrap()),
        unk44: u32::from_le_bytes(data[68..72].try_into().unwrap()),
        unk48: u16::from_le_bytes(data[72..74].try_into().unwrap()),
        unk4a: data[74],
    })
}

/// Full SBR structure. Cites sbrtool.py:37-95.
#[derive(Debug, Clone)]
pub struct Sbr {
    pub mfg: MfgFields,
    pub mfg_duplicate_valid: bool, // True if duplicate copy matches
    pub mfg_duplicate: Option<MfgFields>, // Duplicate MFG block (0x4c-0x97)
    pub sas_addr: Option<u64>,     // WWID SAS address (optional)
    pub checksum_valid: bool,      // MFG checksum valid?
    pub wwid_checksum_valid: bool, // WWID checksum valid?
}

/// Parse SBR binary into struct. Cites sbrtool.py:37-60.
pub fn parse_sbr(data: &[u8]) -> Result<Sbr, SbrError> {
    if data.len() < SBR_SIZE {
        return Err(SbrError::TooShort(data.len()));
    }

    // Extract MFG block (first 76 bytes). Cites sbrtool.py:40.
    let mfg_data = &data[0..MFG_BLOCK_LEN];

    // Compute and verify checksum. Cites sbrtool.py:46-48.
    let stored_checksum = mfg_data[MFG_BLOCK_LEN - 1];
    let computed_checksum = compute_mfg_checksum(&mfg_data[..MFG_BLOCK_LEN - 1]);
    let checksum_valid = stored_checksum == computed_checksum;

    if !checksum_valid {
        eprintln!("WARNING: Mfg data checksum error");
    }

    // Parse MFG fields. Cites sbrtool.py:50-52.
    let mfg = parse_mfg_fields(&mfg_data[..MFG_BLOCK_LEN - 1])?;

    // Extract duplicate MFG block (0x4c-0x97). Cites sbrtool.py:41.
    let mfg_dup_data = &data[MFG_BLOCK_LEN..MFG_BLOCK_LEN * 2];
    let stored_checksum_dup = mfg_dup_data[MFG_BLOCK_LEN - 1];
    let computed_checksum_dup = compute_mfg_checksum(&mfg_dup_data[..MFG_BLOCK_LEN - 1]);

    if mfg_data != mfg_dup_data {
        eprintln!("WARNING: Mfg data copies differ, using first");
    }

    // Extract WWID block (0xd8-0xdf). Cites sbrtool.py:54-58.
    let sas_addr_bytes = &data[WWID_OFFSET..WWID_OFFSET + 8];
    let wwid_checksum_valid = if sas_addr_bytes != [0; 8] {
        let stored_wwid_checksum = data[WWID_CHECKSUM_OFFSET];
        let computed_wwid_checksum = compute_wwid_checksum(sas_addr_bytes);
        let valid = stored_wwid_checksum == computed_wwid_checksum;

        if !valid {
            eprintln!("WARNING: SAS address checksum error");
        }

        Some(valid)
    } else {
        None // No WWID present (all zeros)
    };

    let sas_addr = if sas_addr_bytes != [0; 8] {
        Some(u64::from_be_bytes(sas_addr_bytes.try_into().unwrap()))
    } else {
        None
    };

    Ok(Sbr {
        mfg,
        mfg_duplicate_valid: computed_checksum_dup == stored_checksum_dup,
        mfg_duplicate: if computed_checksum_dup == stored_checksum_dup {
            parse_mfg_fields(&mfg_dup_data[..MFG_BLOCK_LEN - 1]).ok()
        } else {
            None
        },
        sas_addr,
        checksum_valid,
        wwid_checksum_valid: wwid_checksum_valid.unwrap_or(true),
    })
}

/// Build SBR binary from MfgFields. Cites sbrtool.py:62-95.
pub fn build_sbr(mfg: &MfgFields, sas_addr: Option<u64>) -> Result<Vec<u8>, SbrError> {
    let mut mfg_bytes = Vec::with_capacity(75);

    mfg_bytes.extend_from_slice(&mfg.unk00.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk04.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk08.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.pcivid.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.pcipid.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk10.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.hwconfig.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.subsys_vid.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.subsys_pid.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk18.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk1c.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk20.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk24.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk28.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk2c.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk30.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk34.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk38.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk3c.to_le_bytes());
    mfg_bytes.push(mfg.interface);
    mfg_bytes.push(mfg.unk41);
    mfg_bytes.extend_from_slice(&mfg.unk42.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk44.to_le_bytes());
    mfg_bytes.extend_from_slice(&mfg.unk48.to_le_bytes());
    mfg_bytes.push(mfg.unk4a);

    let mfg_checksum = compute_mfg_checksum(&mfg_bytes);
    mfg_bytes.push(mfg_checksum);

    let mut sbr = Vec::with_capacity(SBR_SIZE);
    sbr.extend_from_slice(&mfg_bytes);
    sbr.extend_from_slice(&mfg_bytes); // Duplicate copy (sbrtool.py:82)
    sbr.resize(0xd8, 0x00);

    if let Some(sas_addr) = sas_addr {
        sbr.extend_from_slice(&sas_addr.to_be_bytes());
        sbr.resize(0xef, 0x00);
        let wwid_checksum = compute_wwid_checksum(&sbr[WWID_OFFSET..WWID_CHECKSUM_OFFSET]);
        sbr.push(wwid_checksum);
    } else {
        sbr.resize(0xef, 0x00);
        let wwid_checksum = compute_wwid_checksum(&sbr[WWID_OFFSET..WWID_CHECKSUM_OFFSET]);
        sbr.push(wwid_checksum);
    }

    sbr.resize(SBR_SIZE, 0x00);
    assert_eq!(sbr.len(), SBR_SIZE);

    Ok(sbr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden file #1: LSI SAS9211-8i IT/IR (stock baseline). Cites sample_sbr/sbr_sas9211-8i_itir.cfg.
    #[test]
    fn test_parse_sas9211_8i() {
        let mfg = MfgFields {
            unk00: 0x6122f661,
            unk04: 0xb34f36f7,
            unk08: 0x91d700f8,
            pcivid: 0x1000,     // sbr_sas9211-8i_itir.cfg:4
            pcipid: 0x0072,     // sbr_sas9211-8i_itir.cfg:5
            unk10: 0x0000,      // sbr_sas9211-8i_itir.cfg:6
            hwconfig: 0x0107,   // sbr_sas9211-8i_itir.cfg:7
            subsys_vid: 0x1000, // sbr_sas9211-8i_itir.cfg:8
            subsys_pid: 0x3020, // sbr_sas9211-8i_itir.cfg:9
            unk18: 0x00000000,  // sbr_sas9211-8i_itir.cfg:10
            unk1c: 0x00000000,  // sbr_sas9211-8i_itir.cfg:11
            unk20: 0x00000000,  // sbr_sas9211-8i_itir.cfg:12
            unk24: 0x00000000,  // sbr_sas9211-8i_itir.cfg:13
            unk28: 0x00000000,  // sbr_sas9211-8i_itir.cfg:14
            unk2c: 0x00000000,  // sbr_sas9211-8i_itir.cfg:15
            unk30: 0x00000000,  // sbr_sas9211-8i_itir.cfg:16
            unk34: 0x00000000,  // sbr_sas9211-8i_itir.cfg:17
            unk38: 0x00000000,  // sbr_sas9211-8i_itir.cfg:18
            unk3c: 0x00000000,  // sbr_sas9211-8i_itir.cfg:19
            interface: 0x00,    // sbr_sas9211-8i_itir.cfg:20
            unk41: 0x0c,        // sbr_sas9211-8i_itir.cfg:21
            unk42: 0x005d,      // sbr_sas9211-8i_itir.cfg:22
            unk44: 0x145a305c,  // sbr_sas9211-8i_itir.cfg:23
            unk48: 0x0575,      // sbr_sas9211-8i_itir.cfg:24
            unk4a: 0x10,        // sbr_sas9211-8i_itir.cfg:25
        };

        let sbr = build_sbr(&mfg, None).unwrap();
        assert_eq!(sbr.len(), SBR_SIZE);

        let parsed = parse_sbr(&sbr).unwrap();
        assert_eq!(parsed.mfg.pcivid, 0x1000);
        assert_eq!(parsed.mfg.pcipid, 0x0072);
        assert_eq!(parsed.mfg.subsys_vid, 0x1000);
        assert_eq!(parsed.mfg.subsys_pid, 0x3020);
        assert!(parsed.checksum_valid);
    }

    /// Golden file #2: Dell H200. Cites sample_sbr/sbr_dell_h200e_itir.cfg.
    #[test]
    fn test_parse_dell_h200() {
        let mfg = MfgFields {
            unk00: 0x0022f661,
            unk04: 0xb34f2000,
            unk08: 0x91d802f8,
            pcivid: 0x1000,     // sbr_dell_h200e_itir.cfg:4
            pcipid: 0x0072,     // sbr_dell_h200e_itir.cfg:5
            unk10: 0x0000,      // sbr_dell_h200e_itir.cfg:6
            hwconfig: 0x0107,   // sbr_dell_h200e_itir.cfg:7
            subsys_vid: 0x1028, // sbr_dell_h200e_itir.cfg:8
            subsys_pid: 0x1f1c, // sbr_dell_h200e_itir.cfg:9
            unk18: 0x00000000,  // sbr_dell_h200e_itir.cfg:10
            unk1c: 0x00000000,  // sbr_dell_h200e_itir.cfg:11
            unk20: 0x00000000,  // sbr_dell_h200e_itir.cfg:12
            unk24: 0x00000000,  // sbr_dell_h200e_itir.cfg:13
            unk28: 0x00000000,  // sbr_dell_h200e_itir.cfg:14
            unk2c: 0x00000000,  // sbr_dell_h200e_itir.cfg:15
            unk30: 0x00000000,  // sbr_dell_h200e_itir.cfg:16
            unk34: 0x00000000,  // sbr_dell_h200e_itir.cfg:17
            unk38: 0x00000000,  // sbr_dell_h200e_itir.cfg:18
            unk3c: 0x00000000,  // sbr_dell_h200e_itir.cfg:19
            interface: 0x00,    // sbr_dell_h200e_itir.cfg:20
            unk41: 0x0c,        // sbr_dell_h200e_itir.cfg:21
            unk42: 0x005d,      // sbr_dell_h200e_itir.cfg:22
            unk44: 0x145a305c,  // sbr_dell_h200e_itir.cfg:23
            unk48: 0x0575,      // sbr_dell_h200e_itir.cfg:24
            unk4a: 0x00,        // sbr_dell_h200e_itir.cfg:25
        };

        let sbr = build_sbr(&mfg, None).unwrap();
        assert_eq!(sbr.len(), SBR_SIZE);

        let parsed = parse_sbr(&sbr).unwrap();
        assert_eq!(parsed.mfg.pcivid, 0x1000);
        assert_eq!(parsed.mfg.pcipid, 0x0072);
        assert_eq!(parsed.mfg.subsys_vid, 0x1028); // Dell VID
        assert_eq!(parsed.mfg.subsys_pid, 0x1f1c); // H200 PID
        assert!(parsed.checksum_valid);
    }

    /// Golden file #3: Fujitsu D2607. Cites sample_sbr/sbr_fujitsu_d2607_itir.cfg.
    #[test]
    fn test_parse_fujitsu_d2607() {
        let mfg = MfgFields {
            unk00: 0x6122f661,
            unk04: 0xb34f36f7,
            unk08: 0x91d700f8,
            pcivid: 0x1000,     // sbr_fujitsu_d2607_itir.cfg:4
            pcipid: 0x0072,     // sbr_fujitsu_d2607_itir.cfg:5
            unk10: 0x0000,      // sbr_fujitsu_d2607_itir.cfg:6
            hwconfig: 0x0104,   // sbr_fujitsu_d2607_itir.cfg:7
            subsys_vid: 0x1734, // sbr_fujitsu_d2607_itir.cfg:8
            subsys_pid: 0x1177, // sbr_fujitsu_d2607_itir.cfg:9
            unk18: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:10
            unk1c: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:11
            unk20: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:12
            unk24: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:13
            unk28: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:14
            unk2c: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:15
            unk30: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:16
            unk34: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:17
            unk38: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:18
            unk3c: 0x00000000,  // sbr_fujitsu_d2607_itir.cfg:19
            interface: 0x00,    // sbr_fujitsu_d2607_itir.cfg:20
            unk41: 0x0c,        // sbr_fujitsu_d2607_itir.cfg:21
            unk42: 0x005d,      // sbr_fujitsu_d2607_itir.cfg:22
            unk44: 0x145a305c,  // sbr_fujitsu_d2607_itir.cfg:23
            unk48: 0x0575,      // sbr_fujitsu_d2607_itir.cfg:24
            unk4a: 0x10,        // sbr_fujitsu_d2607_itir.cfg:25
        };

        let sbr = build_sbr(&mfg, None).unwrap();
        assert_eq!(sbr.len(), SBR_SIZE);

        let parsed = parse_sbr(&sbr).unwrap();
        assert_eq!(parsed.mfg.pcivid, 0x1000);
        assert_eq!(parsed.mfg.pcipid, 0x0072);
        assert_eq!(parsed.mfg.subsys_vid, 0x1734); // Fujitsu VID
        assert_eq!(parsed.mfg.subsys_pid, 0x1177); // D2607 PID
        assert!(parsed.checksum_valid);
    }

    #[test]
    fn test_checksum_calculation() {
        let data: Vec<u8> = vec![0x00, 0x01, 0x02];
        let checksum = compute_mfg_checksum(&data);
        // (0x5b - (0+1+2)) & 0xff = (0x5b - 3) & 0xff = 0x58
        assert_eq!(checksum, 0x58);
    }

    #[test]
    fn test_parse_sbr_too_short() {
        let data: Vec<u8> = vec![0x00; 128];
        let result = parse_sbr(&data);
        assert!(matches!(result, Err(SbrError::TooShort(128))));
    }

    #[test]
    fn test_build_with_sas_addr() {
        let mfg = MfgFields {
            unk00: 0x6122f661,
            unk04: 0xb34f36f7,
            unk08: 0x91d700f8,
            pcivid: 0x1000,
            pcipid: 0x0072,
            unk10: 0x0000,
            hwconfig: 0x0107,
            subsys_vid: 0x1000,
            subsys_pid: 0x3020,
            unk18: 0x00000000,
            unk1c: 0x00000000,
            unk20: 0x00000000,
            unk24: 0x00000000,
            unk28: 0x00000000,
            unk2c: 0x00000000,
            unk30: 0x00000000,
            unk34: 0x00000000,
            unk38: 0x00000000,
            unk3c: 0x00000000,
            interface: 0x00,
            unk41: 0x0c,
            unk42: 0x005d,
            unk44: 0x145a305c,
            unk48: 0x0575,
            unk4a: 0x10,
        };

        let sas_addr = 0x0014380b00000001;
        let sbr = build_sbr(&mfg, Some(sas_addr)).unwrap();

        assert_eq!(sbr.len(), SBR_SIZE);

        let parsed = parse_sbr(&sbr).unwrap();
        assert_eq!(parsed.sas_addr, Some(sas_addr));
    }
}
