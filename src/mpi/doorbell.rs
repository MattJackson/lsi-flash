//! MPI 2.0 register offsets and doorbell operations.
//! Cites: references/upstream/lsirec-marcan/lsirec.c:12-133

// Doorbell register offset (BAR1 + 0x00). Cites lsirec.c:12.
pub const MPI2_DOORBELL: u32 = 0x00;

// WRSEQ unlock register offset (BAR1 + 0x04). Cites lsirec.c:19.
pub const MPI2_WRSEQ: u32 = 0x04;

// DIAG register offset (MPT mode, BAR1 + 0x08). Cites lsirec.c:21.
pub const MPI2_DIAG: u32 = 0x08;

// MegaRAID mode DIAG register offset (BAR1 + 0xf8). Cites lsirec.c:53.
pub const MR_DIAG: u32 = 0xf8;

// MegaRAID mode WRSEQ register offset (BAR1 + 0xfc). Cites lsirec.c:54.
pub const MR_WRSEQ: u32 = 0xfc;

// Doorbell state masks (lsirec.c:14-17).
pub const MPI2_DOORBELL_STATE_MASK: u32 = 0xF0000000;
pub const MPI2_DOORBELL_READY: u32 = 0x10000000;
pub const MPI2_DOORBELL_OPERATIONAL: u32 = 0x20000000;
pub const MPI2_DOORBELL_FAULT: u32 = 0x40000000;

// DIAG bit masks (lsirec.c:22-35).
pub const MPI2_DIAG_SBR_RELOAD: u32 = 0x2000;
pub const MPI2_DIAG_BOOTDEVICE_MASK: u32 = 0x1800;
pub const MPI2_DIAG_BOOTDEVICE_DEF: u32 = 0x0000;
pub const MPI2_DIAG_BOOTDEVICE_HCDW: u32 = 0x0800; // Host Code Boot
pub const MPI2_DIAG_CLR_FLASH_BAD_SIG: u32 = 0x0400;
pub const MPI2_DIAG_FORCE_HCB: u32 = 0x0200;
pub const MPI2_DIAG_HCB_MODE: u32 = 0x0100;
pub const MPI2_DIAG_WRITE_ENABLE: u32 = 0x0080;
pub const MPI2_DIAG_FLASH_BAD_SIG: u32 = 0x0040;
pub const MPI2_DIAG_RESET_HISTORY: u32 = 0x0020;
pub const MPI2_DIAG_RW_ENABLE: u32 = 0x0010;
pub const MPI2_DIAG_RESET_ADAPTER: u32 = 0x0004;
pub const MPI2_DIAG_HOLD_IOC_RESET: u32 = 0x0002;

// DIAG read/write registers (lsirec.c:36-38).
pub const MPI2_DIAG_RW_DATA: u32 = 0x10;
pub const MPI2_DIAG_RW_ADDRESS_LOW: u32 = 0x14;
pub const MPI2_DIAG_RW_ADDRESS_HIGH: u32 = 0x18;

// MegaRAID read/write registers (lsirec.c:50-52).
pub const MR_DIAG_RW_DATA: u32 = 0x24;
pub const MR_DIAG_RW_ADDRESS_LOW: u32 = 0x28;
pub const MR_DIAG_RW_ADDRESS_HIGH: u32 = 0x2c;

// DCR indirect registers (lsirec.c:40-41).
pub const MPI2_DCR_DATA: u32 = 0x38;
pub const MPI2_DCR_ADDRESS: u32 = 0x3c;

/// Read 32-bit value from BAR1 offset with volatile semantics. Cites lsirec.c:89-92.
#[inline]
pub(crate) fn read32(bar1: &[u8], offset: u32) -> u32 {
    let ptr = unsafe { bar1.as_ptr().add(offset as usize) } as *const u32;
    unsafe { std::ptr::read_volatile(ptr) }
}

/// Write 32-bit value to BAR1 offset with volatile semantics. Cites lsirec.c:94-97.
#[inline]
pub(crate) fn write32(bar1: &mut [u8], offset: u32, data: u32) {
    let ptr = unsafe { bar1.as_mut_ptr().add(offset as usize) } as *mut u32;
    unsafe { std::ptr::write_volatile(ptr, data) }
}

/// Chip-indirect read via DIAG_RW registers. Cites lsirec.c:99-104.
#[inline]
fn chip_read32(
    bar1: &[u8],
    r_rw_addr_high: u32,
    r_rw_addr_low: u32,
    r_rw_data: u32,
    offset: u32,
) -> u32 {
    let mut bar1_mut = bar1.to_vec();
    write32(&mut bar1_mut, r_rw_addr_high, 0);
    write32(&mut bar1_mut, r_rw_addr_low, offset);
    read32(&bar1_mut, r_rw_data)
}

