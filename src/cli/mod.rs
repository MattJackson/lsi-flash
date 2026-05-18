//! CLI surface for `lsi-flash`. Per ADR-007 + ADR-014: 5 top-level verbs
//! (detect/backup/flash/recover/sbr).
//!
//! See `lsi-flash-notes/01-architecture/adr/007-cli-surface.md` and
//! `lsi-flash-notes/01-architecture/adr/014-sbr-verb-and-card-database.md`.

pub mod sbr;

use clap::{Parser, Subcommand, ValueEnum};

/// Cross-flash LSI SAS2008-based HBAs.
#[derive(Parser, Debug)]
#[command(name = "lsi-flash", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Emit machine-readable JSON output instead of human prose.
    #[arg(long, global = true)]
    pub json: bool,

    /// Disambiguate which card to operate on (e.g. `0000:03:00.0`).
    /// Required only when multiple SAS2008 cards are present.
    #[arg(long, global = true, value_name = "BDF")]
    pub pci: Option<String>,
}

/// Four verbs. That's it.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Identify the card: PCI ids, current firmware, current SBR identity.
    Detect,

    /// Snapshot SBR + flash + identity to disk. Non-destructive.
    Backup {
        /// Output directory for backup artifacts.
        /// Default: `/var/lib/lsi-flash/backups/<sas-addr>/<timestamp>/`.
        #[arg(long, value_name = "DIR")]
        out: Option<String>,
    },

    /// Run the cross-flash workflow.
    ///
    /// `flash` always takes an internal backup first; refuses to proceed if
    /// backup didn't succeed. Per ADR-007 D-5: HCB auto-runs when needed.
    Flash {
        /// Target firmware mode.
        mode: Mode,

        /// OEM identity to write into the SBR. Defaults to preserving the
        /// card's current identity (D-4): if it was Dell, stays Dell.
        #[arg(long = "as", value_name = "IDENTITY")]
        identity: Option<String>,

        /// Show what would happen; write nothing.
        #[arg(long)]
        dry_run: bool,

        /// Skip interactive confirmations (for unattended automation).
        #[arg(long)]
        yes: bool,

        /// Preserve the existing SAS WWN (D-6 default: true).
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        keep_sas_address: bool,

        /// Override the manifest's auto-picked firmware ID
        /// (e.g. `sas2008-it-19-00-00-00` for P19 instead of default P20.00.07).
        #[arg(long, value_name = "ID")]
        firmware: Option<String>,

        /// Skip flashing the BIOS option-ROM (faster POST; no boot-from-HBA).
        #[arg(long)]
        no_bios: bool,

        /// Override the default backup directory.
        #[arg(long, value_name = "DIR")]
        backup_dir: Option<String>,

        /// Advanced: include `PERSIST_MANUFACT_PAGES` in the TOOLBOX_CLEAN
        /// flag set. Wipes Manufacturing pages too. Required for true factory
        /// blank state (e.g. sanitization before disposal). Forces the
        /// workflow to also re-init Mfg pages from card-DB after writing
        /// firmware. **You almost certainly do not want this.**
        #[arg(long)]
        wipe_mfg_pages: bool,
    },

    /// Restore from a backup directory. Bit-for-bit fidelity to whatever was
    /// captured. Only "go back" path; the chip does not remember its origin
    /// once cross-flashed.
    Recover {
        /// Directory containing the backup artifacts to restore from.
        #[arg(long, value_name = "DIR")]
        backup_dir: String,

        /// Skip interactive confirmations.
        #[arg(long)]
        yes: bool,
    },

    /// SBR (Subsystem Boot Record) operations. Per ADR-014.
    Sbr {
        #[command(subcommand)]
        sub: sbr::SbrCommand,
    },
}

/// Firmware-mode targets for `flash`. Per ADR-007:
/// `HBA` = LSI IT firmware (passthrough). `RAID` = LSI IR firmware
/// (RAID 0/1/1E/10 in firmware). Aliases `IT`/`IR` accepted for users
/// who know the LSI internal names.
#[derive(ValueEnum, Clone, Copy, Debug)]
#[clap(rename_all = "UPPER")]
pub enum Mode {
    /// HBA mode (LSI IT firmware — pure passthrough). Alias: IT.
    #[value(alias = "IT", alias = "it", alias = "hba")]
    HBA,
    /// RAID mode (LSI IR firmware — RAID 0/1/1E/10 in firmware). Alias: IR.
    #[value(alias = "IR", alias = "ir", alias = "raid")]
    RAID,
}

