//! `lsi-flash` — command-line consumer of the `liblsi` library.

use clap::Parser;
use liblsi::{run, Cli};

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
