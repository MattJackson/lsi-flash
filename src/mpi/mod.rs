//! MPI 2.0 register access layer for lsi-flash.
//! Port of lsirec.c:89-133 (register accessors, WRSEQ unlock).

pub mod diag;
pub mod doorbell;
pub mod messages;
pub mod mmap_region;
pub mod mock_ioc;
pub mod real_ioc;
pub mod session;

pub use doorbell::*;
pub use messages::*;
