use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "rust-native-verifier",
    about = "Verify Kaspa chain integrity from the current node state back to genesis",
    long_about = "Rust-native Kaspa genesis proof verifier. Verifies cryptographic linkage from the current node state back to genesis for both rusty-kaspa (RocksDB) and legacy kaspad (LevelDB), including the hardwired checkpoint/original-genesis proof chain.",
    after_help = "Examples:\n  rust-native-verifier\n  rust-native-verifier --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir\n  rust-native-verifier --checkpoint-utxos-gz ./utxos.gz\n  rust-native-verifier --no-input --json-out ./kaspa-proof-report.json"
)]
pub struct Cli {
    #[arg(
        long,
        value_enum,
        default_value_t = CliNodeType::Auto,
        help = "Node layout to use (auto detects Rust/Go by default)"
    )]
    pub node_type: CliNodeType,

    #[arg(
        long,
        help = "Path to Kaspa data directory. If omitted, KASPA_DATADIR and default OS paths are probed automatically"
    )]
    pub datadir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Optional pre-checkpoint Go datadir to reproduce the notebook's external checkpoint/original-genesis verification path"
    )]
    pub pre_checkpoint_datadir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Optional path to a manually downloaded kaspad v0.11.5-2 resources/utxos.gz file; if omitted, the verifier uses its embedded canonical copy"
    )]
    pub checkpoint_utxos_gz: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Write a JSON verification report to this path without prompting (parent directories are created as needed)"
    )]
    pub json_out: Option<PathBuf>,

    #[arg(long, short = 'v', help = "Enable verbose chain-walk output")]
    pub verbose: bool,

    #[arg(
        long,
        help = "Disable interactive prompts and continue automatically when sync advisory is triggered"
    )]
    pub no_input: bool,

    #[arg(
        long,
        help = "Wait for Enter before exiting (useful for double-click launches)"
    )]
    pub pause_on_exit: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliNodeType {
    Auto,
    Rust,
    Go,
}

#[cfg(test)]
mod tests;
