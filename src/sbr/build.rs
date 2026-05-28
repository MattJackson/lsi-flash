//! SBR builder — synthesize a 256-byte SBR from a base template + identity.
//!
//! Wired ahead of its consumer: the only caller is `sbr build`
//! (`crate::cli::sbr::run_build`), which is currently a deliberate stub
//! pending SBR template resources (`resources/sbr/<vendor>.sbr`). Until that
//! lands, every item here is dead in non-test builds — hence the
//! module-level allow. This is intentional, not forgotten; remove the allow
//! once `run_build` calls `build_sbr`.
#![allow(dead_code)]

use crate::sbr::parse::{MFG_OFFSET_BACKUP, MFG_OFFSET_PRIMARY};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum BuildError {
    #[error("unknown identity {0:?} — not in card-database.toml")]
    UnknownIdentity(String),
    #[error("invalid SAS WWN: {reason}")]
    InvalidWwn { reason: String },
    #[error("template SBR is corrupt: {0}")]
    InvalidTemplate(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub(crate) struct IdentityPayload {
    pub pci_vid: u16,
    pub pci_did: u16,
    pub subsys_vid: u16,
    pub subsys_pid: u16,
    pub sas_wwn: u64,
    pub board_name: String,
}

pub(crate) fn build_sbr(
    template: &[u8],
    identity: &IdentityPayload,
) -> Result<[u8; 256], BuildError> {
    if template.len() != 256 {
        return Err(BuildError::InvalidTemplate(format!(
            "template must be 256 bytes, got {}",
            template.len()
        )));
    }

    let wwn_oui = ((identity.sas_wwn >> 40) & 0xFFFFFF) as u32;
    if wwn_oui != 0x500605 {
        return Err(BuildError::InvalidWwn {
            reason: format!(
                "OUI mismatch: got 0x{:06X}, expected 0x500605 (LSI)",
                wwn_oui
            ),
        });
    }

    let mut out = [0u8; 256];
    out.copy_from_slice(template);

    write_mfg_block(&mut out, MFG_OFFSET_PRIMARY, identity)?;
    write_mfg_block(&mut out, MFG_OFFSET_BACKUP, identity)?;

    fix_mfg_checksum(&mut out, MFG_OFFSET_PRIMARY);
    fix_mfg_checksum(&mut out, MFG_OFFSET_BACKUP);

    if identity.sas_wwn != 0 {
        write_wwid_block(&mut out, identity.sas_wwn);
    }

    Ok(out)
}

fn write_mfg_block(
    out: &mut [u8; 256],
    base: usize,
    id: &IdentityPayload,
) -> Result<(), BuildError> {
    const OFF_PCI_VID: usize = 0x0C;
    const OFF_PCI_DID: usize = 0x0E;
    const OFF_SUBSYS_VID: usize = 0x14;
    const OFF_SUBSYS_PID: usize = 0x16;

    out[base + OFF_PCI_VID..base + OFF_PCI_VID + 2].copy_from_slice(&id.pci_vid.to_le_bytes());
    out[base + OFF_PCI_DID..base + OFF_PCI_DID + 2].copy_from_slice(&id.pci_did.to_le_bytes());
    out[base + OFF_SUBSYS_VID..base + OFF_SUBSYS_VID + 2]
        .copy_from_slice(&id.subsys_vid.to_le_bytes());
    out[base + OFF_SUBSYS_PID..base + OFF_SUBSYS_PID + 2]
        .copy_from_slice(&id.subsys_pid.to_le_bytes());

    Ok(())
}

fn write_wwid_block(out: &mut [u8; 256], sas_wwn: u64) {
    out[0xd8..0xe0].copy_from_slice(&sas_wwn.to_be_bytes());
    let sum: u16 = out[0xd8..0xef].iter().map(|&b| b as u16).sum();
    out[0xef] = (0x5bu16.wrapping_sub(sum) & 0xff) as u8;
}

fn fix_mfg_checksum(out: &mut [u8; 256], base: usize) {
    const MFG_PAYLOAD_LEN: usize = 0x4b;
    let sum: u32 = out[base..base + MFG_PAYLOAD_LEN]
        .iter()
        .map(|&b| b as u32)
        .sum();
    out[base + MFG_PAYLOAD_LEN] = 0x5bu8.wrapping_sub(sum as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lsi_id() -> IdentityPayload {
        IdentityPayload {
            pci_vid: 0x1000,
            pci_did: 0x0072,
            subsys_vid: 0x1000,
            subsys_pid: 0x3020,
            sas_wwn: 0x5006_05b1_2345_6789,
            board_name: "9211-8i".into(),
        }
    }

    fn dell_id() -> IdentityPayload {
        IdentityPayload {
            pci_vid: 0x1000,
            pci_did: 0x0072,
            subsys_vid: 0x1028,
            subsys_pid: 0x1f1d,
            sas_wwn: 0x5006_05b1_dead_beef,
            board_name: "PERC H200 Adapter".into(),
        }
    }

    fn tpl() -> Vec<u8> {
        vec![0u8; 256]
    }

    #[test]
    fn build_rejects_invalid_template_size() {
        assert!(build_sbr(&[0u8; 100], &lsi_id()).is_err());
    }

    #[test]
    fn build_rejects_invalid_oui() {
        let mut id = lsi_id();
        id.sas_wwn = 0x1234_5678_9abc_def0;
        assert!(matches!(
            build_sbr(&tpl(), &id),
            Err(BuildError::InvalidWwn { .. })
        ));
    }

    #[test]
    fn build_writes_pci_identity_into_both_mfg_copies() {
        let sbr = build_sbr(&tpl(), &dell_id()).unwrap();
        assert_eq!(
            &sbr[MFG_OFFSET_PRIMARY + 0x14..MFG_OFFSET_PRIMARY + 0x18],
            &[0x28, 0x10, 0x1d, 0x1f]
        );
        assert_eq!(
            &sbr[MFG_OFFSET_BACKUP + 0x14..MFG_OFFSET_BACKUP + 0x18],
            &[0x28, 0x10, 0x1d, 0x1f]
        );
    }

    #[test]
    fn build_checksum_verifies() {
        let sbr = build_sbr(&tpl(), &dell_id()).unwrap();
        for base in [MFG_OFFSET_PRIMARY, MFG_OFFSET_BACKUP] {
            let sum: u32 = sbr[base..=base + 0x4b].iter().map(|&b| b as u32).sum();
            assert_eq!(sum as u8, 0x5b);
        }
    }

    #[test]
    fn build_wwid_block_written() {
        let id = dell_id();
        let sbr = build_sbr(&tpl(), &id).unwrap();
        assert_eq!(&sbr[0xd8..0xe0], &id.sas_wwn.to_be_bytes());
    }

    #[test]
    fn build_roundtrips_through_parse() {
        let id = dell_id();
        let sbr = build_sbr(&tpl(), &id).unwrap();
        let p = crate::sbr::parse::parse_sbr(&sbr).expect("parse");
        assert_eq!(p.mfg.pcivid, 0x1000);
        assert_eq!(p.mfg.subsys_vid, 0x1028);
        assert_eq!(p.mfg.subsys_pid, 0x1f1d);
    }

    #[test]
    fn build_parse_sas9211_golden() {
        let sbr = build_sbr(&tpl(), &lsi_id()).unwrap();
        let p = crate::sbr::parse::parse_sbr(&sbr).expect("parse");
        assert_eq!(p.mfg.pcivid, 0x1000);
        assert_eq!(p.mfg.subsys_pid, 0x3020);
        assert!(p.checksum_valid);
    }
}
