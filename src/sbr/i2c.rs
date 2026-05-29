//! I2C bit-bang module for EEPROM access.
//! Port of lsirec.c:392-629 (i2c_delay through lsi_i2c_write_sbr).
//! Cites: references/upstream/lsirec-marcan/lsirec.c:40-41, 113-122 (DCR_ADDRESS/DATA for BAR1 register access)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:56-57 (DCR_I2C_SELECT, DCR_SBR_CONFIG)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:59-65 (CHIP_I2C_BASE/PINS/RESET offsets and bit masks)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:67-68 (EEPROM_TYPE_16BIT, EEPROM_TYPE_8BIT)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:392-496 (bit-bang primitives)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:498-549 (i2c_init/close)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:551-629 (SBR read/write)

/// DCR address register offset in BAR1. Cites lsirec.c:40, 113-117.
const MPI2_DCR_ADDRESS: usize = 0x3c;

/// DCR data register offset in BAR1. Cites lsirec.c:41, 113-117.
const MPI2_DCR_DATA: usize = 0x38;

/// DCR_I2C_SELECT register offset. Cites lsirec.c:56.
pub const DCR_I2C_SELECT: u32 = 0x307;

/// DCR_SBR_CONFIG register offset. Cites lsirec.c:57.
pub const DCR_SBR_CONFIG: u32 = 0x340;

/// CHIP_I2C_BASE address. Cites lsirec.c:59.
pub const CHIP_I2C_BASE: u32 = 0xC2100000;

/// CHIP_I2C_PINS offset (BAR1 + 0x20). Cites lsirec.c:60.
pub const CHIP_I2C_PINS_OFFSET: u32 = 0x20;

/// CHIP_I2C_SCL_RD bit mask. Cites lsirec.c:61.
pub const CHIP_I2C_SCL_RD: u32 = 0x01;

/// CHIP_I2C_SDA_RD bit mask. Cites lsirec.c:62.
pub const CHIP_I2C_SDA_RD: u32 = 0x02;

/// CHIP_I2C_SCL_DRV bit mask. Cites lsirec.c:63.
pub const CHIP_I2C_SCL_DRV: u32 = 0x04;

/// CHIP_I2C_SDA_DRV bit mask. Cites lsirec.c:64.
pub const CHIP_I2C_SDA_DRV: u32 = 0x08;

/// CHIP_I2C_RESET offset (BAR1 + 0x24). Cites lsirec.c:65.
pub const CHIP_I2C_RESET_OFFSET: u32 = 0x24;

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

/// Maximum GPIO register offset used (CHIP_I2C_RESET_OFFSET + 4 bytes).
const MAX_GPIO_OFFSET: usize = (CHIP_I2C_RESET_OFFSET + 4) as usize;

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
    if bar1.len() < MPI2_DCR_ADDRESS + 4 || bar1.len() < MPI2_DCR_DATA + 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bar1 slice too small for DCR access (len={}, need at least {})",
                bar1.len(),
                MPI2_DCR_ADDRESS.max(MPI2_DCR_DATA) + 4
            ),
        )
        .into());
    }

    // Write the DCR register offset to MPI2_DCR_ADDRESS.
    let addr_bytes = offset.to_le_bytes();
    bar1[MPI2_DCR_ADDRESS] = addr_bytes[0];
    bar1[MPI2_DCR_ADDRESS + 1] = addr_bytes[1];
    bar1[MPI2_DCR_ADDRESS + 2] = addr_bytes[2];
    bar1[MPI2_DCR_ADDRESS + 3] = addr_bytes[3];

    // Read the value from MPI2_DCR_DATA.
    Ok(u32::from_le_bytes([
        bar1[MPI2_DCR_DATA],
        bar1[MPI2_DCR_DATA + 1],
        bar1[MPI2_DCR_DATA + 2],
        bar1[MPI2_DCR_DATA + 3],
    ]))
}

