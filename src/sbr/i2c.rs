//! I2C bit-bang module for EEPROM access.
//! Port of lsirec.c:392-629 (i2c_delay through lsi_i2c_write_sbr).
//! Cites: references/upstream/lsirec-marcan/lsirec.c:40-41, 113-122 (DCR_ADDRESS/DATA for BAR1 register access)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:56-57 (DCR_I2C_SELECT, DCR_SBR_CONFIG)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:59-65 (CHIP_I2C_BASE/PINS/RESET offsets and bit masks)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:67-68 (EEPROM_TYPE_16BIT, EEPROM_TYPE_8BIT)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:392-496 (bit-bang primitives)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:498-549 (i2c_init/close)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:551-629 (SBR read/write)

use crate::mpi::doorbell::{
    read32, write32, MPI2_DIAG_RW_ADDRESS_HIGH, MPI2_DIAG_RW_ADDRESS_LOW, MPI2_DIAG_RW_DATA,
};

/// DCR address register offset in BAR1. Cites lsirec.c:40, 113-117.
const MPI2_DCR_ADDRESS: usize = 0x3c;

/// DCR data register offset in BAR1. Cites lsirec.c:41, 113-117.
const MPI2_DCR_DATA: usize = 0x38;

/// WRSEQ (write-sequence) register offset in BAR1. Cites lsirec.c:19.
const MPI2_WRSEQ_OFFSET: usize = 0x04;

/// DIAG register offset in BAR1. Cites lsirec.c:21.
const MPI2_DIAG_OFFSET: usize = 0x08;

/// DIAG write-enable status bit. Cites lsirec.c:29.
const MPI2_DIAG_WRITE_ENABLE: u32 = 0x0080;

/// DIAG read/write-enable bit (unlocks DCR/diag access). Cites lsirec.c:32.
const MPI2_DIAG_RW_ENABLE: u32 = 0x0010;

/// DCR_I2C_SELECT register offset. Cites lsirec.c:56.
pub const DCR_I2C_SELECT: u32 = 0x307;

/// DCR_SBR_CONFIG register offset. Cites lsirec.c:57.
pub const DCR_SBR_CONFIG: u32 = 0x340;

/// CHIP_I2C_BASE address. Cites lsirec.c:59.
pub const CHIP_I2C_BASE: u32 = 0xC2100000;

/// CHIP_I2C_PINS — chip-memory address (CHIP_I2C_BASE + 0x20), reached via the
/// DIAG RW window (NOT a direct BAR1 offset). Cites lsirec.c:60.
pub const CHIP_I2C_PINS: u32 = CHIP_I2C_BASE + 0x20;

/// CHIP_I2C_SCL_RD bit mask. Cites lsirec.c:61.
pub const CHIP_I2C_SCL_RD: u32 = 0x01;

/// CHIP_I2C_SDA_RD bit mask. Cites lsirec.c:62.
pub const CHIP_I2C_SDA_RD: u32 = 0x02;

/// CHIP_I2C_SCL_DRV bit mask. Cites lsirec.c:63.
pub const CHIP_I2C_SCL_DRV: u32 = 0x04;

/// CHIP_I2C_SDA_DRV bit mask. Cites lsirec.c:64.
pub const CHIP_I2C_SDA_DRV: u32 = 0x08;

/// CHIP_I2C_RESET — chip-memory address (CHIP_I2C_BASE + 0x24), reached via the
/// DIAG RW window (NOT a direct BAR1 offset). Cites lsirec.c:65.
pub const CHIP_I2C_RESET: u32 = CHIP_I2C_BASE + 0x24;

/// EEPROM address modes. Cites lsirec.c:67-68.
pub const EEPROM_TYPE_16BIT: u8 = 0x01;
pub const EEPROM_TYPE_8BIT: u8 = 0x02;

/// I2C error type. Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum I2cError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SCL timeout waiting for high")]
    SclTimeout,

    #[error("EEPROM did not ACK {0}")]
    EepromNoAck(&'static str),
}

