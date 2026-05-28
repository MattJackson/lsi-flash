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

// HOST_INTERRUPT_STATUS register and bit definitions.
// Cites: references/upstream/lsiutil/lsi/mpi2.h:236-242
pub const MPI2_HISTATUS: u32 = 0x30;

/// Set by the IOC when it has posted a reply dword to DOORBELL — host polls
/// this to know a reply word is available to read. Cites mpi2.h:241-242.
pub const MPI2_HIS_IOC2SYS_DB_STATUS: u32 = 0x00000001;

/// Set while the host's doorbell write is still being processed by the IOC —
/// host polls for this to CLEAR before sending the next dword. Cites mpi2.h:237-238.
pub const MPI2_HIS_SYS2IOC_DB_STATUS: u32 = 0x80000000;

/// DOORBELL register bit indicating a handshake is already in progress.
/// Host must NOT initiate a new handshake while this is set. Cites mpi2.h:181.
pub const MPI2_DOORBELL_USED: u32 = 0x08000000;

/// MPI2 doorbell function code shift — bits 24-31 of the doorbell register
/// hold the function code; bits 16-23 hold (dword_count - 2). Cites
/// mpi2.h:178-193 + lsiutil/mpt.c:819-820.
pub const MPI2_DOORBELL_FUNCTION_SHIFT: u32 = 24;
pub const MPI2_DOORBELL_ADD_DWORDS_SHIFT: u32 = 16;

/// Poll-spin until `HISTATUS & MPI2_HIS_IOC2SYS_DB_STATUS != 0` (IOC has
/// posted a reply word). Returns timeout error if the deadline elapses.
/// Cites lsiutil/mpt.c:691-733 (mpt_wait_for_doorbell).
pub(crate) fn wait_doorbell_int(
    bar1: &[u8],
    timeout: std::time::Duration,
) -> Result<(), &'static str> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let histatus = read32(bar1, MPI2_HISTATUS);
        if histatus & MPI2_HIS_IOC2SYS_DB_STATUS != 0 {
            return Ok(());
        }
        // Check for IOC fault between polls so we bail fast on a wedged chip.
        let doorbell = read32(bar1, MPI2_DOORBELL);
        if doorbell & MPI2_DOORBELL_STATE_MASK == MPI2_DOORBELL_FAULT {
            return Err("wait_doorbell_int: IOC fault");
        }
        std::thread::sleep(std::time::Duration::from_micros(5));
    }
    Err("wait_doorbell_int: timeout")
}

/// Poll-spin until `HISTATUS & MPI2_HIS_SYS2IOC_DB_STATUS == 0` (IOC has
/// consumed the previous host-side doorbell write). Required between every
/// dword we write to DOORBELL during a send. Cites lsiutil/mpt.c:736-777
/// (mpt_wait_for_response).
pub(crate) fn wait_doorbell_consumed(
    bar1: &[u8],
    timeout: std::time::Duration,
) -> Result<(), &'static str> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let histatus = read32(bar1, MPI2_HISTATUS);
        if histatus & MPI2_HIS_SYS2IOC_DB_STATUS == 0 {
            return Ok(());
        }
        let doorbell = read32(bar1, MPI2_DOORBELL);
        if doorbell & MPI2_DOORBELL_STATE_MASK == MPI2_DOORBELL_FAULT {
            return Err("wait_doorbell_consumed: IOC fault");
        }
        std::thread::sleep(std::time::Duration::from_micros(5));
    }
    Err("wait_doorbell_consumed: timeout")
}

/// Acknowledge a doorbell event by clearing HISTATUS (write 0). The IOC
/// uses this as a signal that the host has consumed the posted word and is
/// ready for the next one. Cites lsiutil/mpt.c:818,823,858,862.
#[inline]
pub(crate) fn clear_histatus(bar1: &mut [u8]) {
    write32(bar1, MPI2_HISTATUS, 0);
}

