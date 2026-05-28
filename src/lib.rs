//! liblsi — a Rust library for operating on LSI / Avago / Broadcom
//! Fusion-MPT SAS HBAs (SAS2008, SAS2208, SAS3008, SAS3408, SAS3508).
//!
//! Read SBR, back up firmware, detect cards, (eventually) flash — all via
//! the `Card` trait with pluggable transports underneath (kernel mpt3sas
//! ioctl, VFIO+doorbell). The `lsi-flash` binary is one consumer of this
//! library. See ADR-017 (Card + transport) and ADR-018 (this extraction).

pub mod card;
pub mod card_database;
pub mod cli;
pub mod error;
pub mod firmware;
pub mod hcb;
pub mod hw;
pub mod mpi;
pub mod mpt;
pub mod pci;
pub mod sbr;

// Curated top-level re-exports for the common consumer entry points.
pub use card::{discover, discover_one, Card, CardError, CardIdentity, ChipFamily};
pub use error::Error;
// Keep CLI exports available to the bin (and any consumer embedding the CLI).
pub use cli::{run, Cli};

/// Library API reachability test — proves public re-exports resolve.
#[test]
fn test_api_reachable() {
    // Verify Card trait is accessible as a type
    let _card_ref: Option<Box<dyn card::Card>> = None;

    // Verify discover function signature resolves
    let _discover_fn = crate::discover;
    let _discover_one_fn = crate::discover_one;

    // Verify Error enum and CardError enum are accessible
    let _error: Result<(), error::Error> = Err(error::Error::Io(std::io::Error::other("test")));
    let _card_error: Result<(), card::CardError> = Err(card::CardError::NoCardsFound);
}
