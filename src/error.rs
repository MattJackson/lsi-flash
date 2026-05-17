/// Top-level error type for lsi-flash. Cites scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("PCI error: {0}")]
    Pci(#[from] pci::PciError),

    #[error("MPI register error: {0}")]
    MpiRegister(#[from] mpi::doorbell::MpiRegisterError),

    #[error("MPI DIAG error: {0}")]
    MpiDiag(#[from] mpi::diag::MpiError),

    #[error("HCB error: {0}")]
    Hcb(#[from] hcb::HcbError),

    #[error("SBR error: {0}")]
    Sbr(#[from] sbr::parse::SbrError),

    #[error("I2C error: {0}")]
    I2c(#[from] sbr::i2c::I2cError),

    #[error("Firmware error: {0}")]
    Firmware(#[from] firmware::inspect::FwError),

    #[error("CLI error: {0}")]
    Cli(#[from] cli::CliError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Unexpected error: {0}")]
    Other(String),
}

/// PCI error type (defined in pci.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum PciError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse hex value error: {0}")]
    ParseHex(#[from] std::num::ParseIntError),

    #[error("BAR1 mmap failed: {0}")]
    Mmap(String),

    #[error("PCI device {bdf} not found", bdf = .bdf)]
    DeviceNotFound { bdf: String },
}

/// MPI register error type (defined in mpi/doorbell.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum MpiRegisterError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid BAR1 mapping size (expected 4096 bytes)")]
    InvalidBarSize,
}

/// MPI DIAG error type (defined in mpi/diag.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
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

/// HCB error type (defined in hcb.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum HcbError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Hugepage allocation failed: {0}")]
    HugepageAllocationFailed(std::io::Error),

    #[error("Insufficient hugepages (need 16, have {available})")]
    InsufficientHugepages { available: u64 },

    #[error("IOC did not become ready after boot (doorbell=0x{doorbell:08x})")]
    IocNotReadyAfterBoot { doorbell: u32 },
}

/// SBR error type (defined in sbr/parse.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum SbrError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SBR too short (got {0} bytes, need 256)")]
    TooShort(usize),

    #[error("MFG block too short (got {0} bytes, need 75)")]
    MfgTooShort(usize),
}

/// I2C error type (defined in sbr/i2c.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum I2cError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SCL timeout waiting for high")]
    SclTimeout,

    #[error("EEPROM did not ACK {0}")]
    EepromNoAck(&'static str),
}

/// Firmware error type (defined in firmware/inspect.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum FwError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Firmware too short (got {0} bytes, need at least 32)")]
    TooShort(usize),

    #[error("Invalid firmware signature (expected 0x{:08x}, got 0x{:08x})", .expected, .got)]
    InvalidSignature { expected: u32, got: u32 },
}

/// CLI error type (defined in cli/mod.rs). Cites thiserror usage from scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum CliError {
    #[error("Command not implemented: {0}")]
    NotImplemented(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