/// Full doorbell handshake send: write function-code header, then each
/// request dword with proper IOC sync between every step. Cites the
/// canonical pattern at lsiutil/mpt.c:781-834.
///
/// `request` is the wire-format request body (header included). Must be
/// dword-aligned (`request.len() % 4 == 0`); caller is responsible for
/// padding.
///
/// `function` is the MPI function code byte (e.g. 0x03 for IOC_FACTS).
///
/// Returns the negotiated doorbell — DOORBELL_USED check passes, function +
/// dword-count is written and ACK'd, payload dwords are streamed with
/// per-dword ack. Subsequent caller invokes `doorbell_handshake_recv` to
/// fetch the reply.
pub(crate) fn doorbell_handshake_send(
    bar1: &mut [u8],
    function: u8,
    request: &[u8],
    timeout: std::time::Duration,
) -> Result<(), &'static str> {
    if request.len() % 4 != 0 {
        return Err("doorbell_handshake_send: request not dword-aligned");
    }
    let dword_count = request.len() / 4;
    if dword_count < 2 {
        return Err("doorbell_handshake_send: request too small (< 2 dwords)");
    }

    // Step 1: refuse to start if a previous handshake is still active.
    let doorbell = read32(bar1, MPI2_DOORBELL);
    if doorbell & MPI2_DOORBELL_USED != 0 {
        return Err("doorbell_handshake_send: doorbell already in use");
    }
    if doorbell & MPI2_DOORBELL_STATE_MASK == MPI2_DOORBELL_FAULT {
        return Err("doorbell_handshake_send: IOC fault before send");
    }

    // Step 2: clear stale HISTATUS, kick the handshake with function-code header.
    // Per mpi2.h:178-193 / mpt.c:819-820 the header dword encodes function in bits
    // 24-31 and (dword_count - 2) in bits 16-23. The "-2" is the MPI 2.0 quirk
    // where the chip subtracts the 2-dword header that's always present.
    clear_histatus(bar1);
    let header = (u32::from(function) << MPI2_DOORBELL_FUNCTION_SHIFT)
        | ((dword_count as u32 - 2) << MPI2_DOORBELL_ADD_DWORDS_SHIFT);
    write32(bar1, MPI2_DOORBELL, header);

    // Step 3: wait for IOC to ack the header (DOORBELL_INT set).
    wait_doorbell_int(bar1, timeout)?;
    clear_histatus(bar1);

    // Step 4: wait for IOC's busy bit to clear so we can post the first payload dword.
    wait_doorbell_consumed(bar1, timeout)?;

    // Step 5: stream each request dword with per-dword sync.
    for chunk_start in (0..request.len()).step_by(4) {
        let dword = u32::from_le_bytes([
            request[chunk_start],
            request[chunk_start + 1],
            request[chunk_start + 2],
            request[chunk_start + 3],
        ]);
        write32(bar1, MPI2_DOORBELL, dword);
        wait_doorbell_consumed(bar1, timeout)?;
    }

    Ok(())
}

/// Full doorbell handshake receive: read MPI reply via DOORBELL register
/// 16 bits at a time, per lsiutil/mpt.c:837-872 (mpt_receive_data). The IOC
/// dynamically tells us how long the reply is via MsgLength in the 2nd U16
/// of the reply, so we ignore `max_bytes` past that and only read what the
/// chip actually sends.
///
/// Returns the reply bytes (length determined by MsgLength * 4).
pub(crate) fn doorbell_handshake_recv(
    bar1: &mut [u8],
    max_bytes: usize,
    timeout: std::time::Duration,
) -> Result<Vec<u8>, &'static str> {
    // Per mpt.c the reply is read U16 at a time from the low 16 bits of DOORBELL.
    // Start by assuming 4 bytes (2 U16 words) until we learn the actual MsgLength.
    let mut real_length_bytes: usize = 4;
    let mut out = Vec::with_capacity(max_bytes);
    let mut i = 0usize;
    while i < real_length_bytes / 2 {
        wait_doorbell_int(bar1, timeout)?;
        let value = (read32(bar1, MPI2_DOORBELL) & 0xFFFF) as u16;
        if i == 1 {
            // Per mpt.c:855 — MsgLength is in the upper byte of the 2nd U16,
            // expressed in dwords. The lower byte holds Function (already
            // validated by the caller). real_length = MsgLength_in_dwords * 4.
            // (mpt.c uses `value & ~0xff00`; we mirror that exactly.)
            real_length_bytes = ((value & !0xff00) as usize) * 4;
            if real_length_bytes < 4 {
                return Err("doorbell_handshake_recv: chip reported zero-length reply");
            }
        }
        // Store as little-endian bytes — caller's parse() expects raw wire bytes.
        if (i * 2) + 2 <= max_bytes {
            out.extend_from_slice(&value.to_le_bytes());
        }
        clear_histatus(bar1);
        i += 1;
    }

    // Final wait + ACK so the chip knows we consumed the last word.
    wait_doorbell_int(bar1, timeout)?;
    clear_histatus(bar1);

    Ok(out)
}

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

// chip_*/dcr_* helpers below mirror lsirec.c's indirect register access
// path. Not used yet — preserved for the upcoming HCB hostboot path
// (cycle 3+) which needs DIAG_RW + DCR indirect reads/writes to drive
// the chip during boot. Allow dead_code until the orchestrator picks
// them up; do NOT delete (every line cites lsirec.c verbatim).
#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
/// DCR-indirect read. Cites lsirec.c:113-117.
#[inline]
fn dcr_read32(bar1: &mut [u8], dcr_address: u32, dcr_data: u32, offset: u32) -> u32 {
    write32(bar1, dcr_address, offset);
    read32(bar1, dcr_data)
}

#[allow(dead_code)]
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
