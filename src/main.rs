mod error;

pub mod firmware;
pub mod hcb;
pub mod mpi;
pub mod sbr;

pub use error::Error;
pub use firmware::*;

fn main() {
    eprintln!("lsi-flash: not yet implemented. See ROADMAP.md.");
    std::process::exit(1);
}
