mod support;

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use support::{create_go_fixture, run_binary, stdout_text};

#[test]
#[ignore = "deep-ci: runs full binary verification flow"]
fn binary_writes_json_report_for_go_fixture() {
    let tempdir = TempDir::new().expect("tempdir");
    let fixture = create_go_fixture(&tempdir, false);
    let report_path = tempdir.path().join("report.json");
    let datadir = fixture.root.to_string_lossy().into_owned();
    let report_arg = report_path.to_string_lossy().into_owned();
    let checkpoint_utxos = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("kaspad-v0.11.5-2-utxos.gz");
    let checkpoint_utxos_arg = checkpoint_utxos.to_string_lossy().into_owned();

    let output = run_binary(
        &[
            "--node-type",
            "go",
            "--datadir",
            &datadir,
            "--checkpoint-utxos-gz",
            &checkpoint_utxos_arg,
            "--no-input",
            "--json-out",
            &report_arg,
        ],
        tempdir.path(),
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout_text(&output),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = stdout_text(&output);
    assert!(stdout.contains("The Kaspa blockchain integrity has been verified"));
    assert!(stdout.contains("Trust model: embedded pre-checkpoint data by default"));
    assert!(stdout.contains("Trust model: operator-supplied checkpoint UTXO dump"));
    assert!(stdout.contains("Using operator-supplied checkpoint utxos.gz:"));
    assert!(
        stdout.contains("Checkpoint dump MuHash matches the checkpoint header UTXO commitment")
    );
    assert!(stdout.contains("Verified checkpoint total: 984,222,544.04487171 KAS"));
    assert!(stdout.contains("JSON report written to"));

    let report: Value = serde_json::from_slice(&fs::read(&report_path).expect("read report json"))
        .expect("parse report json");

    assert_eq!(report["success"], Value::Bool(true));
    assert_eq!(
        report["requested_node_type"],
        Value::String("go".to_string())
    );
    assert_eq!(
        report["store_type"],
        Value::String("Go node store (LevelDB + Protobuf)".to_string())
    );
    assert_eq!(report["tips_count"], Value::from(1));
    assert_eq!(
        report["resolved_db_path"],
        Value::String(fixture.expected_db_path.display().to_string())
    );
    assert_eq!(
        report["checkpoint_utxos_gz"],
        Value::String(checkpoint_utxos.display().to_string())
    );
    assert_eq!(report["checkpoint_utxo_dump_verified"], Value::Bool(true));
    assert_eq!(
        report["checkpoint_total_sompi"],
        Value::String("98422254404487171".to_string())
    );
    assert_eq!(
        report["checkpoint_total_kas"],
        Value::String("984,222,544.04487171".to_string())
    );
    assert_eq!(
        report["chain_tip_used"],
        Value::String(hex::encode(fixture.tip_hash))
    );
    assert!(
        report["screen_output_lines"]
            .as_array()
            .expect("screen output lines array")
            .len()
            > 10
    );
}

#[test]
#[ignore = "deep-ci: runs full binary verification flow"]
fn binary_no_input_skips_prompts_and_does_not_auto_export_json() {
    let tempdir = TempDir::new().expect("tempdir");
    let fixture = create_go_fixture(&tempdir, true);
    let datadir = fixture.root.to_string_lossy().into_owned();

    let output = run_binary(
        &["--node-type", "go", "--datadir", &datadir, "--no-input"],
        tempdir.path(),
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout_text(&output),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = stdout_text(&output);
    assert!(
        stdout
            .contains("Sync advisory prompt skipped due to --no-input; continuing automatically.")
    );
    assert!(stdout.contains("Trust model: embedded pre-checkpoint data by default"));
    assert!(stdout.contains("Trust model: embedded checkpoint dump by default"));
    assert!(!stdout.contains("Do you want to export this verification to JSON?"));
    assert!(!stdout.contains("JSON report written to"));

    let json_reports = fs::read_dir(tempdir.path())
        .expect("list tempdir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("kaspa-proof-report-") && name.ends_with(".json")
                })
        })
        .collect::<Vec<_>>();

    assert!(
        json_reports.is_empty(),
        "unexpected json exports: {json_reports:?}"
    );
}
