//! MPI 2.0 register access layer for lsi-flash.
//! Port of lsirec.c:89-133 (register accessors, WRSEQ unlock).

pub mod diag;
pub mod doorbell;
pub use doorbell::*;
