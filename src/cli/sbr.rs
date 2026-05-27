//! `lsi-flash sbr` — SBR-related subcommands.
//!
//! Per ADR-014 (lsi-flash-notes/01-architecture/adr/014-sbr-verb-and-card-database.md):
//! 6 sub-verbs total. This module implements the OFFLINE three first
//! (no hardware required): `show`, `verify`, `build`. The hardware-bound
//! three (`read`, `write`, `modify`) land in a follow-up.

use clap::Subcommand;
use std::fs;
use std::path::PathBuf;

use crate::card_database::{self, CardInfo};
use crate::sbr::parse::{parse_sbr, Sbr};

/// SBR-related operations.
#[derive(Subcommand, Debug)]
pub enum SbrCommand {
    /// Parse an SBR file and pretty-print every field (offline).
    Show {
        /// Path to the 256-byte SBR file.
        file: PathBuf,
    },

    /// Validate an SBR file's structure and checksums (offline).
    /// Exits non-zero on any failure.
    Verify {
        /// Path to the 256-byte SBR file.
        file: PathBuf,
    },

    /// Synthesize an SBR from a card-database identity (offline).
    /// NOT YET WIRED — needs SBR template binding (resources/sbr/*.sbr).
    Build {
        /// Card identity slug from card-database.toml (e.g. `dell-h200-adapter`).
        #[arg(long = "as", value_name = "IDENTITY")]
        identity: String,

        /// Optional SAS WWN to embed (16 hex chars, no separators).
        #[arg(long, value_name = "WWN")]
        sas_wwn: Option<String>,

        /// Output path. Default: `./<identity>-<timestamp>.sbr`.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },
}

/// Entry point invoked from `cli::run`.
pub fn run(cmd: SbrCommand) -> Result<(), crate::Error> {
    match cmd {
        SbrCommand::Show { file } => run_show(&file),
        SbrCommand::Verify { file } => run_verify(&file),
        SbrCommand::Build {
            identity,
            sas_wwn,
            out,
        } => run_build(&identity, sas_wwn.as_deref(), out.as_deref()),
    }
}

// ---- show -------------------------------------------------------------------

fn run_show(file: &std::path::Path) -> Result<(), crate::Error> {
    let bytes = fs::read(file)?;
    let sbr =
        parse_sbr(&bytes).map_err(|e| crate::Error::Other(format!("SBR parse failed: {}", e)))?;

    let cards = card_database::load_embedded()
        .map_err(|e| crate::Error::Other(format!("card-database load failed: {}", e)))?;
    let identity = card_database::identify_card(
        &cards,
        sbr.mfg.pcivid,
        sbr.mfg.pcipid,
        sbr.mfg.subsys_vid,
        sbr.mfg.subsys_pid,
    );

    print_sbr_summary(file, &sbr, identity);
    Ok(())
}

fn print_sbr_summary(file: &std::path::Path, sbr: &Sbr, identity: Option<&CardInfo>) {
    println!("SBR file:    {}", file.display());
    println!("Size:        256 bytes");
    println!();
    println!(
        "PCI VID:DID       0x{:04x}:0x{:04x}",
        sbr.mfg.pcivid, sbr.mfg.pcipid
    );
    println!(
        "Subsystem ID      0x{:04x}:0x{:04x}",
        sbr.mfg.subsys_vid, sbr.mfg.subsys_pid
    );

    match identity {
        Some(card) => {
            println!("Identity          {} [{}]", card.display, card.slug);
            if !card.quirks.is_empty() {
                println!("Quirks            {:?}", card.quirks);
            }
            if let Some(confirmed) = &card.confirmed_via {
                println!("Confirmed via     {}", confirmed);
            }
        }
        None => {
            println!("Identity          <unknown — not in embedded strict database>");
        }
    }

    let interface_label = match sbr.mfg.interface {
        0x00 => "IT/IR (host bus adapter)",
        0x0c => "IT/IR (legacy)",
        0x10 => "iMR (Integrated MegaRAID)",
        _ => "unknown",
    };
    println!(
        "Interface byte    0x{:02x} ({})",
        sbr.mfg.interface, interface_label
    );

    println!(
        "SAS WWN           {}",
        match sbr.sas_addr {
            Some(wwn) => format!("0x{:016x}", wwn),
            None => "<all zeros — uninitialized>".to_string(),
        }
    );

    println!();
    println!("Checksums:");
    println!(
        "  MFG block       {}",
        if sbr.checksum_valid { "OK" } else { "INVALID" }
    );
    println!(
        "  MFG duplicate   {}",
        if sbr.mfg_duplicate_valid {
            "OK"
        } else {
            "INVALID"
        }
    );
    println!(
        "  WWID            {}",
        if sbr.wwid_checksum_valid {
            "OK"
        } else {
            "INVALID"
        }
    );
}

// ---- verify -----------------------------------------------------------------

fn run_verify(file: &std::path::Path) -> Result<(), crate::Error> {
    let bytes = fs::read(file)?;

    if bytes.len() != 256 {
        eprintln!(
            "FAIL: {} is {} bytes (expected 256)",
            file.display(),
            bytes.len()
        );
        std::process::exit(2);
    }

    let sbr = match parse_sbr(&bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: SBR parse failed: {}", e);
            std::process::exit(2);
        }
    };

