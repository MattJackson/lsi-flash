//! CLI surface for `lsi-flash`. Per ADR-007 + ADR-014: 5 top-level verbs
//! (detect/backup/flash/recover/sbr) + config pages.
//!
//! See `lsi-flash-notes/01-architecture/adr/007-cli-surface.md` and
//! `lsi-flash-notes/01-architecture/adr/014-sbr-verb-and-card-database.md`.

pub mod backup;
pub mod config;
pub mod detect;
pub mod erase;
pub mod flash;
pub mod fw;
pub mod recover;
pub mod region;
pub mod safety;
pub mod sbr;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Cross-flash LSI SAS2008-based HBAs.
#[derive(Parser, Debug)]
#[command(name = "lsi-flash", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Emit machine-readable JSON output instead of human prose.
    #[arg(long, global = true)]
    pub json: bool,

    /// Which card to operate on (e.g. `0000:03:00.0`). REQUIRED for all
    /// card-targeting verbs — run `lsi-flash detect` to list cards, then pass the
    /// BDF here. (`detect` itself needs no --pci.) The impossible sentinel
    /// `-1:-1:-1:-1` explicitly selects the synthetic mock backend.
    /// `allow_hyphen_values` lets the sentinel be passed as `--pci -1:-1:-1:-1`.
    #[arg(long, global = true, value_name = "BDF", allow_hyphen_values = true)]
    pub pci: Option<String>,
}

/// Five verbs + config. That's it.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Identify the card(s): PCI ids, current firmware, current SBR identity.
    Detect {
        /// Print only BDFs, one per line (machine-readable; for piping into
        /// `--pci -`, e.g. `lsi-flash detect --bdf | head -1 | lsi-flash backup --pci -`).
        #[arg(long = "bdf")]
        bdf_only: bool,
    },

    /// Snapshot SBR + flash + identity to disk. Non-destructive.
    Backup {
        /// Output directory for backup artifacts.
        /// Default: `/var/lib/lsi-flash/backups/<sas-addr>/<timestamp>/`.
        #[arg(long, value_name = "DIR")]
        out: Option<String>,
    },

    /// LEGACY (hidden) — superseded by `fw write` + composition (ADR-007 v2).
    /// Kept compiled only until the smart `fw write` orchestration lands; not in
    /// the user surface. Do not document.
    #[command(hide = true)]
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

    /// Restore all 4 artifacts (fw + bios + sbr; nvdata OPEN) from a backup dir,
    /// bit-for-bit + sha-verified. The inverse of `backup`.
    Restore {
        /// Directory containing the backup artifacts to restore from.
        #[arg(long, value_name = "DIR")]
        backup_dir: String,

        /// Skip interactive confirmations.
        #[arg(long)]
        yes: bool,

        /// Show what would happen; write nothing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Firmware image region (read / write). Write = the firmware installer.
    Fw {
        #[command(subcommand)]
        sub: FwCommand,
    },

    /// BIOS option-ROM region (read / write).
    Bios {
        #[command(subcommand)]
        sub: region::RegionCommand,
    },

    /// NVDATA region (read / write).
    Nvdata {
        #[command(subcommand)]
        sub: region::RegionCommand,
    },

    /// SBR (Subsystem Boot Record) operations. Per ADR-014.
    Sbr {
        #[command(subcommand)]
        sub: sbr::SbrCommand,
    },

    /// Config page operations — read individual pages or dump all existing pages.
    /// Cites: mpi2_cnfg.h for page types/actions, mpt3sas_config.c for 2-step pattern.
    Config {
        #[command(subcommand)]
        sub: config::ConfigSubCommand,
    },

    /// DIAGNOSTIC: send a raw TOOLBOX_CLEAN and print the firmware's exact
    /// IOCStatus + IOCLogInfo. DESTRUCTIVE if the firmware honors it. --yes required.
    /// Low-level RE / diagnostic commands. Hidden from normal help — these are
    /// reverse-engineering instruments, not part of the normal flashing workflow.
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        sub: DebugCommand,
    },

    /// LEGACY (hidden) — offline firmware synthesis moved under `fw` (e.g.
    /// `fw reverse-phy`). Kept compiled until fully migrated.
    #[command(hide = true)]
    Firmware {
        #[command(subcommand)]
        sub: FirmwareCommand,
    },
}

