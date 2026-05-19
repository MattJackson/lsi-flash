//! Firmware inspection module for lsi-flash.
//! Port of MPI Fusion-MPT firmware header parsing from mpi2_ioc.h.
//!
//! Cites: references/upstream/lsiutil/lsi/mpi2_ioc.h (lines 1314-1362, 1365-1409)

pub mod inspect;
pub mod synthesize;
pub use inspect::*;
pub use synthesize::*;
