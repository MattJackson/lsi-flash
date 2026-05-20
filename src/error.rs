/// Top-level error type for lsi-flash. Cites scoping doc §1.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("MPI register error: {0}")]
    MpiRegister(#[from] crate::mpi::doorbell::MpiRegisterError),

    #[error("MPI DIAG error: {0}")]
    MpiDiag(#[from] crate::mpi::diag::MpiError),

    /// MPI message serialization/deserialization error.
    #[error("MPI message error: {0}")]
    MpiMessage(#[from] crate::mpi::messages::MpiError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("firmware synthesis error: {0}")]
    Synth(#[from] crate::firmware::synthesize::SynthError),

    #[error("backup error: {0}")]
    Backup(#[from] crate::cli::backup::BackupError),

    #[error("recover error: {0}")]
    Recover(#[from] crate::cli::recover::RecoverError),

    #[error("serde_json: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("toml ser: {0}")]
    TomlSer(#[from] toml::ser::Error),

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