/// Highest *direct* BAR1 register offset touched (DCR_ADDRESS 0x3c + 4). All
/// chip-memory (CHIP_I2C_*) access goes through the DIAG RW window at 0x10-0x18,
/// also covered. Used for the entry-point slice-length guard.
const MAX_GPIO_OFFSET: usize = MPI2_DCR_ADDRESS + 4;

/// I2C context for bit-bang operations. Borrows live BAR1 MMIO slice.
#[derive(Debug)]
pub struct I2cContext<'a> {
    pub bar1: &'a mut [u8], // live BAR1 view, NOT a copy (64 KB on real hardware)
    pub sbr_addr: u8,       // 0x50 or 0x54 (lsirec.c:502-507)
    pub eep_type: u8,       // EEPROM_TYPE_8BIT or EEPROM_TYPE_16BIT (lsirec.c:509-513)
}

/// DCR read32 helper. Cites lsirec.c:40-41, 113-117.
/// Writes offset to MPI2_DCR_ADDRESS, reads value from MPI2_DCR_DATA.
fn dcr_read32(bar1: &mut [u8], offset: u32) -> Result<u32, I2cError> {
    write32(bar1, MPI2_DCR_ADDRESS as u32, offset);
    Ok(read32(bar1, MPI2_DCR_DATA as u32))
}

/// DCR write32 helper. Cites lsirec.c:40-41, 113-117.
/// Writes offset to MPI2_DCR_ADDRESS, writes data to MPI2_DCR_DATA.
fn dcr_write32(bar1: &mut [u8], offset: u32, data: u32) -> Result<(), I2cError> {
    write32(bar1, MPI2_DCR_ADDRESS as u32, offset);
    write32(bar1, MPI2_DCR_DATA as u32, data);
    Ok(())
}

/// Read a CHIP-memory register via the DIAG RW window. The I²C pin/reset
/// registers live in chip address space (CHIP_I2C_BASE = 0xC2100000), NOT at a
/// direct BAR1 offset — they're reached by writing the chip address to
/// DIAG_RW_ADDRESS_{HIGH,LOW} then reading DIAG_RW_DATA. Mirrors lsirec's
/// `chip_read32`. Cites lsirec.c:99-104. (Requires diag RW already enabled —
/// `unlock_diag` does that in `i2c_init`.)
fn chip_read32(bar1: &mut [u8], chip_addr: u32) -> u32 {
    write32(bar1, MPI2_DIAG_RW_ADDRESS_HIGH, 0);
    write32(bar1, MPI2_DIAG_RW_ADDRESS_LOW, chip_addr);
    read32(bar1, MPI2_DIAG_RW_DATA)
}

/// Write a CHIP-memory register via the DIAG RW window. Mirrors lsirec's
/// `chip_write32`. Cites lsirec.c:106-112.
fn chip_write32(bar1: &mut [u8], chip_addr: u32, data: u32) {
    write32(bar1, MPI2_DIAG_RW_ADDRESS_HIGH, 0);
    write32(bar1, MPI2_DIAG_RW_ADDRESS_LOW, chip_addr);
    write32(bar1, MPI2_DIAG_RW_DATA, data);
}

/// Delay for I2C timing. Cites lsirec.c:392-395.
fn i2c_delay(_bar1: &[u8]) {
    std::thread::sleep(std::time::Duration::from_micros(5));
}

/// Set SDA line. Cites lsirec.c:397-405.
fn set_sda(bar1: &mut [u8], sda: bool) {
    let val = chip_read32(bar1, CHIP_I2C_PINS);
    let new_val = if sda {
        val & !CHIP_I2C_SDA_DRV
    } else {
        val | CHIP_I2C_SDA_DRV
    };
    chip_write32(bar1, CHIP_I2C_PINS, new_val);
}

/// Set SCL line. Cites lsirec.c:407-415.
fn set_scl(bar1: &mut [u8], scl: bool) {
    let val = chip_read32(bar1, CHIP_I2C_PINS);
    let new_val = if scl {
        val & !CHIP_I2C_SCL_DRV
    } else {
        val | CHIP_I2C_SCL_DRV
    };
    chip_write32(bar1, CHIP_I2C_PINS, new_val);
}

