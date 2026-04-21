use blake2b_simd::Params;
use prost::Message;
use rusty_leveldb::{DB, Options};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

type Hash32 = [u8; 32];

const HARDWIRED_GENESIS_HASH_HEX: &str =
    "58c2d4199e21f910d1571d114969cecef48f09f934d42ccb6a281a15868f2999";
const HARDWIRED_GENESIS_MERKLE_ROOT_HEX: &str =
    "8ec898568c6801d13df4ee6e2a1b54b7e6236f671f20954f05306410518eeb32";
const HARDWIRED_GENESIS_UTXO_HEX: &str =
    "710f27df423e63aa6cdb72b89ea5a06cffa399d66f167704455b5af59def8e20";
const HARDWIRED_GENESIS_TIMESTAMP_MS: u64 = 1_637_609_671_037;
const TIP_SYNC_WARNING_THRESHOLD_MS: u64 = 10 * 60 * 1000;

#[derive(Clone)]
struct HeaderData {
    version: u16,
    parents: Vec<Vec<Hash32>>,
    hash_merkle_root: Hash32,
    accepted_id_merkle_root: Hash32,
    utxo_commitment: Hash32,
    time_in_milliseconds: u64,
    bits: u32,
    nonce: u64,
    daa_score: u64,
    blue_score: u64,
    blue_work_trimmed_be: Vec<u8>,
    pruning_point: Hash32,
}

struct GoFixture {
    root: PathBuf,
    expected_db_path: PathBuf,
    tip_hash: Hash32,
}

#[derive(Clone, PartialEq, Message)]
struct DbHashMsg {
    #[prost(bytes = "vec", tag = "1")]
    hash: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
struct DbBlockLevelParentsMsg {
    #[prost(message, repeated, tag = "1")]
    parent_hashes: Vec<DbHashMsg>,
}

#[derive(Clone, PartialEq, Message)]
struct DbBlockHeaderMsg {
    #[prost(uint32, tag = "1")]
    version: u32,
    #[prost(message, repeated, tag = "2")]
    parents: Vec<DbBlockLevelParentsMsg>,
    #[prost(message, optional, tag = "3")]
    hash_merkle_root: Option<DbHashMsg>,
    #[prost(message, optional, tag = "4")]
    accepted_id_merkle_root: Option<DbHashMsg>,
    #[prost(message, optional, tag = "5")]
    utxo_commitment: Option<DbHashMsg>,
    #[prost(int64, tag = "6")]
    time_in_milliseconds: i64,
    #[prost(uint32, tag = "7")]
    bits: u32,
    #[prost(uint64, tag = "8")]
    nonce: u64,
    #[prost(uint64, tag = "9")]
    daa_score: u64,
    #[prost(bytes = "vec", tag = "10")]
    blue_work: Vec<u8>,
    #[prost(message, optional, tag = "12")]
    pruning_point: Option<DbHashMsg>,
    #[prost(uint64, tag = "13")]
    blue_score: u64,
}

#[derive(Clone, PartialEq, Message)]
struct DbTipsMsg {
    #[prost(message, repeated, tag = "1")]
    tips: Vec<DbHashMsg>,
}

fn hash32_from_hex(hex_str: &str) -> Hash32 {
    let decoded = hex::decode(hex_str).expect("valid hash hex");
    let mut out = [0u8; 32];
    out.copy_from_slice(&decoded);
    out
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis()
        .try_into()
        .expect("current time fits in u64")
}

fn new_blake2b_32(key: &[u8]) -> blake2b_simd::State {
    let mut params = Params::new();
    params.hash_length(32);
    params.key(key);
    params.to_state()
}

fn header_hash(header: &HeaderData) -> Hash32 {
    let mut hasher = new_blake2b_32(b"BlockHash");
    hasher.update(&header.version.to_le_bytes());
    hasher.update(&(header.parents.len() as u64).to_le_bytes());

    for level_parents in &header.parents {
        hasher.update(&(level_parents.len() as u64).to_le_bytes());
        for parent in level_parents {
            hasher.update(parent);
        }
    }

    hasher.update(&header.hash_merkle_root);
    hasher.update(&header.accepted_id_merkle_root);
    hasher.update(&header.utxo_commitment);
    hasher.update(&header.time_in_milliseconds.to_le_bytes());
    hasher.update(&header.bits.to_le_bytes());
    hasher.update(&header.nonce.to_le_bytes());
    hasher.update(&header.daa_score.to_le_bytes());
    hasher.update(&header.blue_score.to_le_bytes());
    hasher.update(&(header.blue_work_trimmed_be.len() as u64).to_le_bytes());
    hasher.update(&header.blue_work_trimmed_be);
    hasher.update(&header.pruning_point);

    let mut out = [0u8; 32];
    out.copy_from_slice(hasher.finalize().as_bytes());
    out
}

fn encode_db_hash(hash: Hash32) -> Vec<u8> {
    DbHashMsg {
        hash: hash.to_vec(),
    }
    .encode_to_vec()
}

fn encode_db_tips(tips: &[Hash32]) -> Vec<u8> {
    DbTipsMsg {
        tips: tips
            .iter()
            .map(|hash| DbHashMsg {
                hash: hash.to_vec(),
            })
            .collect(),
    }
    .encode_to_vec()
}

fn encode_db_block_header(header: &HeaderData) -> Vec<u8> {
    DbBlockHeaderMsg {
        version: u32::from(header.version),
        parents: header
            .parents
            .iter()
            .map(|level| DbBlockLevelParentsMsg {
                parent_hashes: level
                    .iter()
                    .map(|hash| DbHashMsg {
                        hash: hash.to_vec(),
                    })
                    .collect(),
            })
            .collect(),
        hash_merkle_root: Some(DbHashMsg {
            hash: header.hash_merkle_root.to_vec(),
        }),
        accepted_id_merkle_root: Some(DbHashMsg {
            hash: header.accepted_id_merkle_root.to_vec(),
        }),
        utxo_commitment: Some(DbHashMsg {
            hash: header.utxo_commitment.to_vec(),
        }),
        time_in_milliseconds: i64::try_from(header.time_in_milliseconds)
            .expect("header timestamp fits in i64"),
        bits: header.bits,
        nonce: header.nonce,
        daa_score: header.daa_score,
        blue_work: header.blue_work_trimmed_be.clone(),
        pruning_point: Some(DbHashMsg {
            hash: header.pruning_point.to_vec(),
        }),
        blue_score: header.blue_score,
    }
    .encode_to_vec()
}

fn go_bucketed_key(active_prefix: u8, bucket: &[u8], suffix: Option<&[u8]>) -> Vec<u8> {
    let mut key = Vec::with_capacity(2 + bucket.len() + suffix.map(|s| 1 + s.len()).unwrap_or(0));
    key.push(active_prefix);
    key.push(b'/');
    key.extend_from_slice(bucket);
    if let Some(suffix) = suffix {
        key.push(b'/');
        key.extend_from_slice(suffix);
    }
    key
}

fn hardwired_genesis_header() -> HeaderData {
    HeaderData {
        version: 0,
        parents: Vec::new(),
        hash_merkle_root: hash32_from_hex(HARDWIRED_GENESIS_MERKLE_ROOT_HEX),
        accepted_id_merkle_root: [0u8; 32],
        utxo_commitment: hash32_from_hex(HARDWIRED_GENESIS_UTXO_HEX),
        time_in_milliseconds: HARDWIRED_GENESIS_TIMESTAMP_MS,
        bits: 486_722_099,
        nonce: 211_244,
        daa_score: 1_312_860,
        blue_score: 0,
        blue_work_trimmed_be: Vec::new(),
        pruning_point: [0u8; 32],
    }
}

fn synthetic_tip_header(pruning_point: Hash32, time_in_milliseconds: u64) -> HeaderData {
    HeaderData {
        version: 1,
        parents: vec![vec![hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX)]],
        hash_merkle_root: [0x22; 32],
        accepted_id_merkle_root: [0x33; 32],
        utxo_commitment: [0x44; 32],
        time_in_milliseconds,
        bits: 0x1d00ffff,
        nonce: 42,
        daa_score: 7,
        blue_score: 8,
        blue_work_trimmed_be: vec![0x01, 0x02, 0x03],
        pruning_point,
    }
}

