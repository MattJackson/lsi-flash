//! I2C bit-bang module for EEPROM access.
//! Port of lsirec.c:392-629 (i2c_delay through lsi_i2c_write_sbr).
//! Cites: references/upstream/lsirec-marcan/lsirec.c:56-57 (DCR_I2C_SELECT, DCR_SBR_CONFIG)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:59-65 (CHIP_I2C_BASE/PINS/RESET offsets and bit masks)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:67-68 (EEPROM_TYPE_16BIT, EEPROM_TYPE_8BIT)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:392-496 (bit-bang primitives)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:498-549 (i2c_init/close)
//! Cites: references/upstream/lsirec-marcan/lsirec.c:551-629 (SBR read/write)

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

/// I2C context for bit-bang operations.
#[derive(Debug)]
pub struct I2cContext {
    pub bar1: Box<[u8; 4096]>, // Reuse from pci.rs
    pub sbr_addr: u8,          // 0x50 or 0x54 (lsirec.c:502-507)
    pub eep_type: u8,          // EEPROM_TYPE_8BIT or EEPROM_TYPE_16BIT (lsirec.c:509-513)
}

/// Delay for I2C timing. Cites lsirec.c:392-395.
fn i2c_delay(bar1: &[u8; 4096]) {
    let _ = bar1; // silence unused warning when not needed
    std::thread::sleep(std::time::Duration::from_micros(5));
}

