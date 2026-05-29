//! `fw write` — validated, wizard-style firmware write (FW_DOWNLOAD).
//!
//! Wraps the raw region write with the 5-point validity gate (firmware::validate)
//! and a wizard-style confirmation. The risk model is just "fits + right for the
//! card" (ADR-015 Rule 11a); personality is not a risk axis. `--yes` skips only
//! the interactive confirm — validation always runs and always fails closed.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::firmware::validate::{check_fit, validate_image};
use crate::mpi::messages::ImageType;

fn sha_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    format!("{:x}", h.finalize())
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

    // Write (FW_DOWNLOAD).
    card.write_region(ImageType::Fw, &image)
        .map_err(|e| crate::Error::Other(format!("fw write: {}", e)))?;

    // Check 5 — read-back verify (ADR-015 Rule 5).
    match card.read_region(ImageType::Fw) {
        Ok(rb) => {
            let want = sha_hex(&image);
            let got = sha_hex(&rb[..rb.len().min(image.len())]);
            if want == got {
                eprintln!("fw write: OK ({} bytes), read-back verified ✓", image.len());
            } else {
                return Err(crate::Error::Other(format!(
                    "fw write: read-back MISMATCH (wrote {} got {}) — investigate before trusting",
                    want, got
                )));
            }
        }
        Err(e) => eprintln!(
            "fw write: OK ({} bytes), but read-back verify failed: {} (verify manually)",
            image.len(),
            e
        ),
    }
    Ok(())
}
