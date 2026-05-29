//! SBR (Subsystem Boot Record) module for lsi-flash.
//! Port of sbrtool.py: parse/build/checksum operations.

mod build;
pub mod i2c;
pub mod parse;
pub mod transport;

pub use i2c::*;
pub use parse::*;
pub use transport::{
    Bar1MmapSbrTransport, IstwiSbrTransport, SbrTransport, SbrTransportError, VfioI2cSbrTransport,
};