/// Args for `fw read` — flash copy by default, running image with `--running`.
#[derive(Args, Debug)]
pub struct FwReadArgs {
    /// Output file for the raw region bytes. Default: stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<std::path::PathBuf>,
    /// Read the RUNNING image (FW_CURRENT 0x00) instead of the flash copy
    /// (FW_FLASH 0x01). They differ when a write is in flash but not yet booted.
    #[arg(long)]
    pub running: bool,
}

/// `fw` — firmware image region + offline manipulation.
#[derive(Subcommand, Debug)]
pub enum FwCommand {
    /// Read the firmware region (FW_UPLOAD). Default = the flash copy
    /// (FW_FLASH 0x01); `--running` = the running image (FW_CURRENT 0x00).
    Read(FwReadArgs),
    /// Write the firmware region to the chip (FW_DOWNLOAD). DESTRUCTIVE — --yes.
    Write(region::RegionWriteArgs),
    /// Offline: synthesize a PHY-reversed firmware blob (Port 0↔7 / 1↔6 / 2↔5 / 3↔4).
    ReversePhy {
        #[arg(long = "in", value_name = "PATH")]
        input: std::path::PathBuf,
        #[arg(long = "out", value_name = "PATH")]
        output: std::path::PathBuf,
    },
}

/// Diagnostic / reverse-engineering commands (hidden). NOT part of the normal
/// flashing workflow — erase is internal to `fw write`; this raw erase exists
/// only to characterize firmware behavior (e.g. the Dell erase-lock IOCStatus).
#[derive(Subcommand, Debug)]
pub enum DebugCommand {
    /// Send a raw MPI TOOLBOX_CLEAN and print the firmware's exact IOCStatus +
    /// IOCLogInfo. DESTRUCTIVE if the firmware honors it. --yes required.
    Erase {
        /// Confirm this destructive flash-erase command.
        #[arg(long)]
        yes: bool,

        /// Also wipe Manufacturing pages (PERSIST_MANUFACT_PAGES). Default: off.
        #[arg(long)]
        wipe_mfg_pages: bool,
    },

    /// Read CHIP-INTERNAL memory via the DIAG RW window (IOC-free; works on a
    /// faulted chip). For RE/diagnostics — hunt for firmware/NVDATA/MRAM in chip
    /// address space (e.g. 0xC2100000 = I²C peripheral). Read-only.
    ChipRead {
        /// Chip-internal address (hex, e.g. 0xC2100000).
        #[arg(long, value_name = "0xNNNNNNNN")]
        addr: String,
        /// Number of bytes to read.
        #[arg(long, default_value = "256")]
        len: usize,
        /// Output file for raw bytes. Default: stdout.
        #[arg(long, value_name = "PATH")]
        out: Option<std::path::PathBuf>,
    },

    /// SCAN the chip-internal bus: one 32-bit word at `addr + i*stride`, `count`
    /// times, printing each line flushed. Maps the peripheral grid (e.g. around
    /// the I²C block 0xC2100000) and pinpoints any address that hangs the bus
    /// (the last printed line = the offender). IOC-free; works on a faulted chip.
    ChipScan {
        /// Base chip-internal address (hex, e.g. 0xC2000000).
        #[arg(long, value_name = "0xNNNNNNNN")]
        addr: String,
        /// Stride between reads in bytes (hex or dec, e.g. 0x10000).
        #[arg(long, default_value = "0x10000")]
        stride: String,
        /// Number of words to read.
        #[arg(long, default_value = "256")]
        count: usize,
    },

    /// WRITE chip-internal memory via the DIAG RW window (IOC-free). Writes a
    /// single 32-bit word (--value) or raw bytes from --in, at --addr. Works on
    /// RAM/registers; NOR flash will NOT program from a store (needs the flash
    /// controller). Use to empirically test what an address accepts. DESTRUCTIVE.
    ChipWrite {
        /// Chip-internal address (hex, e.g. 0xFC200000).
        #[arg(long, value_name = "0xNNNNNNNN")]
        addr: String,
        /// Single 32-bit value to write (hex, e.g. 0x12345678). Mutually exclusive with --in.
        #[arg(long, value_name = "0xNNNNNNNN")]
        value: Option<String>,
        /// Raw byte file to write starting at --addr. Mutually exclusive with --value.
        #[arg(long, value_name = "PATH")]
        r#in: Option<std::path::PathBuf>,
        /// Confirm this destructive chip-memory write.
        #[arg(long)]
        yes: bool,
    },

