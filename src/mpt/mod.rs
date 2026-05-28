//! Fusion-MPT (Message Passing Technology) chip family — SAS2008, SAS2208,
//! SAS3008, SAS3408, SAS3508. The `MptTransport` trait abstracts how MPI
//! message bytes flow to/from the chip; concrete impls plug in different
//! transport mechanisms (kernel `mpt3sas` ioctl, our own VFIO+doorbell,
//! future user-space post-queue mode, etc.).
//!
//! See ADR-017 (lsi-flash-notes) for the pluggable-transport design rationale.

pub mod mpt3ctl;
pub mod transport;

pub use mpt3ctl::Mpt3CtlTransport;
pub use transport::{MptTransport, TransportError};