    let mut failures = 0;
    if !sbr.checksum_valid {
        eprintln!("FAIL: MFG block checksum invalid");
        failures += 1;
    }
    if !sbr.mfg_duplicate_valid {
        eprintln!("FAIL: MFG duplicate block checksum invalid");
        failures += 1;
    }
    if !sbr.wwid_checksum_valid {
        eprintln!("FAIL: WWID checksum invalid");
        failures += 1;
    }
    if sbr.mfg.pcivid != 0x1000 {
        eprintln!(
            "FAIL: PCI VendorID is 0x{:04x}, expected 0x1000 (LSI)",
            sbr.mfg.pcivid
        );
        failures += 1;
    }
    if sbr.mfg.pcipid != 0x0072 && sbr.mfg.pcipid != 0x0073 {
        eprintln!(
            "FAIL: PCI DeviceID is 0x{:04x}, expected 0x0072 (IT/IR) or 0x0073 (IMR)",
            sbr.mfg.pcipid
        );
        failures += 1;
    }

    if failures == 0 {
        println!(
            "OK: {} passes all structural and checksum checks",
            file.display()
        );
        Ok(())
    } else {
        eprintln!("\n{} check(s) failed", failures);
        std::process::exit(1);
    }
}

// ---- build ------------------------------------------------------------------

fn run_build(
    identity: &str,
    _sas_wwn: Option<&str>,
    _out: Option<&std::path::Path>,
) -> Result<(), crate::Error> {
    let cards = card_database::load_embedded()
        .map_err(|e| crate::Error::Other(format!("card-database load failed: {}", e)))?;

    let Some(card) = cards.iter().find(|c| c.slug == identity) else {
        eprintln!("Unknown identity slug: {}", identity);
        eprintln!("\nKnown identities:");
        for c in &cards {
            eprintln!("  {:30}  {}", c.slug, c.display);
        }
        std::process::exit(2);
    };

    eprintln!(
        "lsi-flash sbr build — not yet fully wired (Stage 2 follow-up).\n\
        \n\
        Found identity: {} ({})\n\
        PCI:            0x{:04x}:0x{:04x}\n\
        Subsystem:      0x{:04x}:0x{:04x}\n\
        Personality:    {:?}\n\
        Interface byte: 0x{:02x}\n\
        \n\
        Synthesis needs an SBR template (resources/sbr/<vendor>.sbr) to seed the\n\
        unk* fields before identity patching. Tracking this as Stage 2 follow-up.",
        card.display,
        card.slug,
        card.vid,
        card.did,
        card.subsys_vid,
        card.subsys_pid,
        card.default_personality,
        card.interface_byte,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn sbr_command_parses() {
        // Smoke: the clap subcommand surface accepts what we ship.
        let _ = crate::cli::Cli::command();
    }

    #[test]
    fn sbr_show_parses() {
        let cli =
            crate::cli::Cli::try_parse_from(["lsi-flash", "sbr", "show", "/tmp/x.sbr"]).unwrap();
        match cli.command {
            crate::cli::Command::Sbr {
                sub: SbrCommand::Show { file },
            } => {
                assert_eq!(file, PathBuf::from("/tmp/x.sbr"));
            }
            _ => panic!("expected Sbr::Show"),
        }
    }

    #[test]
    fn sbr_verify_parses() {
        let cli =
            crate::cli::Cli::try_parse_from(["lsi-flash", "sbr", "verify", "/tmp/x.sbr"]).unwrap();
        assert!(matches!(
            cli.command,
            crate::cli::Command::Sbr {
                sub: SbrCommand::Verify { .. }
            }
        ));
    }

    #[test]
    fn sbr_build_parses_with_identity() {
        let cli =
            crate::cli::Cli::try_parse_from(["lsi-flash", "sbr", "build", "--as", "lsi-9211-8i"])
                .unwrap();
        match cli.command {
            crate::cli::Command::Sbr {
                sub: SbrCommand::Build { identity, .. },
            } => {
                assert_eq!(identity, "lsi-9211-8i");
            }
            _ => panic!("expected Sbr::Build"),
        }
    }
}
