//! SBR (Subsystem Boot Record) module for lsi-flash.
//! Port of sbrtool.py: parse/build/checksum operations.

pub mod i2c;
pub mod parse;

pub use i2c::*;
pub use parse::*;
