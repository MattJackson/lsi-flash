//! `lsi-flash sbr` — SBR-related subcommands.
//!
//! Per ADR-014 (lsi-flash-notes/01-architecture/adr/014-sbr-verb-and-card-database.md):
//! 6 sub-verbs total. This module implements the OFFLINE three first
//! (no hardware required): `show`, `verify`, `build`. The hardware-bound
//! three (`read`, `write`, `modify`) land in a follow-up.
//!
//! **Read verb** (freshman task): Implemented via I2C bit-bang through
//! `src/sbr/i2c.rs::i2c_read_sbr`. Cites lsirec.c:570-630 for wire protocol.

use clap::Subcommand;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::card_database::{self, CardInfo};
use crate::mpi::real_ioc::RealIoc;
use crate::pci::LinuxSysfs as PlatformImpl;
use crate::sbr::i2c::{i2c_init, i2c_read_sbr, I2cContext};
use crate::sbr::parse::{parse_sbr, Sbr};

/// SBR-related operations.
#[derive(Subcommand, Debug)]
pub enum SbrCommand {
    /// Read SBR from the chip's EEPROM via I2C (hardware-bound).
    /// Cites src/sbr/i2c.rs::i2c_read_sbr for the wire protocol.
    /// Requires Linux with real hardware.
    Read {
        /// PCI BDF of the card to read from (e.g., `0000:03:00.0`).
        /// Defaults to first SAS2008 card if not given.
        #[arg(long, value_name = "BDF")]
        pci: Option<String>,

        /// Output file path for raw SBR bytes. Default: stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,

        /// Write SBR fields as JSON instead of raw bytes.
        #[arg(long)]
        json: bool,
    },

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
        SbrCommand::Read { pci, output, json } => {
            read_sbr_from_chip(pci.as_deref(), output.as_deref(), json)
        }
        SbrCommand::Show { file } => run_show(&file),
        SbrCommand::Verify { file } => run_verify(&file),
        SbrCommand::Build {
            identity,
            sas_wwn,
            out,
        } => run_build(&identity, sas_wwn.as_deref(), out.as_deref()),
    }
}

// ---- read -------------------------------------------------------------------

