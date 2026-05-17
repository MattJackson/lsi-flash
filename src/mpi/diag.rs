//! MPI DIAG register operations and IOC state control.
//! Port of lsirec.c:22-35 (DIAG bit masks), 719-791 (reset, halt).

use crate::mpi::doorbell::*;
use thiserror::Error;

/// Error type for DIAG operations. Cites error.rs:61-75.
#[derive(Error, Debug)]
pub enum MpiError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("IOC did not halt (doorbell=0x{doorbell:08x})")]
    IocNotHalted { doorbell: u32 },

    #[error("IOC did not become ready (doorbell=0x{doorbell:08x})")]
    IocNotReady { doorbell: u32 },

    #[error("Device not writable (WRITE_ENABLE bit not set)")]
    DeviceNotWritable,
}

/// Halt the IOC in HCB mode. Cites lsirec.c:760-791.
pub fn halt(bar1: &mut [u8; 4096], ctx: &MpiRegisterContext) -> Result<(), MpiError> {
    println!("Resetting adapter in HCB mode...");

    // Set FORCE_HCB and RESET_ADAPTER bits (lsirec.c:770-774).
    let mut val = read32(bar1, ctx.diag_offset);
    val |= MPI2_DIAG_FORCE_HCB;
    write32(bar1, ctx.diag_offset, val);

    val |= MPI2_DIAG_RESET_ADAPTER;
    write32(bar1, ctx.diag_offset, val);

    // Wait 1 second (lsirec.c:776).
    std::thread::sleep(std::time::Duration::from_millis(1000));

    // Reopen device — caller responsibility.

    // Clear FORCE_HCB bit (lsirec.c:782-784).
    val = read32(bar1, ctx.diag_offset);
    val &= !MPI2_DIAG_FORCE_HCB;
    write32(bar1, ctx.diag_offset, val);

    // Verify IOC stayed in reset (no state bits set) (lsirec.c:786-790).
    let doorbell = read32(bar1, MPI2_DOORBELL);
    if doorbell & MPI2_DOORBELL_STATE_MASK != 0 {
        return Err(MpiError::IocNotHalted { doorbell });
    }

    Ok(())
}

/// Reset the adapter (normal reset, not HCB). Cites lsirec.c:719-758.
pub fn reset(bar1: &mut [u8; 4096], ctx: &MpiRegisterContext) -> Result<(), MpiError> {
    println!("Resetting adapter...");

    // Clear boot device mask and HCB bits (lsirec.c:730-734).
    let mut val = read32(bar1, ctx.diag_offset);
    val &= !MPI2_DIAG_BOOTDEVICE_MASK;
    val &= !MPI2_DIAG_FORCE_HCB;
    val &= !MPI2_DIAG_HCB_MODE;
    write32(bar1, ctx.diag_offset, val);

    // Wait 100ms (lsirec.c:736).
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Set RESET_ADAPTER bit (lsirec.c:738-739).
    val = read32(bar1, ctx.diag_offset);
    val |= MPI2_DIAG_RESET_ADAPTER;
    write32(bar1, ctx.diag_offset, val);

    // Wait 100ms (lsirec.c:741).
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Print IOC state (lsirec.c:743-745).
    let state = get_ioc_state(bar1, MPI2_DOORBELL);
    println!("IOC state after reset: {:?}", state);

    // Wait for READY state (poll up to 2s) (lsirec.c:747-753).
    for _ in 0..200 {
        let db = read32(bar1, MPI2_DOORBELL);
        if db & MPI2_DOORBELL_READY != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Verify READY state (lsirec.c:755-757).
    let db = read32(bar1, MPI2_DOORBELL);
    if db & MPI2_DOORBELL_READY == 0 {
        return Err(MpiError::IocNotReady { doorbell: db });
    }

    println!(
        "IOC state after ready poll: {:?}",
        get_ioc_state(bar1, MPI2_DOORBELL)
    );

    Ok(())
}

/// Read a chip register via DIAG indirect path. Cites lsirec.c:99-104 pattern.
pub fn diag_read_chip(
    bar1: &mut [u8; 4096],
    ctx: &MpiRegisterContext,
    offset: u32,
) -> Result<u32, MpiError> {
    // Enable DIAG RW path first (lsirec.c:146-148 pattern).
    let val = read32(bar1, ctx.diag_offset);
    if val & MPI2_DIAG_WRITE_ENABLE == 0 {
        return Err(MpiError::DeviceNotWritable);
    }

    // Write address (lsirec.c:101-102).
    write32(bar1, ctx.rw_addr_high_offset, 0);
    write32(bar1, ctx.rw_addr_low_offset, offset);

    // Read data (lsirec.c:103).
    Ok(read32(bar1, ctx.rw_data_offset))
}

/// Write a chip register via DIAG indirect path. Cites lsirec.c:106-111 pattern.
pub fn diag_write_chip(
    bar1: &mut [u8; 4096],
    ctx: &MpiRegisterContext,
    offset: u32,
    data: u32,
) -> Result<(), MpiError> {
    // Enable DIAG RW path first (lsirec.c:146-148 pattern).
    let val = read32(bar1, ctx.diag_offset);
    if val & MPI2_DIAG_WRITE_ENABLE == 0 {
        return Err(MpiError::DeviceNotWritable);
    }

    // Write address (lsirec.c:109-110).
    write32(bar1, ctx.rw_addr_high_offset, 0);
    write32(bar1, ctx.rw_addr_low_offset, offset);

    // Write data (lsirec.c:111).
    write32(bar1, ctx.rw_data_offset, data);

    Ok(())
}
