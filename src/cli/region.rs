//! `fw` / `bios` / `nvdata` — flash-chip region read/write primitives.
//!
//! All three share the same FW_UPLOAD (read) / FW_DOWNLOAD (write) mechanism via
//! `Card::read_region` / `Card::write_region`; only the MPI `ImageType` differs.
//! Named by artifact (fw/bios/nvdata), never "flash" (which collides with the
//! verb). See ADR-007 command-set v2.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use sha2::{Digest, Sha256};

use crate::firmware::guard::{GuardedFlash, WriteIntent};
use crate::mpi::messages::ImageType;

/// Shared read/write subcommands for a flash-chip region.
#[derive(Subcommand, Debug)]
pub enum RegionCommand {
    /// Read this region from the chip (FW_UPLOAD).
    Read(RegionReadArgs),
    /// Write this region to the chip (FW_DOWNLOAD). DESTRUCTIVE — requires --yes.
    Write(RegionWriteArgs),
}

#[derive(Args, Debug)]
pub struct RegionReadArgs {
    /// Output file for the raw region bytes. Default: stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct RegionWriteArgs {
    /// File whose bytes to write to this region.
    #[arg(long, value_name = "PATH")]
    pub from_file: PathBuf,
    /// Confirm this destructive write.
    #[arg(long)]
    pub yes: bool,
}

fn sha_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    format!("{:x}", h.finalize())
}

/// Read a region to a file (or stdout) + print its sha256 to stderr.
pub fn run_read(
    bdf: String,
    image_type: ImageType,
    region: &str,
    out: Option<&std::path::Path>,
) -> Result<(), crate::Error> {
    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("discover_one({}): {}", bdf, e)))?;
    let data = card
        .read_region(image_type)
        .map_err(|e| crate::Error::Other(format!("{} read: {}", region, e)))?;
    eprintln!(
        "{} read: {} bytes, sha256={}",
        region,
        data.len(),
        sha_hex(&data)
    );
    match out {
        Some(p) => std::fs::write(p, &data)?,
        None => {
            use std::io::Write;
            std::io::stdout().lock().write_all(&data)?;
        }
    }
    Ok(())
}

/// Write a region from a file (DESTRUCTIVE; --yes gated), then read-back-verify.
pub fn run_write(
    bdf: String,
    image_type: ImageType,
    region: &str,
    from_file: &std::path::Path,
    yes: bool,
) -> Result<(), crate::Error> {
    let data = std::fs::read(from_file)
        .map_err(|e| crate::Error::Other(format!("read {}: {}", from_file.display(), e)))?;
    if data.is_empty() {
        return Err(crate::Error::Other(format!(
            "{} is empty",
            from_file.display()
        )));
    }
    if !yes {
        return Err(crate::Error::Other(format!(
            "Refusing destructive {} write without --yes ({} bytes, sha256={}).",
            region,
            data.len(),
            sha_hex(&data)
        )));
    }

    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("discover_one({}): {}", bdf, e)))?;

    // All flash writes go through the guarded transaction (ADR-021). For bios/nvdata this is the
    // key fix: previously these writes had NO whole-flash check, so a write that spilled into the
    // boot-critical region (the 2026-05-20 brick mechanism) went undetected. Now every byte of the
    // boot-input zone is verified unchanged against the mandatory pre-write snapshot, and banks are
    // checked for consistency, regardless of which region was targeted.
    let txn = GuardedFlash::begin(&bdf, &snapshot_dir())?;
    txn.commit(
        &mut *card,
        WriteIntent {
            region: image_type,
            bytes: data.clone(),
            label: region.to_string(),
        },
    )?;
    eprintln!(
        "{} write: OK ({} bytes) — guarded transaction verified ✓",
        region,
        data.len()
    );
    Ok(())
}

/// Directory where guarded-flash writes drop the pre-write recovery snapshot (current dir).
fn snapshot_dir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Dispatch a `bios`/`nvdata` region subcommand (fw has its own enum w/ extras).
pub fn run(
    bdf: String,
    image_type: ImageType,
    region: &str,
    sub: RegionCommand,
) -> Result<(), crate::Error> {
    match sub {
        RegionCommand::Read(a) => run_read(bdf, image_type, region, a.out.as_deref()),
        RegionCommand::Write(a) => run_write(bdf, image_type, region, &a.from_file, a.yes),
    }
}