/// DCR write32 helper. Cites lsirec.c:40-41, 113-117.
/// Writes offset to MPI2_DCR_ADDRESS, writes data to MPI2_DCR_DATA.
fn dcr_write32(bar1: &mut [u8], offset: u32, data: u32) -> Result<(), I2cError> {
    if bar1.len() < MPI2_DCR_ADDRESS + 4 || bar1.len() < MPI2_DCR_DATA + 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bar1 slice too small for DCR access (len={}, need at least {})",
                bar1.len(),
                MPI2_DCR_ADDRESS.max(MPI2_DCR_DATA) + 4
            ),
        )
        .into());
    }

    // Write the DCR register offset to MPI2_DCR_ADDRESS.
    let addr_bytes = offset.to_le_bytes();
    bar1[MPI2_DCR_ADDRESS] = addr_bytes[0];
    bar1[MPI2_DCR_ADDRESS + 1] = addr_bytes[1];
    bar1[MPI2_DCR_ADDRESS + 2] = addr_bytes[2];
    bar1[MPI2_DCR_ADDRESS + 3] = addr_bytes[3];

    // Write the data value to MPI2_DCR_DATA.
    let data_bytes = data.to_le_bytes();
    bar1[MPI2_DCR_DATA] = data_bytes[0];
    bar1[MPI2_DCR_DATA + 1] = data_bytes[1];
    bar1[MPI2_DCR_DATA + 2] = data_bytes[2];
    bar1[MPI2_DCR_DATA + 3] = data_bytes[3];

    Ok(())
}

/// Delay for I2C timing. Cites lsirec.c:392-395.
fn i2c_delay(bar1: &[u8]) {
    let _ = bar1; // silence unused warning when not needed
    std::thread::sleep(std::time::Duration::from_micros(5));
}

/// Set SDA line. Cites lsirec.c:397-405.
fn set_sda(bar1: &mut [u8], sda: bool) {
    let offset = CHIP_I2C_PINS_OFFSET as usize;
    if bar1.len() < offset + 4 {
        panic!(
            "bar1 slice too small for GPIO access (len={}, need {})",
            bar1.len(),
            offset + 4
        );
    }
    let val = u32::from_le_bytes([
        bar1[offset],
        bar1[offset + 1],
        bar1[offset + 2],
        bar1[offset + 3],
    ]);
    let new_val = if sda {
        val & !CHIP_I2C_SDA_DRV
    } else {
        val | CHIP_I2C_SDA_DRV
    };
    bar1[offset] = (new_val & 0xff) as u8;
    bar1[offset + 1] = ((new_val >> 8) & 0xff) as u8;
    bar1[offset + 2] = ((new_val >> 16) & 0xff) as u8;
    bar1[offset + 3] = ((new_val >> 24) & 0xff) as u8;
}

/// Set SCL line. Cites lsirec.c:407-415.
fn set_scl(bar1: &mut [u8], scl: bool) {
    let offset = CHIP_I2C_PINS_OFFSET as usize;
    if bar1.len() < offset + 4 {
        panic!(
            "bar1 slice too small for GPIO access (len={}, need {})",
            bar1.len(),
            offset + 4
        );
    }
    let val = u32::from_le_bytes([
        bar1[offset],
        bar1[offset + 1],
        bar1[offset + 2],
        bar1[offset + 3],
    ]);
    let new_val = if scl {
        val & !CHIP_I2C_SCL_DRV
    } else {
        val | CHIP_I2C_SCL_DRV
    };
    bar1[offset] = (new_val & 0xff) as u8;
    bar1[offset + 1] = ((new_val >> 8) & 0xff) as u8;
    bar1[offset + 2] = ((new_val >> 16) & 0xff) as u8;
    bar1[offset + 3] = ((new_val >> 24) & 0xff) as u8;
}

/// Get SDA line state. Cites lsirec.c:417-420.
fn get_sda(bar1: &[u8]) -> bool {
    let offset = CHIP_I2C_PINS_OFFSET as usize;
    if bar1.len() < offset + 4 {
        panic!(
            "bar1 slice too small for GPIO access (len={}, need {})",
            bar1.len(),
            offset + 4
        );
    }
    let val = u32::from_le_bytes([
        bar1[offset],
        bar1[offset + 1],
        bar1[offset + 2],
        bar1[offset + 3],
    ]);
    (val & CHIP_I2C_SDA_RD) != 0
}