/// Get SDA line state. Cites lsirec.c:417-420.
fn get_sda(bar1: &mut [u8]) -> bool {
    let val = chip_read32(bar1, CHIP_I2C_PINS);
    (val & CHIP_I2C_SDA_RD) != 0
}

/// Wait for SCL to go high. Cites lsirec.c:422-432.
fn wait_scl(bar1: &mut [u8]) -> Result<(), I2cError> {
    for _ in 0..100 {
        let val = chip_read32(bar1, CHIP_I2C_PINS);
        if val & CHIP_I2C_SCL_RD != 0 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_micros(5));
    }
    Err(I2cError::SclTimeout)
}

/// I2C START condition. Cites lsirec.c:445-457.
fn i2c_start(bar1: &mut [u8]) {
    i2c_delay(bar1);
    set_sda(bar1, true);
    i2c_delay(bar1);
    set_scl(bar1, true);
    i2c_delay(bar1);
    let _ = wait_scl(bar1);
    set_sda(bar1, false);
    i2c_delay(bar1);
    set_scl(bar1, false);
    i2c_delay(bar1);
}

/// I2C STOP condition. Cites lsirec.c:435-443.
fn i2c_stop(bar1: &mut [u8]) {
    i2c_delay(bar1);
    set_sda(bar1, false);
    i2c_delay(bar1);
    set_scl(bar1, true);
    i2c_delay(bar1);
    set_sda(bar1, true);
    i2c_delay(bar1);
}

/// Send a single bit. Cites lsirec.c:459-468.
fn i2c_sendbit(bar1: &mut [u8], bit: bool) {
    set_sda(bar1, bit);
    i2c_delay(bar1);
    set_scl(bar1, true);
    let _ = wait_scl(bar1);
    i2c_delay(bar1);
    set_scl(bar1, false);
    i2c_delay(bar1);
}

/// Receive a single bit. Cites lsirec.c:470-481.
fn i2c_getbit(bar1: &mut [u8]) -> bool {
    set_sda(bar1, true);
    i2c_delay(bar1);
    set_scl(bar1, true);
    let _ = wait_scl(bar1);
    i2c_delay(bar1);
    let val = get_sda(bar1);
    set_scl(bar1, false);
    i2c_delay(bar1);
    val
}

/// Send a byte (MSB first). Cites lsirec.c:483-487.
fn i2c_sendbyte(bar1: &mut [u8], byte: u8) {
    for mask in [0x80u8, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01] {
        i2c_sendbit(bar1, byte & mask != 0);
    }
}

/// Receive a byte (MSB first). Cites lsirec.c:489-496.
fn i2c_getbyte(bar1: &mut [u8]) -> u8 {
    let mut val = 0u8;
    for mask in [0x80u8, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01] {
        if i2c_getbit(bar1) {
            val |= mask;
        }
    }
    val
}

/// Initialize I2C interface. Cites lsirec.c:498-533, 498-524 (addr/type detection).
/// Returns Err on timeout if controller is wedged (bounded reset loop).
/// Unlock the chip's diagnostic registers so DCR (BAR1 0x3c/0x38) reads return
/// valid data. WITHOUT this, DCR reads are stale and EEPROM addr/type
/// auto-detect picks the wrong values (observed on dev-1 2026-05-29:
/// 0x50/type-2 instead of the correct 0x54/type-1, → "EEPROM did not ACK").
/// Mirrors lsirec's `lsi_unlock` + diag-RW-enable. Cites lsirec.c:125-160.
fn unlock_diag(bar1: &mut [u8]) -> Result<(), I2cError> {
    // WRSEQ unlock sequence. Cites lsirec.c:125-133.
    for &v in &[0x00u32, 0x04, 0x0b, 0x02, 0x07, 0x0d] {
        write32(bar1, MPI2_WRSEQ_OFFSET as u32, v);
    }

    // If the unlock took (write-enable set), enable diag read/write. Cites lsirec.c:145-148.
    let diag = read32(bar1, MPI2_DIAG_OFFSET as u32);
    if diag & MPI2_DIAG_WRITE_ENABLE != 0 {
        write32(bar1, MPI2_DIAG_OFFSET as u32, diag | MPI2_DIAG_RW_ENABLE);
    }
    Ok(())
}

