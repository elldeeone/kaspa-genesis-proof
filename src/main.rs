use clap::Parser;
use rust_native_verifier::{Cli, run_cli};

fn main() {
    std::process::exit(run_cli(Cli::parse()));
}