/// Chip-indirect write via DIAG_RW registers. Cites lsirec.c:106-111.
#[inline]
fn chip_write32(
    bar1: &mut [u8],
    r_rw_addr_high: u32,
    r_rw_addr_low: u32,
    r_rw_data: u32,
    offset: u32,
    data: u32,
) {
    write32(bar1, r_rw_addr_high, 0);
    write32(bar1, r_rw_addr_low, offset);
    write32(bar1, r_rw_data, data);
}

/// DCR-indirect read. Cites lsirec.c:113-117.
#[inline]
fn dcr_read32(bar1: &mut [u8], dcr_address: u32, dcr_data: u32, offset: u32) -> u32 {
    write32(bar1, dcr_address, offset);
    read32(bar1, dcr_data)
}

/// DCR-indirect write. Cites lsirec.c:119-123.
#[inline]
fn dcr_write32(bar1: &mut [u8], dcr_address: u32, dcr_data: u32, offset: u32, data: u32) {
    write32(bar1, dcr_address, offset);
    write32(bar1, dcr_data, data);
}

/// WRSEQ unlock sequence (6-step pattern). Cites lsirec.c:125-133.
pub const WRSEQ_UNLOCK_SEQ: &[u32] = &[0x00, 0x04, 0x0b, 0x02, 0x07, 0x0d];

/// Unlock WRSEQ for register writes. Cites lsirec.c:125-133.
pub fn lsi_unlock(bar1: &mut [u8], wrseq_offset: u32) {
    for &val in WRSEQ_UNLOCK_SEQ {
        write32(bar1, wrseq_offset, val);
    }
}

/// IOC state enum from doorbell value. Cites lsirec.c:632-649 pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IocState {
    Reset,       // No state bits set
    Ready,       // MPI2_DOORBELL_READY set
    Operational, // MPI2_DOORBELL_READY | MPI2_DOORBELL_OPERATIONAL set
    Fault,       // MPI2_DOORBELL_FAULT set
}

/// Check doorbell state. Cites lsirec.c:632-649 pattern.
pub fn get_ioc_state(bar1: &[u8], doorbell_offset: u32) -> IocState {
    let db = read32(bar1, doorbell_offset);

    if db & MPI2_DOORBELL_STATE_MASK == 0 {
        return IocState::Reset;
    }

    if db & MPI2_DOORBELL_FAULT != 0 {
        return IocState::Fault;
    }

    if db & MPI2_DOORBELL_OPERATIONAL != 0 {
        return IocState::Operational;
    }

    if db & MPI2_DOORBELL_READY != 0 {
        return IocState::Ready;
    }

    IocState::Reset // Default fallback
}

/// Context for MPI register access (MPT vs MegaRAID mode). Cites lsirec.c:72-87, 139-143.
#[derive(Debug, Clone)]
pub struct MpiRegisterContext {
    pub doorbell_offset: u32,
    pub wrseq_offset: u32,
    pub diag_offset: u32,
    pub rw_data_offset: u32,
    pub rw_addr_low_offset: u32,
    pub rw_addr_high_offset: u32,
}

impl MpiRegisterContext {
    /// Create context for MPT mode (default). Cites lsirec.c:139-143.
    pub fn mpt_mode() -> Self {
        Self {
            doorbell_offset: MPI2_DOORBELL,
            wrseq_offset: MPI2_WRSEQ,
            diag_offset: MPI2_DIAG,
            rw_data_offset: MPI2_DIAG_RW_DATA,
            rw_addr_low_offset: MPI2_DIAG_RW_ADDRESS_LOW,
            rw_addr_high_offset: MPI2_DIAG_RW_ADDRESS_HIGH,
        }
    }

    /// Create context for MegaRAID mode. Cites lsirec.c:166-171.
    pub fn megaraid_mode() -> Self {
        Self {
            doorbell_offset: MPI2_DOORBELL,
            wrseq_offset: MR_WRSEQ,
            diag_offset: MR_DIAG,
            rw_data_offset: MR_DIAG_RW_DATA,
            rw_addr_low_offset: MR_DIAG_RW_ADDRESS_LOW,
            rw_addr_high_offset: MR_DIAG_RW_ADDRESS_HIGH,
        }
    }
}

/// Error type for MPI register operations. Cites error.rs:51-58.
#[derive(thiserror::Error, Debug)]
pub enum MpiRegisterError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid BAR1 mapping size (expected 4096 bytes)")]
    InvalidBarSize,
}