/// Entry point invoked from `main.rs`. All subcommands stub out for now —
/// real implementations land in Stages 2/3.
pub fn run(cli: Cli) -> Result<(), crate::Error> {
    match cli.command {
        Command::Detect => {
            eprintln!("lsi-flash detect — not yet implemented (Stage 2)");
            Ok(())
        }
        Command::Backup { out } => {
            eprintln!(
                "lsi-flash backup --out {:?} — not yet implemented (Stage 2)",
                out
            );
            Ok(())
        }
        Command::Flash {
            mode,
            identity,
            dry_run,
            yes,
            keep_sas_address,
            firmware,
            no_bios,
            backup_dir,
            wipe_mfg_pages,
        } => {
            eprintln!(
                "lsi-flash flash {:?} --as {:?} --dry-run={} --yes={} \
                 --keep-sas-address={} --firmware {:?} --no-bios={} \
                 --backup-dir {:?} --wipe-mfg-pages={} — not yet implemented (Stage 3)",
                mode,
                identity,
                dry_run,
                yes,
                keep_sas_address,
                firmware,
                no_bios,
                backup_dir,
                wipe_mfg_pages
            );
            Ok(())
        }
        Command::Recover { backup_dir, yes } => {
            eprintln!(
                "lsi-flash recover --backup-dir {} --yes={} — not yet implemented (Stage 3)",
                backup_dir, yes
            );
            Ok(())
        }
        Command::Sbr { sub } => sbr::run(sub),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_help() {
        Cli::command().debug_assert();
    }

    #[test]
    fn detect_parses() {
        let cli = Cli::try_parse_from(["lsi-flash", "detect"]).unwrap();
        assert!(matches!(cli.command, Command::Detect));
    }

    #[test]
    fn flash_hba_parses() {
        let cli = Cli::try_parse_from(["lsi-flash", "flash", "HBA"]).unwrap();
        assert!(matches!(cli.command, Command::Flash { mode: Mode::HBA, .. }));
    }

    #[test]
    fn flash_it_alias_parses() {
        let cli = Cli::try_parse_from(["lsi-flash", "flash", "IT"]).unwrap();
        assert!(matches!(cli.command, Command::Flash { mode: Mode::HBA, .. }));
    }

    #[test]
    fn flash_raid_with_identity_parses() {
        let cli = Cli::try_parse_from([
            "lsi-flash", "flash", "RAID", "--as", "dell-h200",
        ])
        .unwrap();
        match cli.command {
            Command::Flash { mode, identity, .. } => {
                assert!(matches!(mode, Mode::RAID));
                assert_eq!(identity.as_deref(), Some("dell-h200"));
            }
            _ => panic!("expected Flash"),
        }
    }

    #[test]
    fn recover_requires_backup_dir() {
        let res = Cli::try_parse_from(["lsi-flash", "recover"]);
        assert!(res.is_err());
    }

    #[test]
    fn recover_parses_with_backup_dir() {
        let cli = Cli::try_parse_from([
            "lsi-flash",
            "recover",
            "--backup-dir",
            "/var/lib/lsi-flash/backups/foo",
        ])
        .unwrap();
        match cli.command {
            Command::Recover { backup_dir, .. } => {
                assert_eq!(backup_dir, "/var/lib/lsi-flash/backups/foo");
            }
            _ => panic!("expected Recover"),
        }
    }

    #[test]
    fn backup_optional_out() {
        let cli = Cli::try_parse_from(["lsi-flash", "backup"]).unwrap();
        match cli.command {
            Command::Backup { out } => assert!(out.is_none()),
            _ => panic!("expected Backup"),
        }
    }

    #[test]
    fn json_flag_global() {
        let cli = Cli::try_parse_from(["lsi-flash", "--json", "detect"]).unwrap();
        assert!(cli.json);
    }
}