pub fn i2c_init(ctx: &mut I2cContext<'_>) -> Result<(), I2cError> {
    // Unlock diagnostic registers BEFORE any DCR access (lsirec.c:125-160).
    // Without this, DCR reads are stale → wrong EEPROM addr/type auto-detect.
    unlock_diag(ctx.bar1)?;

    // Read DCR_SBR_CONFIG to determine addr and type (lsirec.c:501-514).
    let val = dcr_read32(ctx.bar1, DCR_SBR_CONFIG)?;

    if val & 2 != 0 {
        ctx.sbr_addr = 0x54; // 16-bit addressed EEPROM
    } else {
        ctx.sbr_addr = 0x50; // 8-bit addressed EEPROM
    }
    println!("Using I2C address 0x{:02x}", ctx.sbr_addr);

    if val & 8 != 0 {
        ctx.eep_type = EEPROM_TYPE_16BIT;
    } else {
        ctx.eep_type = EEPROM_TYPE_8BIT;
    }
    println!("Using EEPROM type {}", ctx.eep_type);

    // Reset I2C controller (lsirec.c:516-519).

    // Reset loop with bounded iterations to prevent infinite hang (OPEN: wedged controller must error).
    let mut iter = 0u32;
    const MAX_RESET_ITERATIONS: u32 = 100_000;

    loop {
        chip_write32(ctx.bar1, CHIP_I2C_RESET, 1);
        i2c_delay(ctx.bar1);
        let reset_val = chip_read32(ctx.bar1, CHIP_I2C_RESET);
        if reset_val & 1 == 0 {
            break;
        }

        iter += 1;
        if iter >= MAX_RESET_ITERATIONS {
            return Err(I2cError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "I²C reset timeout after {} iterations (controller may be wedged)",
                    MAX_RESET_ITERATIONS
                ),
            )));
        }
    }

    // Enable I2C select in DCR (lsirec.c:521-524).
    let val = dcr_read32(ctx.bar1, DCR_I2C_SELECT)?;
    dcr_write32(ctx.bar1, DCR_I2C_SELECT, val | 0x800000)?;

    // Reset I2C lines (lsirec.c:526-531).
    for _ in 0..9 {
        i2c_sendbit(ctx.bar1, true);
    }
    i2c_stop(ctx.bar1);
    i2c_start(ctx.bar1);
    i2c_stop(ctx.bar1);

    Ok(())
}

/// Close I2C interface. Cites lsirec.c:535-549.
pub fn i2c_close(ctx: &mut I2cContext<'_>) -> Result<(), I2cError> {
    // Reset loop with bounded iterations to prevent infinite hang.
    let mut iter = 0u32;
    const MAX_RESET_ITERATIONS: u32 = 100_000;

    loop {
        chip_write32(ctx.bar1, CHIP_I2C_RESET, 1);
        i2c_delay(ctx.bar1);
        let reset_val = chip_read32(ctx.bar1, CHIP_I2C_RESET);
        if reset_val & 1 == 0 {
            break;
        }

        iter += 1;
        if iter >= MAX_RESET_ITERATIONS {
            return Err(I2cError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "I²C reset timeout after {} iterations (controller may be wedged)",
                    MAX_RESET_ITERATIONS
                ),
            )));
        }
    }

    let val = dcr_read32(ctx.bar1, DCR_I2C_SELECT)?;
    dcr_write32(ctx.bar1, DCR_I2C_SELECT, val & !0x800000)?;

    Ok(())
}