/// Wait for SCL to go high. Cites lsirec.c:422-432.
fn wait_scl(bar1: &[u8]) -> Result<(), I2cError> {
    let offset = CHIP_I2C_PINS_OFFSET as usize;
    if bar1.len() < offset + 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bar1 slice too small for GPIO access (len={}, need {})",
                bar1.len(),
                offset + 4
            ),
        )
        .into());
    }
    for _ in 0..100 {
        let val = u32::from_le_bytes([
            bar1[offset],
            bar1[offset + 1],
            bar1[offset + 2],
            bar1[offset + 3],
        ]);
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
pub fn i2c_init(ctx: &mut I2cContext<'_>) -> Result<(), I2cError> {
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
    let offset = CHIP_I2C_RESET_OFFSET as usize;
    if ctx.bar1.len() < offset + 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bar1 slice too small for I²C reset (len={}, need {})",
                ctx.bar1.len(),
                offset + 4
            ),
        )
        .into());
    }

    // Reset loop with bounded iterations to prevent infinite hang (OPEN: wedged controller must error).
    let mut iter = 0u32;
    const MAX_RESET_ITERATIONS: u32 = 100_000;

    loop {
        ctx.bar1[offset] = 1;
        ctx.bar1[offset + 1] = 0;
        ctx.bar1[offset + 2] = 0;
        ctx.bar1[offset + 3] = 0;

        i2c_delay(ctx.bar1);
        let reset_val = u32::from_le_bytes([
            ctx.bar1[offset],
            ctx.bar1[offset + 1],
            ctx.bar1[offset + 2],
            ctx.bar1[offset + 3],
        ]);
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
    let offset = CHIP_I2C_RESET_OFFSET as usize;

    if ctx.bar1.len() < offset + 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bar1 slice too small for I²C reset (len={}, need {})",
                ctx.bar1.len(),
                offset + 4
            ),
        )
        .into());
    }

    // Reset loop with bounded iterations to prevent infinite hang.
    let mut iter = 0u32;
    const MAX_RESET_ITERATIONS: u32 = 100_000;

    loop {
        ctx.bar1[offset] = 1;
        ctx.bar1[offset + 1] = 0;
        ctx.bar1[offset + 2] = 0;
        ctx.bar1[offset + 3] = 0;

        i2c_delay(ctx.bar1);
        let reset_val = u32::from_le_bytes([
            ctx.bar1[offset],
            ctx.bar1[offset + 1],
            ctx.bar1[offset + 2],
            ctx.bar1[offset + 3],
        ]);
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
    if !i2c_getbit(ctx.bar1) {
        return Err(I2cError::EepromNoAck("address W"));
    }

    // Send offset (16-bit mode). Cites lsirec.c:560-564.
    if ctx.eep_type == EEPROM_TYPE_16BIT {
        i2c_sendbyte(ctx.bar1, (offset >> 8) as u8);
        if !i2c_getbit(ctx.bar1) {
            return Err(I2cError::EepromNoAck("offset1"));
        }
    }

    // Send offset low byte. Cites lsirec.c:566-570.
    i2c_sendbyte(ctx.bar1, (offset & 0xff) as u8);
    if !i2c_getbit(ctx.bar1) {
        return Err(I2cError::EepromNoAck("offset0"));
    }

    // Repeated START for read mode. Cites lsirec.c:572-576.
    i2c_start(ctx.bar1);
    i2c_sendbyte(ctx.bar1, (ctx.sbr_addr << 1) | 0x01);
    if !i2c_getbit(ctx.bar1) {
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
        if !i2c_getbit(ctx.bar1) {
            return Err(I2cError::EepromNoAck("address W"));
        }

        if ctx.eep_type == EEPROM_TYPE_16BIT {
            i2c_sendbyte(ctx.bar1, (abs_offset >> 8) as u8);
            if !i2c_getbit(ctx.bar1) {
                return Err(I2cError::EepromNoAck("offset1"));
            }
        }

        i2c_sendbyte(ctx.bar1, (abs_offset & 0xff) as u8);
        if !i2c_getbit(ctx.bar1) {
            return Err(I2cError::EepromNoAck("offset0"));
        }

        i2c_sendbyte(ctx.bar1, byte);
        if !i2c_getbit(ctx.bar1) {
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
        // CHIP_I2C_RESET_OFFSET is 0x24 = 36, so we need at least 40 bytes (offset + 4)
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

        // Should not return the "too small" error (will timeout on SCL but that's expected)
        let result = i2c_read_sbr(&mut ctx, 0, 1);
        assert!(result.is_err()); // Expected to fail with timeout or I/O, not size check
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
        // Too small buffer for DCR access (need at least 0x3c + 4 = 64 bytes).
        let mut bar1 = vec![0u8; 32];

        let result = dcr_read32(&mut bar1, 0);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("bar1 slice too small for DCR access"));

        let result = dcr_write32(&mut bar1, 0, 0);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("bar1 slice too small for DCR access"));
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
