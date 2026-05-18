mod card_database;
mod cli;
mod error;

pub mod firmware;
pub mod hcb;
pub mod mpi;
pub mod sbr;

use clap::Parser;
pub use cli::{Cli, run};
pub use error::Error;
pub use firmware::*;

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