fn create_go_fixture(tempdir: &TempDir, stale_tip: bool) -> GoFixture {
    let root = tempdir.path().join("go-node");
    let db_path = root.join("kaspa-mainnet").join("datadir2");
    fs::create_dir_all(&db_path).expect("create go fixture db dir");

    let mut opts = Options::default();
    opts.create_if_missing = true;
    let mut db = DB::open(&db_path, opts).expect("open go fixture leveldb");

    let active_prefix = 1u8;
    let tip_time = if stale_tip {
        now_millis().saturating_sub(TIP_SYNC_WARNING_THRESHOLD_MS + 1_000)
    } else {
        now_millis()
    };

    let genesis_hash = hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX);
    let genesis_header = hardwired_genesis_header();
    let tip_header = synthetic_tip_header(genesis_hash, tip_time);
    let tip_hash = header_hash(&tip_header);

    let entries = vec![
        (b"active-prefix".to_vec(), vec![active_prefix]),
        (
            go_bucketed_key(active_prefix, b"headers-selected-tip", None),
            encode_db_hash(tip_hash),
        ),
        (
            go_bucketed_key(active_prefix, b"tips", None),
            encode_db_tips(&[tip_hash]),
        ),
        (
            go_bucketed_key(active_prefix, b"block-headers", Some(&tip_hash)),
            encode_db_block_header(&tip_header),
        ),
        (
            go_bucketed_key(active_prefix, b"block-headers", Some(&genesis_hash)),
            encode_db_block_header(&genesis_header),
        ),
    ];

    for (key, value) in entries {
        db.put(&key, &value).expect("write go fixture entry");
    }
    db.flush().expect("flush go fixture db");
    db.close().expect("close go fixture db");

    GoFixture {
        root,
        expected_db_path: db_path,
        tip_hash,
    }
}

fn run_binary(args: &[&str], current_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rust-native-verifier"))
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("run rust-native-verifier")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is utf8")
}

#[test]
fn binary_writes_json_report_for_go_fixture() {
    let tempdir = TempDir::new().expect("tempdir");
    let fixture = create_go_fixture(&tempdir, false);
    let report_path = tempdir.path().join("report.json");
    let datadir = fixture.root.to_string_lossy().into_owned();
    let report_arg = report_path.to_string_lossy().into_owned();

    let output = run_binary(
        &[
            "--node-type",
            "go",
            "--datadir",
            &datadir,
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
