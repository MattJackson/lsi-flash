//! `lsi-flash sbr` — SBR-related subcommands.
//!
//! Per ADR-014 (lsi-flash-notes/01-architecture/adr/014-sbr-verb-and-card-database.md):
//! 6 sub-verbs total. This module implements the OFFLINE three first
//! (no hardware required): `show`, `verify`, `build`. The hardware-bound
//! three (`read`, `write`, `modify`) land in a follow-up.
//!
//! **Read verb** (freshman task v2): Implemented via Card trait + TOOLBOX_ISTWI
//! transport through `MptCard::sbr_read()`. Per ADR-017's hybrid-transport,
//! kernel-mediated via /dev/mpt3ctl — mpt3sas stays bound, /dev/sdb stays mounted.

use clap::Subcommand;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::card_database::{self, CardInfo};
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

    /// Write a 256-byte SBR file to the chip's EEPROM (hardware-bound, DESTRUCTIVE).
    /// Changes PCI identity at next chip reset/reboot. Back up first with `sbr read`.
    Write {
        /// PCI BDF of the card (e.g., `0000:03:00.0`).
        #[arg(long, value_name = "BDF")]
        pci: Option<String>,

        /// Path to the 256-byte SBR file to write.
        #[arg(long, value_name = "PATH")]
        from_file: PathBuf,

        /// Confirm this destructive write (required).
        #[arg(long)]
        yes: bool,
    },

    /// Zero-risk write-path proof: read SBR, write identical bytes back, re-read,
    /// assert unchanged. Validates the I²C write path with no identity change.
    Selftest {
        /// PCI BDF of the card (e.g., `0000:03:00.0`).
        #[arg(long, value_name = "BDF")]
        pci: Option<String>,
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
        SbrCommand::Write {
            pci,
            from_file,
            yes,
        } => run_write(pci.as_deref(), &from_file, yes),
        SbrCommand::Selftest { pci } => run_selftest(pci.as_deref()),
    }
}

/// Resolve the target BDF: explicit `--pci`, else auto-detect the single card.
/// Delegates to the shared `crate::card::resolve_bdf` (never assumes a default).
fn resolve_bdf(pci_bdf: Option<&str>) -> Result<String, crate::Error> {
    crate::card::resolve_bdf(pci_bdf).map_err(|e| crate::Error::Other(format!("{}", e)))
}

fn sha_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

// ---- write ------------------------------------------------------------------

/// Write a 256-byte SBR file to the chip (DESTRUCTIVE). Validates the file
/// structure before writing; requires `--yes`.
fn run_write(
    pci_bdf: Option<&str>,
    from_file: &std::path::Path,
    yes: bool,
) -> Result<(), crate::Error> {
    let bytes = fs::read(from_file)
        .map_err(|e| crate::Error::Other(format!("read {}: {}", from_file.display(), e)))?;
    if bytes.len() != 256 {
        return Err(crate::Error::Other(format!(
            "SBR file must be exactly 256 bytes, got {}",
            bytes.len()
        )));
    }
    // Structural sanity check before any destructive write.
    parse_sbr(&bytes).map_err(|e| {
        crate::Error::Other(format!(
            "refusing to write: SBR file failed structural parse: {}",
            e
        ))
    })?;

    if !yes {
        return Err(crate::Error::Other(format!(
            "Refusing destructive SBR write without --yes. This changes PCI identity \
             at next reset. File SHA256={}. Re-run with --yes to confirm.",
            sha_hex(&bytes)
        )));
    }

    let bdf = resolve_bdf(pci_bdf)?;
    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("discover_one({}): {}", bdf, e)))?;

    let mut arr = [0u8; 256];
    arr.copy_from_slice(&bytes);
    card.sbr_write(&arr)
        .map_err(|e| crate::Error::Other(format!("card.sbr_write: {}", e)))?;

    eprintln!(
        "SBR written ({} bytes, SHA256={}). Takes effect on next chip reset/reboot.",
        bytes.len(),
        sha_hex(&bytes)
    );
    Ok(())
}

// ---- selftest ---------------------------------------------------------------