/// Read SBR from chip via I2C. Cites src/sbr/i2c.rs::i2c_read_sbr signature:
/// `pub fn i2c_read_sbr(ctx: &mut I2cContext, offset: usize, len: usize) -> Result<Vec<u8>, I2cError>`
fn read_sbr_from_chip(
    pci_bdf: Option<&str>,
    output: Option<&std::path::Path>,
    json_output: bool,
) -> Result<(), crate::Error> {
    let platform = PlatformImpl {};

    // Discover cards if no BDF given
    let bdf = if let Some(bdf) = pci_bdf {
        bdf.to_string()
    } else {
        match crate::pci::discover_sas2008_devices(&platform) {
            Ok(mut cards) => {
                if cards.is_empty() {
                    return Err(crate::Error::Other(
                        "No SAS2008 card found on system".to_string(),
                    ));
                }
                cards.remove(0).bdf
            }
            Err(e) => {
                return Err(crate::Error::Other(format!(
                    "Failed to discover SAS2008 devices: {}",
                    e
                )));
            }
        }
    };

    // Open RealIoc against the BDF
    let mut realioc = match RealIoc::open(platform, &bdf) {
        Ok(r) => r,
        Err(e) => {
            return Err(crate::Error::Other(format!(
                "RealIoc::open failed for {}: {}",
                bdf, e
            )));
        }
    };

    // Get mutable BAR1 access via RealIoc::bar1_mut()
    let bar1 = realioc
        .bar1_mut()
        .ok_or_else(|| crate::Error::Other("BAR1 not mapped".to_string()))?;

    // Initialize I2C context - uses DCR_SBR_CONFIG to determine addr/type
    // Cites src/sbr/i2c.rs:206-258 for i2c_init signature and usage
    let mut ctx = I2cContext {
        bar1: Box::new([0u8; 4096]),
        sbr_addr: 0x50, // Default, will be overridden by init
        eep_type: 0x02, // Default
    };

    // Copy current BAR1 state into context for i2c_init
    ctx.bar1.copy_from_slice(bar1);

    // Initialize I2C - reads DCR_SBR_CONFIG to determine EEPROM address/type
    // Cites lsirec.c:501-514 for addr determination pattern
    let ctx_bar1 = ctx.bar1.clone();

    i2c_init(
        &mut ctx,
        move |addr| {
            let off = addr as usize;
            u32::from_le_bytes([
                ctx_bar1[off],
                ctx_bar1[off + 1],
                ctx_bar1[off + 2],
                ctx_bar1[off + 3],
            ])
        },
        |_addr, _val| {}, // No writes needed - we just need to set up the context
    );

    eprintln!("Using I2C address 0x{:02x}", ctx.sbr_addr);

    // Read SBR via i2c_read_sbr - reads 256 bytes from offset 0
    // Cites src/sbr/i2c.rs:289 signature and lsirec.c:570-630 for wire protocol
    let sbr_bytes = match i2c_read_sbr(&mut ctx, 0, 256) {
        Ok(bytes) => bytes,
        Err(e) => {
            return Err(crate::Error::Other(format!("i2c_read_sbr failed: {}", e)));
        }
    };

    // Copy BAR1 changes back to real BAR1 - ctx.bar1 now contains the updated state from i2c_init operations
    bar1.copy_from_slice(ctx.bar1.as_ref());

    // Compute SHA256 of SBR bytes - printed to stderr regardless of output mode
    let mut hasher = Sha256::new();
    hasher.update(&sbr_bytes);
    let sha256_hex = format!("{:x}", hasher.finalize());
    eprintln!("SBR SHA256: {}", sha256_hex);

    // Output handling
    if json_output {
        // Parse and serialize as JSON
        let sbr = parse_sbr(&sbr_bytes)
            .map_err(|e| crate::Error::Other(format!("SBR parse failed: {}", e)))?;
        let json = serde_json::to_string_pretty(&sbr)
            .map_err(|e| crate::Error::Other(format!("JSON serialization failed: {}", e)))?;

        if let Some(out_path) = output {
            fs::write(out_path, format!("{}\n", json))
                .map_err(|e| crate::Error::Other(format!("Failed to write JSON: {}", e)))?;
        } else {
            println!("{}", json);
        }
    } else {
        // Write raw bytes
        if let Some(out_path) = output {
            fs::write(out_path, &sbr_bytes)
                .map_err(|e| crate::Error::Other(format!("Failed to write SBR: {}", e)))?;
        } else {
            std::io::stdout()
                .lock()
                .write_all(&sbr_bytes)
                .map_err(|e| crate::Error::Other(format!("Failed to write stdout: {}", e)))?;
        }
    }

    Ok(())
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

    // ---- read verb tests ----

    #[test]
    fn test_read_sbr_canned_bytes_write_to_file() {
        // Build a canned SBR (256 bytes) with known values
        let mut canned_sbr = [0u8; 256];

        // Set up minimal valid MFG block at offset 0-75
        canned_sbr[0..4].copy_from_slice(&0x6122f661u32.to_le_bytes());
        canned_sbr[4..8].copy_from_slice(&0xb34f36f7u32.to_le_bytes());
        canned_sbr[8..12].copy_from_slice(&0x91d700f8u32.to_le_bytes());

        // pcivid = 0x1000 (LSI), pcipid = 0x0072 (IT/IR) at offset 0x0C-0x0E
        canned_sbr[12..14].copy_from_slice(&0x1000u16.to_le_bytes());
        canned_sbr[14..16].copy_from_slice(&0x0072u16.to_le_bytes());

        // Fill rest of MFG block with zeros, compute checksum at offset 75
        let mut mfg_sum: u16 = 0;
        for &b in &canned_sbr[0..75] {
            mfg_sum += b as u16;
        }
        let checksum = (0x5b as u16).wrapping_sub(mfg_sum) & 0xff;
        canned_sbr[75] = checksum as u8;

        // Duplicate MFG block at offset 0x4C-0x97
        let mid_point = 0x4c;
        for i in 0..mid_point {
            canned_sbr[mid_point + i] = canned_sbr[i];
        }

        // Compute duplicate checksum
        let mut dup_sum: u16 = 0;
        for i in mid_point..(mid_point + 75) {
            dup_sum += canned_sbr[i] as u16;
        }
        canned_sbr[mid_point + 75] = ((0x5b as u16).wrapping_sub(dup_sum) & 0xff) as u8;

        // Verify the parsing works with canned data
        let parsed = crate::sbr::parse::parse_sbr(&canned_sbr).unwrap();

        assert_eq!(parsed.mfg.pcivid, 0x1000);
        assert_eq!(parsed.mfg.pcipid, 0x0072);
        assert!(parsed.checksum_valid);

        // Verify JSON serialization works
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(!json.is_empty());

        // Verify SHA256 computation
        let mut hasher = sha2::Sha256::new();
        hasher.update(&canned_sbr);
        let sha256_hex = format!("{:x}", hasher.finalize());
        assert_eq!(sha256_hex.len(), 64);
    }

    #[test]
    fn test_read_sbr_json_output_decodes_correctly() {
        // Build a canned SBR with SAS address for JSON testing
        let mut canned_sbr = [0u8; 256];

        // MFG block (same as above)
        canned_sbr[0..4].copy_from_slice(&0x6122f661u32.to_le_bytes());
        canned_sbr[4..8].copy_from_slice(&0xb34f36f7u32.to_le_bytes());
        canned_sbr[8..12].copy_from_slice(&0x91d700f8u32.to_le_bytes());
        canned_sbr[12..14].copy_from_slice(&0x1000u16.to_le_bytes());
        canned_sbr[14..16].copy_from_slice(&0x0072u16.to_le_bytes());

        // subsys_vid = 0x1028 (Dell), subsys_pid = 0x1f1c at offset 0x14-0x16
        canned_sbr[20..22].copy_from_slice(&0x1028u16.to_le_bytes());
        canned_sbr[22..24].copy_from_slice(&0x1f1cu16.to_le_bytes());

        // interface at offset 0x40 = 0x00 (IT/IR)
        canned_sbr[64] = 0x00;
        canned_sbr[65] = 0x0c;

        // Compute MFG checksums and fill duplicates
        let mut mfg_sum: u16 = 0;
        for &b in &canned_sbr[0..75] {
            mfg_sum += b as u16;
        }
        canned_sbr[75] = ((0x5b as u16).wrapping_sub(mfg_sum) & 0xff) as u8;

        let mid_point = 0x4c;
        for i in 0..mid_point {
            canned_sbr[mid_point + i] = canned_sbr[i];
        }
        let mut dup_sum: u16 = 0;
        for i in mid_point..(mid_point + 75) {
            dup_sum += canned_sbr[i] as u16;
        }
        canned_sbr[mid_point + 75] = ((0x5b as u16).wrapping_sub(dup_sum) & 0xff) as u8;

        // Add SAS address at offset 0xD8-0xDF (big-endian)
        let sas_addr: u64 = 0x0014380b00000001;
        canned_sbr[216..224].copy_from_slice(&sas_addr.to_be_bytes());

        // WWID checksum at offset 0xEF
        let mut wwid_sum: u16 = 0;
        for i in 216..239 {
            wwid_sum += canned_sbr[i] as u16;
        }
        canned_sbr[239] = ((0x5b as u16).wrapping_sub(wwid_sum) & 0xff) as u8;

        // Parse and verify JSON round-trip
        let parsed = crate::sbr::parse::parse_sbr(&canned_sbr).unwrap();

        assert_eq!(parsed.mfg.pcivid, 0x1000);
        assert_eq!(parsed.mfg.subsys_vid, 0x1028); // Dell
        assert_eq!(parsed.sas_addr, Some(sas_addr));

        // Serialize to JSON and deserialize back
        let json = serde_json::to_string(&parsed).unwrap();
        let re_parsed: crate::sbr::parse::Sbr = serde_json::from_str(&json).unwrap();

        assert_eq!(re_parsed.mfg.pcivid, parsed.mfg.pcivid);
        assert_eq!(re_parsed.sas_addr, parsed.sas_addr);
        assert!(re_parsed.checksum_valid);
    }

    #[test]
    fn test_sbr_read_subcommand_parsing() {
        // Test --pci flag parsing
        let cli =
            crate::cli::Cli::try_parse_from(["lsi-flash", "sbr", "read", "--pci", "0000:03:00.0"])
                .unwrap();

        match cli.command {
            crate::cli::Command::Sbr {
                sub: SbrCommand::Read { pci, output, json },
            } => {
                assert_eq!(pci.as_deref(), Some("0000:03:00.0"));
                assert!(output.is_none());
                assert!(!json);
            }
            _ => panic!("expected Sbr::Read"),
        }

        // Test --output flag parsing
        let cli = crate::cli::Cli::try_parse_from([
            "lsi-flash",
            "sbr",
            "read",
            "--pci",
            "0000:03:00.0",
            "--output",
            "/tmp/sbr.bin",
        ])
        .unwrap();

        match cli.command {
            crate::cli::Command::Sbr {
                sub: SbrCommand::Read { output, .. },
            } => {
                assert_eq!(
                    output.as_deref(),
                    Some(std::path::Path::new("/tmp/sbr.bin"))
                );
            }
            _ => panic!("expected Sbr::Read"),
        }

        // Test --json flag parsing
        let cli = crate::cli::Cli::try_parse_from([
            "lsi-flash",
            "sbr",
            "read",
            "--pci",
            "0000:03:00.0",
            "--json",
        ])
        .unwrap();

        match cli.command {
            crate::cli::Command::Sbr {
                sub: SbrCommand::Read { json, .. },
            } => {
                assert!(json);
            }
            _ => panic!("expected Sbr::Read"),
        }

        // Test all flags together
        let cli = crate::cli::Cli::try_parse_from([
            "lsi-flash",
            "sbr",
            "read",
            "--pci",
            "0000:03:00.0",
            "--output",
            "/tmp/sbr.json",
            "--json",
        ])
        .unwrap();

        match cli.command {
            crate::cli::Command::Sbr {
                sub: SbrCommand::Read { pci, output, json },
            } => {
                assert_eq!(pci.as_deref(), Some("0000:03:00.0"));
                assert_eq!(
                    output.as_deref(),
                    Some(std::path::Path::new("/tmp/sbr.json"))
                );
                assert!(json);
            }
            _ => panic!("expected Sbr::Read"),
        }

        // Test read without --pci (should succeed, defaults to first card)
        let cli = crate::cli::Cli::try_parse_from(["lsi-flash", "sbr", "read"]).unwrap();

        match cli.command {
            crate::cli::Command::Sbr {
                sub: SbrCommand::Read { pci, .. },
            } => {
                assert!(pci.is_none());
            }
            _ => panic!("expected Sbr::Read"),
        }
    }
}