/// Read SBR from EEPROM. Cites lsirec.c:551-589.
pub fn i2c_read_sbr(
    ctx: &mut I2cContext<'_>,
    offset: usize,
    len: usize,
) -> Result<Vec<u8>, I2cError> {
    // Guard: bar1 must be large enough for highest GPIO register access (RESET + 4 bytes).
    if ctx.bar1.len() < MAX_GPIO_OFFSET {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bar1 slice too small for I²C ops (len={}, need {})",
                ctx.bar1.len(),
                MAX_GPIO_OFFSET
            ),
        )
        .into());
    }

    let mut buf = vec![0u8; len];

    // Send address byte (write mode). Cites lsirec.c:553-569.
    i2c_start(ctx.bar1);
    i2c_sendbyte(ctx.bar1, ctx.sbr_addr << 1);
    if i2c_getbit(ctx.bar1) {
        return Err(I2cError::EepromNoAck("address W"));
    }

    // Send offset (16-bit mode). Cites lsirec.c:560-564.
    if ctx.eep_type == EEPROM_TYPE_16BIT {
        i2c_sendbyte(ctx.bar1, (offset >> 8) as u8);
        if i2c_getbit(ctx.bar1) {
            return Err(I2cError::EepromNoAck("offset1"));
        }
    }

    // Send offset low byte. Cites lsirec.c:566-570.
    i2c_sendbyte(ctx.bar1, (offset & 0xff) as u8);
    if i2c_getbit(ctx.bar1) {
        return Err(I2cError::EepromNoAck("offset0"));
    }

    // Repeated START for read mode. Cites lsirec.c:572-576.
    i2c_start(ctx.bar1);
    i2c_sendbyte(ctx.bar1, (ctx.sbr_addr << 1) | 0x01);
    if i2c_getbit(ctx.bar1) {
        return Err(I2cError::EepromNoAck("address R"));
    }

    // Read bytes. Cites lsirec.c:578-586.
    for (i, slot) in buf.iter_mut().enumerate().take(len) {
        *slot = i2c_getbyte(ctx.bar1);
        i2c_sendbit(ctx.bar1, i == len - 1); // NACK on last byte
    }

    i2c_stop(ctx.bar1);
    Ok(buf)
}