/// Set SDA line. Cites lsirec.c:397-405.
fn set_sda(bar1: &mut [u8; 4096], sda: bool) {
    let offset = CHIP_I2C_PINS_OFFSET as usize;
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
fn set_scl(bar1: &mut [u8; 4096], scl: bool) {
    let offset = CHIP_I2C_PINS_OFFSET as usize;
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
fn get_sda(bar1: &[u8; 4096]) -> bool {
    let offset = CHIP_I2C_PINS_OFFSET as usize;
    let val = u32::from_le_bytes([
        bar1[offset],
        bar1[offset + 1],
        bar1[offset + 2],
        bar1[offset + 3],
    ]);
    (val & CHIP_I2C_SDA_RD) != 0
}

/// Wait for SCL to go high. Cites lsirec.c:422-432.
fn wait_scl(bar1: &[u8; 4096]) -> Result<(), I2cError> {
    for _ in 0..100 {
        let offset = CHIP_I2C_PINS_OFFSET as usize;
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
fn i2c_start(bar1: &mut [u8; 4096]) {
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
fn i2c_stop(bar1: &mut [u8; 4096]) {
    i2c_delay(bar1);
    set_sda(bar1, false);
    i2c_delay(bar1);
    set_scl(bar1, true);
    i2c_delay(bar1);
    set_sda(bar1, true);
    i2c_delay(bar1);
}

/// Send a single bit. Cites lsirec.c:459-468.
fn i2c_sendbit(bar1: &mut [u8; 4096], bit: bool) {
    set_sda(bar1, bit);
    i2c_delay(bar1);
    set_scl(bar1, true);
    let _ = wait_scl(bar1);
    i2c_delay(bar1);
    set_scl(bar1, false);
    i2c_delay(bar1);
}

/// Receive a single bit. Cites lsirec.c:470-481.
fn i2c_getbit(bar1: &mut [u8; 4096]) -> bool {
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
fn i2c_sendbyte(bar1: &mut [u8; 4096], byte: u8) {
    for mask in [0x80u8, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01] {
        i2c_sendbit(bar1, byte & mask != 0);
    }
}

/// Receive a byte (MSB first). Cites lsirec.c:489-496.
fn i2c_getbyte(bar1: &mut [u8; 4096]) -> u8 {
    let mut val = 0u8;
    for mask in [0x80u8, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01] {
        if i2c_getbit(bar1) {
            val |= mask;
        }
    }
    val
}

/// Initialize I2C interface. Cites lsirec.c:498-533.
pub fn i2c_init(
    ctx: &mut I2cContext,
    dcr_read32_fn: impl Fn(u32) -> u32,
    dcr_write32_fn: impl Fn(u32, u32),
) {
    // Read DCR_SBR_CONFIG to determine addr and type (lsirec.c:501-514).
    let val = dcr_read32_fn(DCR_SBR_CONFIG);

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
    ctx.bar1[offset] = 1;
    ctx.bar1[offset + 1] = 0;
    ctx.bar1[offset + 2] = 0;
    ctx.bar1[offset + 3] = 0;
    loop {
        i2c_delay(&ctx.bar1);
        let reset_val = u32::from_le_bytes([
            ctx.bar1[offset],
            ctx.bar1[offset + 1],
            ctx.bar1[offset + 2],
            ctx.bar1[offset + 3],
        ]);
        if reset_val & 1 == 0 {
            break;
        }
    }

    // Enable I2C select in DCR (lsirec.c:521-524).
    let val = dcr_read32_fn(DCR_I2C_SELECT);
    dcr_write32_fn(DCR_I2C_SELECT, val | 0x800000);

    // Reset I2C lines (lsirec.c:526-531).
    for _ in 0..9 {
        i2c_sendbit(&mut ctx.bar1, true);
    }
    i2c_stop(&mut ctx.bar1);
    i2c_start(&mut ctx.bar1);
    i2c_stop(&mut ctx.bar1);
}

/// Close I2C interface. Cites lsirec.c:535-549.
pub fn i2c_close(
    ctx: &mut I2cContext,
    dcr_read32_fn: impl Fn(u32) -> u32,
    dcr_write32_fn: impl Fn(u32, u32),
) {
    let offset = CHIP_I2C_RESET_OFFSET as usize;
    ctx.bar1[offset] = 1;
    ctx.bar1[offset + 1] = 0;
    ctx.bar1[offset + 2] = 0;
    ctx.bar1[offset + 3] = 0;
    loop {
        i2c_delay(&ctx.bar1);
        let reset_val = u32::from_le_bytes([
            ctx.bar1[offset],
            ctx.bar1[offset + 1],
            ctx.bar1[offset + 2],
            ctx.bar1[offset + 3],
        ]);
        if reset_val & 1 == 0 {
            break;
        }
    }

    let val = dcr_read32_fn(DCR_I2C_SELECT);
    dcr_write32_fn(DCR_I2C_SELECT, val & !0x800000);
}

/// Read SBR from EEPROM. Cites lsirec.c:551-589.
pub fn i2c_read_sbr(ctx: &mut I2cContext, offset: usize, len: usize) -> Result<Vec<u8>, I2cError> {
    let mut buf = vec![0u8; len];

    // Send address byte (write mode). Cites lsirec.c:553-569.
    i2c_start(&mut ctx.bar1);
    i2c_sendbyte(&mut ctx.bar1, ctx.sbr_addr << 1);
    if !i2c_getbit(&mut ctx.bar1) {
        return Err(I2cError::EepromNoAck("address W"));
    }

    // Send offset (16-bit mode). Cites lsirec.c:560-564.
    if ctx.eep_type == EEPROM_TYPE_16BIT {
        i2c_sendbyte(&mut ctx.bar1, (offset >> 8) as u8);
        if !i2c_getbit(&mut ctx.bar1) {
            return Err(I2cError::EepromNoAck("offset1"));
        }
    }

    // Send offset low byte. Cites lsirec.c:566-570.
    i2c_sendbyte(&mut ctx.bar1, (offset & 0xff) as u8);
    if !i2c_getbit(&mut ctx.bar1) {
        return Err(I2cError::EepromNoAck("offset0"));
    }

    // Repeated START for read mode. Cites lsirec.c:572-576.
    i2c_start(&mut ctx.bar1);
    i2c_sendbyte(&mut ctx.bar1, (ctx.sbr_addr << 1) | 0x01);
    if !i2c_getbit(&mut ctx.bar1) {
        return Err(I2cError::EepromNoAck("address R"));
    }

    // Read bytes. Cites lsirec.c:578-586.
    for (i, slot) in buf.iter_mut().enumerate().take(len) {
        *slot = i2c_getbyte(&mut ctx.bar1);
        i2c_sendbit(&mut ctx.bar1, i == len - 1); // NACK on last byte
    }

    i2c_stop(&mut ctx.bar1);
    Ok(buf)
}

/// Write SBR to EEPROM. Cites lsirec.c:591-629.
pub fn i2c_write_sbr(ctx: &mut I2cContext, offset: usize, data: &[u8]) -> Result<(), I2cError> {
    for (i, &byte) in data.iter().enumerate() {
        let abs_offset = offset + i;

        i2c_start(&mut ctx.bar1);
        i2c_sendbyte(&mut ctx.bar1, ctx.sbr_addr << 1);
        if !i2c_getbit(&mut ctx.bar1) {
            return Err(I2cError::EepromNoAck("address W"));
        }

        if ctx.eep_type == EEPROM_TYPE_16BIT {
            i2c_sendbyte(&mut ctx.bar1, (abs_offset >> 8) as u8);
            if !i2c_getbit(&mut ctx.bar1) {
                return Err(I2cError::EepromNoAck("offset1"));
            }
        }

        i2c_sendbyte(&mut ctx.bar1, (abs_offset & 0xff) as u8);
        if !i2c_getbit(&mut ctx.bar1) {
            return Err(I2cError::EepromNoAck("offset0"));
        }

        i2c_sendbyte(&mut ctx.bar1, byte);
        if !i2c_getbit(&mut ctx.bar1) {
            return Err(I2cError::EepromNoAck("data"));
        }

        i2c_stop(&mut ctx.bar1);
        std::thread::sleep(std::time::Duration::from_millis(5)); // 5ms write cycle (lsirec.c:625)
    }

    Ok(())
}
