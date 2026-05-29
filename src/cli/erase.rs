//! `lsi-flash erase` — DIAGNOSTIC/EXPERIMENTAL.
//!
//! Sends a raw MPI `TOOLBOX_CLEAN` (CLEAN_FLASH only by default) and prints the
//! firmware's EXACT reply — `IOCStatus` + `IOCLogInfo` — which sas2flsh/lsiutil
//! discard behind a misleading exit-0. Purpose: characterize the Dell erase
//! "lock" — is the erase vetoed by the firmware (and with what code), or do the
//! original tools merely refuse to send it?
//!
//! DESTRUCTIVE if the running firmware honors the clean (erases the flash).
//! Gated behind `--yes`. The card keeps running its already-loaded firmware
//! until reset, so a successful erase leaves a window to re-flash (use
//! `lsi-flash recover --backup-dir <dir>`), but do NOT reset/reboot before
//! re-flashing or the card boots firmware-less.

/// Run `lsi-flash erase`.
pub fn run(
    bdf: Option<String>,
    yes: bool,
    wipe_mfg_pages: bool,
    json: bool,
) -> Result<(), crate::Error> {
    if !yes {
        return Err(crate::Error::Other(
            "Refusing to send TOOLBOX_CLEAN without --yes. This is a real flash-erase \
             command (destructive if the firmware honors it). Ensure a fresh backup \
             exists, then re-run with --yes."
                .to_string(),
        ));
    }

    let bdf = crate::card::resolve_bdf(bdf.as_deref())
        .map_err(|e| crate::Error::Other(format!("{}", e)))?;
    let mut card = crate::card::discover_one(&bdf)
        .map_err(|e| crate::Error::Other(format!("discover_one({}): {}", bdf, e)))?;

    eprintln!(
        "Sending raw TOOLBOX_CLEAN ({}) to {} — capturing firmware reply…",
        if wipe_mfg_pages {
            "CLEAN_FLASH | PERSIST_MANUFACT_PAGES"
        } else {
            "CLEAN_FLASH only"
        },
        bdf
    );

    let report = card
        .erase_flash(wipe_mfg_pages)
        .map_err(|e| crate::Error::Other(format!("erase_flash: {}", e)))?;

    if json {
        println!(
            "{{\"flags_sent\":\"0x{:08x}\",\"ioc_status\":\"0x{:04x}\",\"success\":{},\
             \"ioc_log_info\":\"0x{:08x}\",\"raw_reply_hex\":\"{}\"}}",
            report.flags_sent,
            report.ioc_status,
            report.success,
            report.ioc_log_info,
            report.raw_reply_hex
        );
    } else {
        println!("TOOLBOX_CLEAN reply:");
        println!("  flags sent:   0x{:08x}", report.flags_sent);
        println!("  IOCStatus:    0x{:04x}", report.ioc_status);
        println!("  IOCLogInfo:   0x{:08x}", report.ioc_log_info);
        println!("  raw reply:    {}", report.raw_reply_hex);
        if report.success {
            println!(
                "  => SUCCESS: firmware ACCEPTED the clean. The flash WAS erased. \
                 Re-flash the OEM firmware now (do not reset before reflashing)."
            );
        } else {
            println!(
                "  => REJECTED: firmware refused the clean (non-zero IOCStatus). \
                 Flash unchanged. The IOCStatus/IOCLogInfo above characterize the lock."
            );
        }
    }

    Ok(())
}