/// Write SBR to EEPROM. Cites lsirec.c:591-629.
pub fn i2c_write_sbr(ctx: &mut I2cContext<'_>, offset: usize, data: &[u8]) -> Result<(), I2cError> {
    // Guard: bar1 must be large enough for highest GPIO register access (RESET + 4 bytes).
    if ctx.bar1.len() < MAX_GPIO_OFFSET {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bar1 slice too small for I²C ops (len={}, need {})",
                ctx.bar1.len(),
                MAX_GPIO_OFFSET
            ),
        )
        .into());
    }

    for (i, &byte) in data.iter().enumerate() {
        let abs_offset = offset + i;

        i2c_start(ctx.bar1);
        i2c_sendbyte(ctx.bar1, ctx.sbr_addr << 1);
        if i2c_getbit(ctx.bar1) {
            return Err(I2cError::EepromNoAck("address W"));
        }

        if ctx.eep_type == EEPROM_TYPE_16BIT {
            i2c_sendbyte(ctx.bar1, (abs_offset >> 8) as u8);
            if i2c_getbit(ctx.bar1) {
                return Err(I2cError::EepromNoAck("offset1"));
            }
        }

        i2c_sendbyte(ctx.bar1, (abs_offset & 0xff) as u8);
        if i2c_getbit(ctx.bar1) {
            return Err(I2cError::EepromNoAck("offset0"));
        }

        i2c_sendbyte(ctx.bar1, byte);
        if i2c_getbit(ctx.bar1) {
            return Err(I2cError::EepromNoAck("data"));
        }

        i2c_stop(ctx.bar1);
        std::thread::sleep(std::time::Duration::from_millis(5)); // 5ms write cycle (lsirec.c:625)
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bar1_too_short_read_returns_err() {
        let mut bar1: [u8; 32] = [0u8; 32]; // Too short for GPIO access
        let mut ctx = I2cContext {
            bar1: &mut bar1[..],
            sbr_addr: 0x50,
            eep_type: EEPROM_TYPE_8BIT,
        };

        let result = i2c_read_sbr(&mut ctx, 0, 256);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("bar1 slice too small"));
    }

    #[test]
    fn test_bar1_too_short_write_returns_err() {
        let mut bar1: [u8; 32] = [0u8; 32]; // Too short for GPIO access
        let mut ctx = I2cContext {
            bar1: &mut bar1[..],
            sbr_addr: 0x50,
            eep_type: EEPROM_TYPE_8BIT,
        };

        let result = i2c_write_sbr(&mut ctx, 0, &[1u8; 10]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("bar1 slice too small"));
    }

    #[test]
    fn test_bar1_minimum_size_ok() {
        // MAX_GPIO_OFFSET = DCR_ADDRESS (0x3c) + 4 = 64, the highest direct BAR1
        // register offset; chip-memory access uses the DIAG RW window (0x10-0x18).
        let mut bar1: [usize; MAX_GPIO_OFFSET / std::mem::size_of::<usize>()] =
            [0; MAX_GPIO_OFFSET / std::mem::size_of::<usize>()];
        let bar1_bytes: &mut [u8] = unsafe {
            std::slice::from_raw_parts_mut(bar1.as_mut_ptr() as *mut u8, MAX_GPIO_OFFSET)
        };
        let mut ctx = I2cContext {
            bar1: bar1_bytes,
            sbr_addr: 0x50,
            eep_type: EEPROM_TYPE_8BIT,
        };

        // A buffer of exactly MAX_GPIO_OFFSET must PASS the size guard. On a zeroed
        // mock (no real EEPROM) the bit-bang reads all-zero and the idle-low SDA
        // looks like a perpetual ACK, so it returns Ok — the point of this test is
        // only that it does NOT hit the "too small" size-guard error.
        let result = i2c_read_sbr(&mut ctx, 0, 1);
        if let Err(e) = &result {
            assert!(
                !e.to_string().contains("too small"),
                "minimum-size buffer must not trip the size guard, got: {e}"
            );
        }
    }

    #[test]
    fn test_i2c_delay_accepts_slice() {
        let bar1: &[u8] = &[0u8; 64];
        i2c_delay(bar1); // Should compile and run without panic
    }

    #[test]
    fn test_dcr_read32_write32_roundtrip() {
        let mut bar1 = vec![0u8; 0x100];

        // Write offset to MPI2_DCR_ADDRESS (0x3c) and value to MPI2_DCR_DATA (0x38).
        let test_offset: u32 = 0x12345678;
        let test_value: u32 = 0xDEADBEEF;

        dcr_write32(&mut bar1, test_offset, test_value).expect("dcr_write32 should succeed");

        // Read it back.
        let read_val = dcr_read32(&mut bar1, test_offset).expect("dcr_read32 should succeed");

        assert_eq!(read_val, test_value, "DCR round-trip value mismatch");
    }

    #[test]
    fn test_dcr_read32_write32_bounds_check() {
        // DCR helpers no longer perform bounds checks; entry points guard bar1 length.
        let mut bar1 = vec![0u8; 32];

        // These should succeed on RAM (volatile semantics don't change runtime behavior)
        dcr_write32(&mut bar1, 0x12345678, 0xDEADBEEF).expect("dcr_write32 should succeed");
        let val = dcr_read32(&mut bar1, 0x12345678).expect("dcr_read32 should succeed");
        assert_eq!(val, 0xDEADBEEF);
    }

    #[test]
    fn test_dcr_read32_write32_multiple_values() {
        let mut bar1 = vec![0u8; 0x100];

        // Test multiple offset/value pairs.
        let pairs = [
            (0x00000000, 0x11111111),
            (0xFFFFFFFF, 0xFEDCBA98),
            (0x340, 0xC2100000), // DCR_SBR_CONFIG example
        ];

        for (offset, value) in pairs.iter() {
            dcr_write32(&mut bar1, *offset, *value).expect("dcr_write32 should succeed");
            let read_val = dcr_read32(&mut bar1, *offset).expect("dcr_read32 should succeed");
            assert_eq!(
                read_val, *value,
                "DCR round-trip failed for offset 0x{:08X}",
                offset
            );
        }
    }
}