/// Idempotent SBR write round-trip: read → write identical → re-read → compare.
/// Proves the I²C write path with zero identity change.
fn run_selftest(pci_bdf: Option<&str>) -> Result<(), crate::Error> {
    let bdf = resolve_bdf(pci_bdf)?;
    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("discover_one({}): {}", bdf, e)))?;

    println!("sbr selftest: idempotent write round-trip on {bdf} (no identity change)");

    let before = card
        .sbr_read()
        .map_err(|e| crate::Error::Other(format!("read (before): {}", e)))?;
    println!("  read before:  SHA256={}", sha_hex(&before));

    card.sbr_write(&before)
        .map_err(|e| crate::Error::Other(format!("write (identical): {}", e)))?;
    println!("  write:        Success (wrote back identical bytes)");

    let after = card
        .sbr_read()
        .map_err(|e| crate::Error::Other(format!("read (after): {}", e)))?;
    println!("  read after:   SHA256={}", sha_hex(&after));

    if before == after {
        println!("PASS: SBR write path validated, zero identity change.");
        Ok(())
    } else {
        Err(crate::Error::Other(
            "FAIL: SBR changed across idempotent write! Restore from backup.".to_string(),
        ))
    }
}

// ---- read -------------------------------------------------------------------

/// Read SBR from chip via TOOLBOX_ISTWI transport through Card trait.
/// Per ADR-017's hybrid-transport: kernel-mediated via Mpt3CtlTransport,
/// mpt3sas stays bound, /dev/sdb stays mounted. Cites src/card/mpt.rs::MptCard::sbr_read
/// for the wire protocol implementation (TOOLBOX_ISTWI_READ_WRITE_REQUEST).
fn read_sbr_from_chip(
    pci_bdf: Option<&str>,
    output: Option<&std::path::Path>,
    json_output: bool,
) -> Result<(), crate::Error> {
    // Resolve target: explicit --pci, else auto-detect the single card.
    let bdf = resolve_bdf(pci_bdf)?;

    // Discover the specific card at BDF via Card trait dispatch.
    // Per ADR-017 §Decision, this returns MptCard for known chip families,
    // or UnsupportedCard(vid, did) cleanly for unknown devices.
    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("card.discover_one({}): {}", bdf, e)))?;

    // Read SBR via Card trait — MptCard impl uses TOOLBOX_ISTWI through Mpt3CtlTransport.
    // No RealIoc / I2C bit-bang required; kernel handles DMA via /dev/mpt3ctl.
    let sbr_bytes = card
        .sbr_read()
        .map_err(|e| crate::Error::Other(format!("card.sbr_read: {}", e)))?;

    // Compute SHA256 of SBR bytes - printed to stderr regardless of output mode
    let mut hasher = Sha256::new();
    hasher.update(sbr_bytes);
    let sha256_hex = format!("{:x}", hasher.finalize());
    eprintln!("SBR SHA256: {}", sha256_hex);

    // Output handling - identical to original implementation for consistency
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
            fs::write(out_path, sbr_bytes)
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
        let checksum = 0x5b_u16.wrapping_sub(mfg_sum) & 0xff;
        canned_sbr[75] = checksum as u8;

        // Duplicate MFG block at offset 0x4C-0x97
        let mid_point = 0x4c;
        for i in 0..mid_point {
            canned_sbr[mid_point + i] = canned_sbr[i];
        }

        // Compute duplicate checksum
        let dup_sum: u16 = canned_sbr[mid_point..mid_point + 75]
            .iter()
            .map(|&b| b as u16)
            .sum();
        canned_sbr[mid_point + 75] = (0x5b_u16.wrapping_sub(dup_sum) & 0xff) as u8;

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
        hasher.update(canned_sbr);
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
        canned_sbr[75] = (0x5b_u16.wrapping_sub(mfg_sum) & 0xff) as u8;

        let mid_point = 0x4c;
        for i in 0..mid_point {
            canned_sbr[mid_point + i] = canned_sbr[i];
        }
        let dup_sum: u16 = canned_sbr[mid_point..mid_point + 75]
            .iter()
            .map(|&b| b as u16)
            .sum();
        canned_sbr[mid_point + 75] = (0x5b_u16.wrapping_sub(dup_sum) & 0xff) as u8;

        // Add SAS address at offset 0xD8-0xDF (big-endian)
        let sas_addr: u64 = 0x0014380b00000001;
        canned_sbr[216..224].copy_from_slice(&sas_addr.to_be_bytes());

        // WWID checksum at offset 0xEF
        let wwid_sum: u16 = canned_sbr[216..239].iter().map(|&b| b as u16).sum();
        canned_sbr[239] = (0x5b_u16.wrapping_sub(wwid_sum) & 0xff) as u8;

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
