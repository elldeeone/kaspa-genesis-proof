use super::Cli;
use std::path::PathBuf;

#[test]
fn cli_accepts_json_out_flag() {
    let cli = <Cli as clap::Parser>::try_parse_from([
        "genesis-proof",
        "--json-out",
        "report.json",
        "--no-input",
    ])
    .expect("parse cli");

    assert_eq!(cli.json_out, Some(PathBuf::from("report.json")));
    assert!(cli.no_input);
}

#[test]
fn cli_accepts_pre_checkpoint_datadir_flag() {
    let cli = <Cli as clap::Parser>::try_parse_from([
        "genesis-proof",
        "--pre-checkpoint-datadir",
        "/tmp/pre-checkpoint",
    ])
    .expect("parse cli");

    assert_eq!(
        cli.pre_checkpoint_datadir,
        Some(PathBuf::from("/tmp/pre-checkpoint"))
    );
}

#[test]
fn cli_accepts_checkpoint_utxos_gz_flag() {
    let cli = <Cli as clap::Parser>::try_parse_from([
        "genesis-proof",
        "--checkpoint-utxos-gz",
        "/tmp/utxos.gz",
    ])
    .expect("parse cli");

    assert_eq!(
        cli.checkpoint_utxos_gz,
        Some(PathBuf::from("/tmp/utxos.gz"))
    );
}
