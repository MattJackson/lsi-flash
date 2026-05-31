//! `fw write` — validated, wizard-style firmware write (FW_DOWNLOAD).
//!
//! Wraps the raw region write with the 5-point validity gate (firmware::validate)
//! and a wizard-style confirmation. The risk model is just "fits + right for the
//! card" (ADR-015 Rule 11a); personality is not a risk axis. `--yes` skips only
//! the interactive confirm — validation always runs and always fails closed.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::firmware::guard::{GuardedFlash, WriteIntent};
use crate::firmware::validate::{check_fit, validate_image};
use crate::mpi::messages::ImageType;

/// Directory where guarded-flash writes drop the pre-write recovery snapshot. Current working
/// directory — discoverable and deterministic (no timestamp in the path; one snapshot per card).
fn snapshot_dir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

fn sha_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    format!("{:x}", h.finalize())
}

/// First `MPTFW-<ver>-<suffix>` banner in a firmware blob, for human-readable
/// version/personality identification (e.g. `MPTFW-20.00.07.00-IE`).
fn fw_banner(data: &[u8]) -> Option<String> {
    let needle = b"MPTFW-";
    let pos = data.windows(needle.len()).position(|w| w == needle)?;
    let end = data[pos..]
        .iter()
        .position(|&b| b == 0 || b == b' ')
        .map(|e| pos + e)
        .unwrap_or(data.len());
    std::str::from_utf8(&data[pos..end]).ok().map(String::from)
}

/// `fw read` — read the firmware region. By default reads the **flash** copy
/// (FW_UPLOAD ITYPE 0x01 = FW_FLASH, the persisted image); `--running` reads
/// the **running** image (ITYPE 0x00 = FW_CURRENT, what the chip booted). The
/// two differ when a `fw write` landed in flash but the chip hasn't reset —
/// comparing them verifies a write without a reboot.
///
/// Per ADR-020: try IOC path (FW_UPLOAD) first; on error fall back to diag-mapped
/// back-door read from BAR1 @0xFC000000 + FLASH_LAYOUT region extraction. Logs which
/// path was used via eprintln. Result bytes are identical either way. Cites: ADR-020
/// line 41 (diag-mapped flash read → IOC FW_UPLOAD ordering).
pub fn run_read(bdf: String, running: bool, out: Option<&Path>) -> Result<(), crate::Error> {
    use crate::card::mpt::{FW_UPLOAD_ITYPE_FW_CURRENT, FW_UPLOAD_ITYPE_FW_FLASH};
    let (itype, label) = if running {
        (FW_UPLOAD_ITYPE_FW_CURRENT, "running (FW_CURRENT 0x00)")
    } else {
        (FW_UPLOAD_ITYPE_FW_FLASH, "flash (FW_FLASH 0x01)")
    };

    // Try IOC path first (healthy card).
    let (data, path_used) = match crate::card::discover_one(&bdf) {
        Ok(mut card) => {
            let data = card.read_region_itype(itype)?;
            (data, "IOC (FW_UPLOAD)")
        }
        Err(e) => {
            // IOC path failed — fall back to diag back-door.
            eprintln!(
                "fw read [{}]: IOC path failed ({}), falling back to diag back-door",
                label, e
            );
            let data = crate::firmware::flash_layout::read_firmware_backdoor(&bdf)?;
            (data, "diag back-door")
        }
    };

    eprintln!(
        "fw read [{}]: {} bytes, sha256={}, banner={}, path={}",
        label,
        data.len(),
        sha_hex(&data),
        fw_banner(&data).unwrap_or_else(|| "?".into()),
        path_used
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

/// Validated firmware write: pre-flight checks 1-4, wizard confirm, write,
/// then read-back verify (check 5).
pub fn run_write(bdf: String, from_file: &Path, yes: bool) -> Result<(), crate::Error> {
    let image = std::fs::read(from_file)
        .map_err(|e| crate::Error::Other(format!("read {}: {}", from_file.display(), e)))?;
    if image.is_empty() {
        return Err(crate::Error::Other(format!(
            "{} is empty",
            from_file.display()
        )));
    }

    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("discover_one({}): {}", bdf, e)))?;

    // Pull the card's current firmware (FW_UPLOAD — non-destructive) so the fit
    // check can infer the active FW-region size from what's already running.
    let current = card
        .read_region(ImageType::Fw)
        .map_err(|e| crate::Error::Other(format!("FW_UPLOAD (for fit check): {}", e)))?;

    // 5-point gate: 1-3 image-only, 4 card-derived. (#5 = read-back, post-write.)
    let mut val = validate_image(&image);
    check_fit(&mut val, &image, &current);

    // Wizard presentation.
    eprintln!();
    eprintln!("fw write — pre-flight validation");
    eprintln!("  image: {} ({} bytes)", from_file.display(), image.len());
    eprintln!("  card:  {}", bdf);
    for c in &val.checks {
        eprintln!(
            "  [{}] {:<12} {}",
            if c.pass { "PASS" } else { "FAIL" },
            c.name,
            c.detail
        );
    }

    if !val.ok() {
        let f = val.first_failure().unwrap();
        return Err(crate::Error::Other(format!(
            "refusing to flash — validation failed at '{}': {}",
            f.name, f.detail
        )));
    }
    eprintln!("  [ -- ] write-integrity  verified by read-back after write");

    if !yes {
        eprint!("\nAll checks passed. Proceed with DESTRUCTIVE fw write? [y/N] ");
        use std::io::Write;
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
            return Err(crate::Error::Other("aborted by user".into()));
        }
    }

    // The guarded transaction (ADR-021) is the ONLY path bytes reach flash: it takes a mandatory
    // pre-write snapshot (recovery image), writes via FW_DOWNLOAD, then enforces the postflight
    // invariants (boot-critical zone unchanged, banks consistent, read-back matches intent) with
    // a severity-driven response. Supersedes the old hand-rolled read-back + whole-flash verify.
    let mut txn = GuardedFlash::begin(&bdf, &snapshot_dir())?;
    txn.commit(
        &mut *card,
        WriteIntent {
            region: ImageType::Fw,
            bytes: image.clone(),
            label: "firmware".into(),
        },
    )?;
    eprintln!(
        "fw write: OK ({} bytes) — guarded transaction verified ✓",
        image.len()
    );
    Ok(())
}
