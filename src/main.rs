use clap::Parser;
use genesis_proof::{Cli, run_cli};

fn main() {
    std::process::exit(run_cli(Cli::parse()));
}