    /// SCAN the PowerPC DCR register bus (separate from chip memory). Reads one
    /// word at `addr + i` for `count` iterations. lsirec only touches 0x307 and
    /// 0x340; the rest is unexplored boot/config state. IOC-free.
    DcrScan {
        /// Base DCR offset (hex, e.g. 0x300).
        #[arg(long, default_value = "0x300")]
        addr: String,
        /// Number of DCR words to read.
        #[arg(long, default_value = "256")]
        count: usize,
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

/// Manipulator-grade firmware synthesis subcommands. Cites
/// `lsi-flash-notes/03-firmware-formats/mpt-firmware-format.md` §N (PHY-to-slot map).
#[derive(Subcommand, Debug)]
pub enum FirmwareCommand {
    /// Synthesize a PHY-reversed firmware blob (Port 0↔7 / 1↔6 / 2↔5 / 3↔4).
    ///
    /// Closes the gap left by the absent `phase15_reversed.zip` from
    /// Broadcom's public archive — any standard MPT firmware can be
    /// permuted into a PHY-reversed variant.
    ReversePhy {
        /// Input firmware path (e.g. `2118it.bin` or `H200A.FW`).
        #[arg(long = "in", value_name = "PATH")]
        input: String,
        /// Output path for the synthesized firmware.
        #[arg(long = "out", value_name = "PATH")]
        output: String,
    },
}

/// Entry point invoked from `main.rs`. All subcommands stub out for now —
/// real implementations land in Stages 2/3.
pub fn run(cli: Cli) -> Result<(), crate::Error> {
    match cli.command {
        Command::Detect { bdf_only } => detect::run(cli.json, bdf_only),
        Command::Backup { out } => backup::run(out, cli.json, cli.pci.clone()),
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
        } => flash::run(
            mode,
            identity,
            dry_run,
            yes,
            keep_sas_address,
            firmware,
            no_bios,
            backup_dir,
            wipe_mfg_pages,
            cli.json,
        ),
        Command::Restore {
            backup_dir,
            yes,
            dry_run,
        } => recover::run(backup_dir, yes, cli.json, cli.pci.clone(), dry_run),
        Command::Fw { sub } => match sub {
            FwCommand::Read(a) => {
                let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                    .map_err(|e| crate::Error::Other(format!("{}", e)))?;
                fw::run_read(bdf, a.running, a.out.as_deref())
            }
            FwCommand::Write(a) => {
                let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                    .map_err(|e| crate::Error::Other(format!("{}", e)))?;
                // fw write goes through the validated, wizard-style path (5-point
                // gate); bios/nvdata stay on the raw region writer.
                fw::run_write(bdf, &a.from_file, a.yes)
            }
            FwCommand::ReversePhy { input, output } => {
                let data = std::fs::read(&input)?;
                let out = crate::firmware::synthesize::synthesize_reverse_phy(&data)?;
                std::fs::write(&output, &out)?;
                eprintln!("synthesized {} bytes → {}", out.len(), output.display());
                Ok(())
            }
        },
        Command::Bios { sub } => {
            let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                .map_err(|e| crate::Error::Other(format!("{}", e)))?;
            region::run(bdf, crate::mpi::messages::ImageType::Bios, "bios", sub)
        }
        Command::Nvdata { sub } => {
            let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                .map_err(|e| crate::Error::Other(format!("{}", e)))?;
            region::run(
                bdf,
                crate::mpi::messages::ImageType::FlashLayout,
                "nvdata",
                sub,
            )
        }
        Command::Sbr { sub } => sbr::run(sub),
        Command::Debug { sub } => match sub {
            DebugCommand::Erase {
                yes,
                wipe_mfg_pages,
            } => erase::run(cli.pci.clone(), yes, wipe_mfg_pages, cli.json),
            DebugCommand::ChipRead { addr, len, out } => {
                let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                    .map_err(|e| crate::Error::Other(format!("{}", e)))?;
                let chip_addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16)
                    .map_err(|e| crate::Error::Other(format!("bad --addr {}: {}", addr, e)))?;
                let mut t = crate::sbr::transport::Bar1MmapSbrTransport::open(&bdf)
                    .map_err(|e| crate::Error::Other(format!("bar1 open: {}", e)))?;
                let bytes = t
                    .read_chip_mem(chip_addr, len)
                    .map_err(|e| crate::Error::Other(format!("chip read: {}", e)))?;
                eprintln!(
                    "chipread @0x{:08x}: {} bytes, sha256={}",
                    chip_addr,
                    bytes.len(),
                    {
                        use sha2::{Digest, Sha256};
                        let mut h = Sha256::new();
                        h.update(&bytes);
                        format!("{:x}", h.finalize())
                    }
                );
                match out {
                    Some(p) => std::fs::write(&p, &bytes)?,
                    None => {
                        use std::io::Write;
                        std::io::stdout().lock().write_all(&bytes)?;
                    }
                }
                Ok(())
            }
            DebugCommand::ChipScan {
                addr,
                stride,
                count,
            } => {
                let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                    .map_err(|e| crate::Error::Other(format!("{}", e)))?;
                let parse = |s: &str| {
                    let t = s.trim_start_matches("0x");
                    u32::from_str_radix(t, 16).or_else(|_| s.parse::<u32>())
                };
                let base =
                    parse(&addr).map_err(|e| crate::Error::Other(format!("bad --addr: {}", e)))?;
                let step = parse(&stride)
                    .map_err(|e| crate::Error::Other(format!("bad --stride: {}", e)))?;
                let mut t = crate::sbr::transport::Bar1MmapSbrTransport::open(&bdf)
                    .map_err(|e| crate::Error::Other(format!("bar1 open: {}", e)))?;
                eprintln!(
                    "chip-scan base=0x{:08x} stride=0x{:x} count={} (last printed addr = any hang)",
                    base, step, count
                );
                use std::io::Write;
                t.scan_chip_mem(base, step, count, |a, v| {
                    println!("0x{:08x} : 0x{:08x}", a, v);
                    let _ = std::io::stdout().flush();
                })
                .map_err(|e| crate::Error::Other(format!("chip scan: {}", e)))?;
                Ok(())
            }
            DebugCommand::ChipWrite {
                addr,
                value,
                r#in,
                yes,
            } => {
                if !yes {
                    return Err(crate::Error::Other(
                        "refusing chip write without --yes (DESTRUCTIVE)".into(),
                    ));
                }
                let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                    .map_err(|e| crate::Error::Other(format!("{}", e)))?;
                let chip_addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16)
                    .map_err(|e| crate::Error::Other(format!("bad --addr {}: {}", addr, e)))?;
                let bytes: Vec<u8> = match (value, r#in) {
                    (Some(v), None) => {
                        let w = u32::from_str_radix(v.trim_start_matches("0x"), 16)
                            .map_err(|e| crate::Error::Other(format!("bad --value {}: {}", v, e)))?;
                        w.to_le_bytes().to_vec()
                    }
                    (None, Some(p)) => std::fs::read(&p)?,
                    _ => {
                        return Err(crate::Error::Other(
                            "specify exactly one of --value or --in".into(),
                        ))
                    }
                };
                let mut t = crate::sbr::transport::Bar1MmapSbrTransport::open(&bdf)
                    .map_err(|e| crate::Error::Other(format!("bar1 open: {}", e)))?;
                t.write_chip_mem(chip_addr, &bytes)
                    .map_err(|e| crate::Error::Other(format!("chip write: {}", e)))?;
                eprintln!("chip-write @0x{:08x}: {} bytes written", chip_addr, bytes.len());
                Ok(())
            }
            DebugCommand::DcrScan { addr, count } => {
                let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                    .map_err(|e| crate::Error::Other(format!("{}", e)))?;
                let base = u32::from_str_radix(addr.trim_start_matches("0x"), 16)
                    .or_else(|_| addr.parse::<u32>())
                    .map_err(|e| crate::Error::Other(format!("bad --addr: {}", e)))?;
                let mut t = crate::sbr::transport::Bar1MmapSbrTransport::open(&bdf)
                    .map_err(|e| crate::Error::Other(format!("bar1 open: {}", e)))?;
                eprintln!("dcr-scan base=0x{:x} count={}", base, count);
                use std::io::Write;
                t.scan_dcr(base, count, |off, v| {
                    println!("DCR 0x{:04x} : 0x{:08x}", off, v);
                    let _ = std::io::stdout().flush();
                })
                .map_err(|e| crate::Error::Other(format!("dcr scan: {}", e)))?;
                Ok(())
            }
        },
        Command::Config { sub } => {
            let bdf = crate::card::resolve_bdf(cli.pci.as_deref())
                .map_err(|e| crate::Error::Other(format!("{}", e)))?;
            config::run(bdf, sub)
        }
        Command::Firmware { sub } => match sub {
            FirmwareCommand::ReversePhy { input, output } => {
                let data = std::fs::read(&input)?;
                let out = crate::firmware::synthesize::synthesize_reverse_phy(&data)?;
                std::fs::write(&output, &out)?;
                eprintln!(
                    "synthesized {} bytes → {} (PhyData permuted, file checksum recomputed)",
                    out.len(),
                    output
                );
                Ok(())
            }
        },
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
        assert!(matches!(cli.command, Command::Detect { .. }));
    }

    #[test]
    fn flash_hba_parses() {
        let cli = Cli::try_parse_from(["lsi-flash", "flash", "HBA"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Flash {
                mode: Mode::HBA,
                ..
            }
        ));
    }

    #[test]
    fn flash_it_alias_parses() {
        let cli = Cli::try_parse_from(["lsi-flash", "flash", "IT"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Flash {
                mode: Mode::HBA,
                ..
            }
        ));
    }

    #[test]
    fn flash_raid_with_identity_parses() {
        let cli = Cli::try_parse_from(["lsi-flash", "flash", "RAID", "--as", "dell-h200"]).unwrap();
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
    fn restore_parses_with_backup_dir() {
        let cli = Cli::try_parse_from([
            "lsi-flash",
            "restore",
            "--backup-dir",
            "/var/lib/lsi-flash/backups/foo",
        ])
        .unwrap();
        match cli.command {
            Command::Restore { backup_dir, .. } => {
                assert_eq!(backup_dir, "/var/lib/lsi-flash/backups/foo");
            }
            _ => panic!("expected Restore"),
        }
    }

    #[test]
    fn fw_bios_nvdata_namespaces_parse() {
        use super::*;
        assert!(matches!(
            Cli::try_parse_from(["lsi-flash", "fw", "read"])
                .unwrap()
                .command,
            Command::Fw {
                sub: FwCommand::Read(_)
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["lsi-flash", "fw", "write", "--from-file", "x"])
                .unwrap()
                .command,
            Command::Fw {
                sub: FwCommand::Write(_)
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["lsi-flash", "bios", "read"])
                .unwrap()
                .command,
            Command::Bios { .. }
        ));
        assert!(matches!(
            Cli::try_parse_from(["lsi-flash", "nvdata", "write", "--from-file", "x"])
                .unwrap()
                .command,
            Command::Nvdata { .. }
        ));
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

    #[test]
    fn firmware_reverse_phy_parses() {
        let cli = Cli::try_parse_from([
            "lsi-flash",
            "firmware",
            "reverse-phy",
            "--in",
            "/tmp/a",
            "--out",
            "/tmp/b",
        ])
        .unwrap();
        match cli.command {
            Command::Firmware {
                sub: FirmwareCommand::ReversePhy { input, output },
            } => {
                assert_eq!(input, "/tmp/a");
                assert_eq!(output, "/tmp/b");
            }
            _ => panic!("expected Firmware::ReversePhy"),
        }
    }

    #[test]
    fn config_read_parses() {
        let cli = Cli::try_parse_from([
            "lsi-flash",
            "--pci",
            "0000:03:00.0",
            "config",
            "read",
            "manufacturing",
            "0",
        ])
        .unwrap();
        match cli.command {
            Command::Config { sub } => {
                matches!(sub, config::ConfigSubCommand::Read { .. });
            }
            _ => panic!("expected Config"),
        }
    }

    #[test]
    fn config_read_all_parses() {
        // No group → read every group (the old `config dump`).
        let cli =
            Cli::try_parse_from(["lsi-flash", "--pci", "0000:03:00.0", "config", "read"]).unwrap();
        match cli.command {
            Command::Config { sub } => {
                matches!(sub, config::ConfigSubCommand::Read { .. });
            }
            _ => panic!("expected Config"),
        }
    }

    #[test]
    fn config_read_requires_pci() {
        let res = Cli::try_parse_from(["lsi-flash", "config", "read", "manufacturing", "0"]);
        assert!(res.is_ok()); // --pci is global, not required for parsing
    }
}
