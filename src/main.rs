use clap::{Parser, ValueEnum};
use rocksdb::DB as RocksDb;
use rusty_leveldb::DB as LevelDb;
use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

mod hashing;
mod model;
mod output;
mod store;
mod verify;

pub(crate) use model::{
    CheckpointJson, Hash32, HeaderSource, HeaderStore, ParsedHeader, Transaction,
    VerificationReport,
};
use output::{
    build_initial_report, capture_output_line, clear_output_capture, now_millis,
    output_capture_snapshot, print_error, print_info, print_plain, prompt_export_json_decision,
    write_json_report,
};
use verify::run;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/serialization.rs"));
}

const GREEN: &str = "\x1b[92m";
const RED: &str = "\x1b[91m";
const YELLOW: &str = "\x1b[93m";
const BLUE: &str = "\x1b[94m";
const BOLD: &str = "\x1b[1m";
const END: &str = "\x1b[0m";

const HARDWIRED_GENESIS_HASH_HEX: &str =
    "58c2d4199e21f910d1571d114969cecef48f09f934d42ccb6a281a15868f2999";
const ORIGINAL_GENESIS_HASH_HEX: &str =
    "caeb97960a160c211a6b2196bd78399fd4c4cc5b509f55c12c8a7d815f7536ea";
const CHECKPOINT_HASH_HEX: &str =
    "0fca37ca667c2d550a6c4416dad9717e50927128c424fa4edbebc436ab13aeef";
const EMPTY_MUHASH_HEX: &str = "544eb3142c000f0ad2c76ac41f4222abbababed830eeafee4b6dc56b52d5cac0";

const MAINNET_SUBNETWORK_ID_COINBASE_HEX: &str = "0100000000000000000000000000000000000000";
const HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX: &str =
    "0000000000000000000b1f8e1c17b0133d439174e52efbb0c41c3583a8aa66b0";
const ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX: &str =
    "00000000000000000001733c62adb19f1b77fa0735d0e11f25af36fc9ca908a5";

const HARDWIRED_GENESIS_TX_PAYLOAD_HEX: &str = "000000000000000000e1f5050000000000000100d795d79ed79420d793d79920d7a2d79cd799d79a20d795d7a2d79c20d790d797d799d79a20d799d799d798d79120d791d7a9d790d7a820d79bd7a1d7a4d79020d795d793d794d791d79420d79cd79ed7a2d791d79320d79bd7a8d7a2d795d7aa20d790d79cd794d79bd79d20d7aad7a2d791d793d795d79f0000000000000000000b1f8e1c17b0133d439174e52efbb0c41c3583a8aa66b00fca37ca667c2d550a6c4416dad9717e50927128c424fa4edbebc436ab13aeef";
const ORIGINAL_GENESIS_TX_PAYLOAD_HEX: &str = "000000000000000000e1f5050000000000000100d795d79ed79420d793d79920d7a2d79cd799d79a20d795d7a2d79c20d790d797d799d79a20d799d799d798d79120d791d7a9d790d7a820d79bd7a1d7a4d79020d795d793d794d791d79420d79cd79ed7a2d791d79320d79bd7a8d7a2d795d7aa20d790d79cd794d79bd79d20d7aad7a2d791d793d795d79f00000000000000000001733c62adb19f1b77fa0735d0e11f25af36fc9ca908a5";

const CHECKPOINT_DATA_JSON: &str = include_str!("../checkpoint_data.json");
const TIP_SYNC_WARNING_THRESHOLD_MS: u64 = 10 * 60 * 1000;
const RUST_MULTI_CONSENSUS_METADATA_KEY: &[u8] = &[124u8];
const RUST_CONSENSUS_ENTRY_PREFIX: &[u8] = &[125u8];
const LEGACY_MULTI_CONSENSUS_METADATA_KEY: &[u8] = b"multi-consensus-metadata-key";
const LEGACY_CONSENSUS_ENTRIES_PREFIX: &[u8] = b"consensus-entries-prefix";
const ROCKSDB_READ_ONLY_MAX_OPEN_FILES: i32 = 128;
static OUTPUT_CAPTURE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(
    name = "rust-native-verifier",
    about = "Verify Kaspa chain integrity from the current node state back to genesis",
    long_about = "Rust-native Kaspa genesis proof verifier. Verifies cryptographic linkage from the current node state back to genesis for both rusty-kaspa (RocksDB) and legacy kaspad (LevelDB), including the hardwired checkpoint/original-genesis proof chain.",
    after_help = "Examples:\n  rust-native-verifier\n  rust-native-verifier --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir\n  rust-native-verifier --no-input --json-out ./kaspa-proof-report.json"
)]
struct Cli {
    #[arg(
        long,
        value_enum,
        default_value_t = CliNodeType::Auto,
        help = "Node layout to use (auto detects Rust/Go by default)"
    )]
    node_type: CliNodeType,

    #[arg(
        long,
        help = "Path to Kaspa data directory. If omitted, KASPA_DATADIR and default OS paths are probed automatically"
    )]
    datadir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Optional pre-checkpoint Go datadir to reproduce the notebook's external checkpoint/original-genesis verification path"
    )]
    pre_checkpoint_datadir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        help = "Write a JSON verification report to this path without prompting (parent directories are created as needed)"
    )]
    json_out: Option<PathBuf>,

    #[arg(long, short = 'v', help = "Enable verbose chain-walk output")]
    verbose: bool,

    #[arg(
        long,
        help = "Disable interactive prompts and continue automatically when sync advisory is triggered"
    )]
    no_input: bool,

    #[arg(
        long,
        help = "Wait for Enter before exiting (useful for double-click launches)"
    )]
    pause_on_exit: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum CliNodeType {
    Auto,
    Rust,
    Go,
}

#[derive(Debug)]
struct RustStore {
    db: RocksDb,
    resolution: RustDbResolution,
}

struct GoStore {
    db: LevelDb,
    db_path: PathBuf,
    active_prefix: u8,
    notes: Vec<String>,
}

#[derive(Default)]
struct CheckpointStore {
    headers: HashMap<Hash32, ParsedHeader>,
}

#[derive(Debug)]
struct RustDbResolution {
    active_consensus_db_path: PathBuf,
    notes: Vec<String>,
}

struct OpenStoreResult {
    store: Box<dyn HeaderStore>,
    input_path: PathBuf,
    probe_notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MultiConsensusMetadata {
    current_consensus_key: Option<u64>,
    staging_consensus_key: Option<u64>,
    max_key_used: u64,
    is_archival_node: bool,
    props: HashMap<Vec<u8>, Vec<u8>>,
    version: u32,
}

#[derive(Debug, Deserialize)]
struct ConsensusEntry {
    key: u64,
    directory_name: String,
    creation_timestamp: u64,
}

#[derive(Debug, Deserialize)]
struct HeaderWithBlockLevelWireCompressed {
    header: HeaderWireCompressed,
    block_level: u8,
}

#[derive(Debug, Deserialize)]
struct HeaderWithBlockLevelWireLegacy {
    header: HeaderWireLegacy,
    block_level: u8,
}

#[derive(Debug, Deserialize)]
struct CompressedParentsWire(Vec<(u8, Vec<Hash32>)>);

#[derive(Debug, Deserialize)]
struct HeaderWireCompressed {
    hash: Hash32,
    version: u16,
    parents_by_level: CompressedParentsWire,
    hash_merkle_root: Hash32,
    accepted_id_merkle_root: Hash32,
    utxo_commitment: Hash32,
    timestamp: u64,
    bits: u32,
    nonce: u64,
    daa_score: u64,
    blue_work: [u64; 3],
    blue_score: u64,
    pruning_point: Hash32,
}

#[derive(Debug, Deserialize)]
struct HeaderWireLegacy {
    hash: Hash32,
    version: u16,
    parents_by_level: Vec<Vec<Hash32>>,
    hash_merkle_root: Hash32,
    accepted_id_merkle_root: Hash32,
    utxo_commitment: Hash32,
    timestamp: u64,
    bits: u32,
    nonce: u64,
    daa_score: u64,
    blue_work: [u64; 3],
    blue_score: u64,
    pruning_point: Hash32,
}

fn main() {
    clear_output_capture();
    let cli = Cli::parse();
    let mut report = build_initial_report(&cli);

    println!("{BOLD}Kaspa Genesis Proof Verification (Rust-Native){END}");
    capture_output_line("Kaspa Genesis Proof Verification (Rust-Native)");
    print_plain(&format!("Requested node type: {:?}", cli.node_type));

    if let Some(datadir) = cli.datadir.as_deref() {
        print_plain(&format!("Input data directory: {}", datadir.display()));
    } else {
        print_plain("Input data directory: auto-detect (OS default Kaspa locations)");
    }

    let mut exit_code = match run(&cli, &mut report) {
        Ok(success) => {
            report.success = success;
            if success { 0 } else { 1 }
        }
        Err(err) => {
            let error_chain = format!("{err:#}");
            print_error(&format!("Verification failed with error: {error_chain}"));
            report.success = false;
            report.error = Some(error_chain);
            1
        }
    };

    if let Some(json_out) = cli.json_out.as_ref() {
        report.screen_output_lines = output_capture_snapshot();
        match write_json_report(json_out, &report) {
            Ok(_) => print_info(&format!("JSON report written to {}", json_out.display())),
            Err(err) => {
                print_error(&format!("Failed writing JSON report: {err}"));
                exit_code = 1;
            }
        }
    } else {
        match prompt_export_json_decision(cli.no_input) {
            Ok(true) => {
                let json_out = PathBuf::from(format!(
                    "kaspa-proof-report-{}.json",
                    now_millis().unwrap_or(0)
                ));
                report.screen_output_lines = output_capture_snapshot();
                match write_json_report(&json_out, &report) {
                    Ok(_) => print_info(&format!("JSON report written to {}", json_out.display())),
                    Err(err) => {
                        print_error(&format!("Failed writing JSON report: {err}"));
                        exit_code = 1;
                    }
                }
            }
            Ok(false) => {}
            Err(err) => {
                print_error(&format!("Failed during export prompt: {err}"));
                exit_code = 1;
            }
        }
    }

    if cli.pause_on_exit {
        print_plain("");
        print_plain("Press Enter to exit...");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }

    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use rocksdb::{DB as RocksDb, Options as RocksOptions};
    use rusty_leveldb::{DB as LevelDb, Options as LevelOptions};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    use crate::hashing::{
        decode_tip_hash_from_key_suffix, hash32_from_hex, header_hash, hex_of, transaction_hash,
    };
    use crate::store::{
        open_store_with_resolved_input, parse_consensus_entry_dir_name,
        parse_current_consensus_key, resolve_rust_db_path,
    };
    use crate::verify::{hardwired_genesis_coinbase_tx, original_genesis_coinbase_tx};

    fn create_temp_datadir() -> (TempDir, PathBuf, PathBuf) {
        let tempdir = TempDir::new().expect("tempdir");
        let datadir = tempdir.path().join("datadir");
        let consensus_root = datadir.join("consensus");

        fs::create_dir_all(consensus_root.join("consensus-001")).expect("consensus-001");
        fs::create_dir_all(consensus_root.join("consensus-002")).expect("consensus-002");

        (tempdir, datadir, consensus_root)
    }

    fn create_meta_db(meta_path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
        fs::create_dir_all(meta_path).expect("create meta dir");

        let mut opts = RocksOptions::default();
        opts.create_if_missing(true);

        let db = RocksDb::open(&opts, meta_path).expect("open meta db");
        for (key, value) in entries {
            db.put(key, value).expect("write meta key");
        }
        drop(db);
    }

    fn create_consensus_db(db_path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
        fs::create_dir_all(db_path).expect("create consensus db dir");

        let mut opts = RocksOptions::default();
        opts.create_if_missing(true);

        let db = RocksDb::open(&opts, db_path).expect("open consensus db");
        for (key, value) in entries {
            db.put(key, value).expect("write consensus key");
        }
        drop(db);
    }

    fn create_go_leveldb(db_path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
        fs::create_dir_all(db_path).expect("create go db dir");

        let mut opts = LevelOptions::default();
        opts.create_if_missing = true;

        let mut db = LevelDb::open(db_path, opts).expect("open go db");
        for (key, value) in entries {
            db.put(key, value).expect("write go db key");
        }
        db.flush().expect("flush go db");
        db.close().expect("close go db");
    }

    fn test_hash(fill: u8) -> Hash32 {
        [fill; 32]
    }

    fn encode_option_u64(value: Option<u64>) -> Vec<u8> {
        match value {
            None => vec![0],
            Some(value) => {
                let mut bytes = vec![1];
                bytes.extend_from_slice(&value.to_le_bytes());
                bytes
            }
        }
    }

    fn encode_consensus_entry(key: u64, directory_name: &str, creation_timestamp: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&key.to_le_bytes());
        bytes.extend_from_slice(&(directory_name.len() as u64).to_le_bytes());
        bytes.extend_from_slice(directory_name.as_bytes());
        bytes.extend_from_slice(&creation_timestamp.to_le_bytes());
        bytes
    }

    fn encode_db_hash(hash: Hash32) -> Vec<u8> {
        proto::DbHash {
            hash: hash.to_vec(),
        }
        .encode_to_vec()
    }

    fn encode_db_tips(tips: &[Hash32]) -> Vec<u8> {
        proto::DbTips {
            tips: tips
                .iter()
                .map(|hash| proto::DbHash {
                    hash: hash.to_vec(),
                })
                .collect(),
        }
        .encode_to_vec()
    }

    fn encode_db_block_header(header: &ParsedHeader) -> Vec<u8> {
        proto::DbBlockHeader {
            version: u32::from(header.version),
            parents: header
                .parents
                .iter()
                .map(|level| proto::DbBlockLevelParents {
                    parent_hashes: level
                        .iter()
                        .map(|hash| proto::DbHash {
                            hash: hash.to_vec(),
                        })
                        .collect(),
                })
                .collect(),
            hash_merkle_root: Some(proto::DbHash {
                hash: header.hash_merkle_root.to_vec(),
            }),
            accepted_id_merkle_root: Some(proto::DbHash {
                hash: header.accepted_id_merkle_root.to_vec(),
            }),
            utxo_commitment: Some(proto::DbHash {
                hash: header.utxo_commitment.to_vec(),
            }),
            time_in_milliseconds: i64::try_from(header.time_in_milliseconds)
                .expect("header timestamp fits in i64"),
            bits: header.bits,
            nonce: header.nonce,
            daa_score: header.daa_score,
            blue_work: header.blue_work_trimmed_be.clone(),
            pruning_point: Some(proto::DbHash {
                hash: header.pruning_point.to_vec(),
            }),
            blue_score: header.blue_score,
        }
        .encode_to_vec()
    }

    fn go_bucketed_key(active_prefix: u8, bucket: &[u8], suffix: Option<&[u8]>) -> Vec<u8> {
        let mut key =
            Vec::with_capacity(2 + bucket.len() + suffix.map(|s| 1 + s.len()).unwrap_or(0));
        key.push(active_prefix);
        key.push(b'/');
        key.extend_from_slice(bucket);
        if let Some(suffix) = suffix {
            key.push(b'/');
            key.extend_from_slice(suffix);
        }
        key
    }

    fn sample_go_header(selected_tip: Hash32) -> ParsedHeader {
        ParsedHeader {
            version: 9,
            parents: vec![
                vec![test_hash(0x33)],
                vec![test_hash(0x44), test_hash(0x55)],
            ],
            hash_merkle_root: test_hash(0x66),
            accepted_id_merkle_root: test_hash(0x77),
            utxo_commitment: test_hash(0x88),
            time_in_milliseconds: 1_717_171_717_171,
            bits: 123_456_789,
            nonce: 987_654_321,
            daa_score: 456_789,
            blue_score: 654_321,
            blue_work_trimmed_be: vec![0x01, 0x23, 0x45, 0x67],
            pruning_point: selected_tip,
        }
    }

    #[test]
    fn resolve_rust_db_path_falls_back_to_latest_consensus_dir_without_meta_db() {
        let (_tempdir, datadir, consensus_root) = create_temp_datadir();

        let resolution = resolve_rust_db_path(&datadir).expect("resolve rust datadir");

        assert_eq!(
            resolution.active_consensus_db_path,
            consensus_root.join("consensus-002")
        );
        assert!(
            resolution
                .notes
                .iter()
                .any(|note| note.contains("fallback"))
        );
    }

    #[test]
    fn resolve_rust_db_path_supports_legacy_meta_keys() {
        let (_tempdir, datadir, consensus_root) = create_temp_datadir();
        let entry_key = [b"consensus-entries-prefix".as_slice(), &1u64.to_le_bytes()].concat();

        create_meta_db(
            &datadir.join("meta"),
            &[
                (
                    b"multi-consensus-metadata-key".to_vec(),
                    encode_option_u64(Some(1)),
                ),
                (entry_key, encode_consensus_entry(1, "consensus-001", 123)),
            ],
        );

        let resolution = resolve_rust_db_path(&datadir).expect("resolve rust datadir");

        assert_eq!(
            resolution.active_consensus_db_path,
            consensus_root.join("consensus-001")
        );
        assert!(
            resolution
                .notes
                .iter()
                .any(|note| note.contains("rust-meta-managed"))
        );
    }

    #[test]
    fn resolve_rust_db_path_supports_minimal_metadata_encoding() {
        let (_tempdir, datadir, consensus_root) = create_temp_datadir();
        let entry_key = [[125u8].as_slice(), &1u64.to_le_bytes()].concat();

        create_meta_db(
            &datadir.join("meta"),
            &[
                (vec![124u8], encode_option_u64(Some(1))),
                (entry_key, encode_consensus_entry(1, "consensus-001", 123)),
            ],
        );

        let resolution = resolve_rust_db_path(&datadir).expect("resolve rust datadir");

        assert_eq!(
            resolution.active_consensus_db_path,
            consensus_root.join("consensus-001")
        );
        assert!(
            resolution
                .notes
                .iter()
                .any(|note| note.contains("rust-meta-managed"))
        );
    }

    #[test]
    fn resolve_rust_db_path_errors_when_current_consensus_entry_is_missing() {
        let (_tempdir, datadir, _consensus_root) = create_temp_datadir();

        create_meta_db(
            &datadir.join("meta"),
            &[(vec![124u8], encode_option_u64(Some(7)))],
        );

        let err = resolve_rust_db_path(&datadir)
            .expect_err("missing metadata-selected consensus entry should fail");
        let err_text = format!("{err:#}");

        assert!(err_text.contains("current consensus key 7"));
        assert!(err_text.contains("no matching consensus entry"));
    }

    #[test]
    fn hardwired_genesis_coinbase_tx_hash_matches_live_node_merkle_root() {
        let tx = hardwired_genesis_coinbase_tx().expect("hardwired tx");
        let tx_hash = transaction_hash(&tx, true);

        assert_eq!(
            hex_of(&tx_hash),
            "8ec898568c6801d13df4ee6e2a1b54b7e6236f671f20954f05306410518eeb32"
        );
    }

    #[test]
    fn hardwired_genesis_payload_embeds_expected_bitcoin_and_checkpoint_hashes() {
        let tx = hardwired_genesis_coinbase_tx().expect("hardwired tx");

        assert_eq!(
            hex::encode(&tx.payload[140..172]),
            HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX
        );
        assert_eq!(hex::encode(&tx.payload[172..204]), CHECKPOINT_HASH_HEX);
    }

    #[test]
    fn original_genesis_coinbase_tx_hash_matches_original_genesis_merkle_root() {
        let tx = original_genesis_coinbase_tx().expect("original genesis tx");
        let tx_hash = transaction_hash(&tx, true);

        assert_eq!(
            hex_of(&tx_hash),
            "caedaf7d4a08bbe89011640c4841b66d5bba67d7288ce6d67228db000966e974"
        );
    }

    #[test]
    fn original_genesis_payload_embeds_expected_bitcoin_hash() {
        let tx = original_genesis_coinbase_tx().expect("original genesis tx");

        assert_eq!(
            hex::encode(&tx.payload[140..172]),
            ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX
        );
    }

    #[test]
    fn hardwired_genesis_header_hash_matches_live_node_hash() {
        let header = ParsedHeader {
            version: 0,
            parents: Vec::new(),
            hash_merkle_root: hash32_from_hex(
                "8ec898568c6801d13df4ee6e2a1b54b7e6236f671f20954f05306410518eeb32",
            )
            .expect("hash merkle root"),
            accepted_id_merkle_root: [0u8; 32],
            utxo_commitment: hash32_from_hex(
                "710f27df423e63aa6cdb72b89ea5a06cffa399d66f167704455b5af59def8e20",
            )
            .expect("utxo commitment"),
            time_in_milliseconds: 1_637_609_671_037,
            bits: 486_722_099,
            nonce: 211_244,
            daa_score: 1_312_860,
            blue_score: 0,
            blue_work_trimmed_be: Vec::new(),
            pruning_point: [0u8; 32],
        };

        assert_eq!(hex_of(&header_hash(&header)), HARDWIRED_GENESIS_HASH_HEX);
    }

    #[test]
    fn embedded_checkpoint_store_reaches_original_genesis_with_empty_utxo_commitment() {
        let mut store = CheckpointStore::from_embedded_json().expect("checkpoint store");
        let checkpoint_hash = hash32_from_hex(CHECKPOINT_HASH_HEX).expect("checkpoint hash");
        let original_genesis_hash =
            hash32_from_hex(ORIGINAL_GENESIS_HASH_HEX).expect("original genesis hash");
        let empty_muhash = hash32_from_hex(EMPTY_MUHASH_HEX).expect("empty muhash");

        let checkpoint_header = store
            .get_raw_header(&checkpoint_hash)
            .expect("checkpoint lookup")
            .expect("checkpoint header");
        let original_genesis_header = store
            .get_raw_header(&original_genesis_hash)
            .expect("genesis lookup")
            .expect("genesis header");

        assert_eq!(
            hex_of(&header_hash(&checkpoint_header)),
            CHECKPOINT_HASH_HEX
        );
        assert_eq!(
            hex_of(&header_hash(&original_genesis_header)),
            ORIGINAL_GENESIS_HASH_HEX
        );
        assert_eq!(original_genesis_header.utxo_commitment, empty_muhash);
    }

    #[test]
    fn parse_current_consensus_key_supports_live_node_metadata_bytes() {
        let live_metadata_bytes =
            hex::decode("01020000000000000000020000000000000000000000000000000006000000")
                .expect("metadata bytes");

        assert_eq!(
            parse_current_consensus_key(&live_metadata_bytes).expect("parse metadata"),
            Some(2)
        );
    }

    #[test]
    fn parse_consensus_entry_dir_name_supports_live_node_entry_bytes() {
        let live_entry_bytes = hex::decode(
            "02000000000000000d00000000000000636f6e73656e7375732d3030327b2340189d010000",
        )
        .expect("entry bytes");

        assert_eq!(
            parse_consensus_entry_dir_name(&live_entry_bytes).expect("parse entry"),
            "consensus-002"
        );
    }

    #[test]
    fn decode_tip_hash_from_key_suffix_supports_live_raw_tip_keys() {
        let tip_hash = test_hash(0x24);

        assert_eq!(
            decode_tip_hash_from_key_suffix(&tip_hash).expect("decode raw tip suffix"),
            tip_hash
        );
    }

    #[test]
    fn decode_tip_hash_from_key_suffix_supports_length_prefixed_tip_keys() {
        let tip_hash = test_hash(0x42);
        let mut encoded = Vec::from(32u64.to_le_bytes());
        encoded.extend_from_slice(&tip_hash);

        assert_eq!(
            decode_tip_hash_from_key_suffix(&encoded).expect("decode length-prefixed tip suffix"),
            tip_hash
        );
    }

    #[test]
    fn rust_store_tips_reads_live_style_tip_keys() {
        let (_tempdir, datadir, consensus_root) = create_temp_datadir();
        let db_path = consensus_root.join("consensus-002");
        let headers_selected_tip = test_hash(0x90);
        let other_tip = test_hash(0xab);

        create_consensus_db(
            &db_path,
            &[
                (vec![7u8], headers_selected_tip.to_vec()),
                (
                    [vec![24u8], headers_selected_tip.to_vec()].concat(),
                    Vec::new(),
                ),
                ([vec![24u8], other_tip.to_vec()].concat(), Vec::new()),
            ],
        );

        let mut store = RustStore::open(&datadir).expect("open rust store");
        let (tips, hst) = store.tips().expect("read tips");

        assert_eq!(hst, headers_selected_tip);
        assert_eq!(tips, vec![headers_selected_tip, other_tip]);
    }

    #[test]
    fn rust_store_tips_preserves_iterator_order_without_hash_sorting() {
        let (_tempdir, datadir, consensus_root) = create_temp_datadir();
        let db_path = consensus_root.join("consensus-002");
        let headers_selected_tip = test_hash(0x77);
        let length_prefixed_tip = test_hash(0x01);
        let raw_tip = test_hash(0x90);
        let mut encoded_prefixed_tip = Vec::from(32u64.to_le_bytes());
        encoded_prefixed_tip.extend_from_slice(&length_prefixed_tip);

        create_consensus_db(
            &db_path,
            &[
                (vec![7u8], headers_selected_tip.to_vec()),
                ([vec![24u8], raw_tip.to_vec()].concat(), Vec::new()),
                ([vec![24u8], encoded_prefixed_tip].concat(), Vec::new()),
            ],
        );

        let mut store = RustStore::open(&datadir).expect("open rust store");
        let (tips, _hst) = store.tips().expect("read tips");

        assert_eq!(tips, vec![length_prefixed_tip, raw_tip]);
    }

    #[test]
    fn rust_store_tips_returns_empty_list_when_tip_store_is_empty() {
        let (_tempdir, _datadir, consensus_root) = create_temp_datadir();
        let db_path = consensus_root.join("consensus-002");
        let headers_selected_tip = test_hash(0x55);

        create_consensus_db(&db_path, &[(vec![7u8], headers_selected_tip.to_vec())]);

        let mut store = RustStore::open(&db_path).expect("open rust store");
        let (tips, hst) = store.tips().expect("read tips");

        assert_eq!(hst, headers_selected_tip);
        assert!(tips.is_empty());
    }

    #[test]
    fn rust_store_open_rejects_db_without_headers_selected_tip_key() {
        let (_tempdir, _datadir, consensus_root) = create_temp_datadir();
        let db_path = consensus_root.join("consensus-002");

        create_consensus_db(&db_path, &[]);

        let err = RustStore::open(&db_path)
            .expect_err("db without rust headers selected tip key should be rejected");
        let err_text = format!("{err:#}");

        assert!(err_text.contains("not a valid rusty-kaspa consensus DB"));
        assert!(err_text.contains("missing headers selected tip key"));
    }

    #[test]
    fn rocksdb_read_only_open_files_limit_stays_bounded_for_live_nodes() {
        assert_eq!(ROCKSDB_READ_ONLY_MAX_OPEN_FILES, 128);
        assert!(ROCKSDB_READ_ONLY_MAX_OPEN_FILES > 0);
        assert!(ROCKSDB_READ_ONLY_MAX_OPEN_FILES < 1024);
    }

    #[test]
    fn go_store_open_finds_nested_datadir2_and_reads_upstream_layout() {
        let tempdir = TempDir::new().expect("tempdir");
        let root = tempdir.path().join("go-node");
        let db_path = root.join("kaspa-mainnet").join("datadir2");
        let active_prefix = 1u8;
        let selected_tip = test_hash(0x91);
        let other_tip = test_hash(0xa2);
        let header = sample_go_header(test_hash(0xbb));

        create_go_leveldb(
            &db_path,
            &[
                (b"active-prefix".to_vec(), vec![active_prefix]),
                (
                    go_bucketed_key(active_prefix, b"headers-selected-tip", None),
                    encode_db_hash(selected_tip),
                ),
                (
                    go_bucketed_key(active_prefix, b"tips", None),
                    encode_db_tips(&[selected_tip, other_tip]),
                ),
                (
                    go_bucketed_key(active_prefix, b"block-headers", Some(&selected_tip)),
                    encode_db_block_header(&header),
                ),
            ],
        );

        let mut store = GoStore::open(&root).expect("open go store");
        let (tips, hst) = store.tips().expect("tips");
        let decoded_header = store
            .get_raw_header(&selected_tip)
            .expect("read header")
            .expect("header exists");

        assert_eq!(store.db_path, db_path);
        assert_eq!(store.active_prefix, active_prefix);
        assert_eq!(hst, selected_tip);
        assert_eq!(tips, vec![selected_tip, other_tip]);
        assert_eq!(decoded_header.version, header.version);
        assert_eq!(decoded_header.parents, header.parents);
        assert_eq!(decoded_header.hash_merkle_root, header.hash_merkle_root);
        assert_eq!(
            decoded_header.accepted_id_merkle_root,
            header.accepted_id_merkle_root
        );
        assert_eq!(decoded_header.utxo_commitment, header.utxo_commitment);
        assert_eq!(
            decoded_header.time_in_milliseconds,
            header.time_in_milliseconds
        );
        assert_eq!(decoded_header.bits, header.bits);
        assert_eq!(decoded_header.nonce, header.nonce);
        assert_eq!(decoded_header.daa_score, header.daa_score);
        assert_eq!(decoded_header.blue_score, header.blue_score);
        assert_eq!(
            decoded_header.blue_work_trimmed_be,
            header.blue_work_trimmed_be
        );
        assert_eq!(decoded_header.pruning_point, header.pruning_point);
    }

    #[test]
    fn go_store_open_prefers_datadir2_over_datadir() {
        let tempdir = TempDir::new().expect("tempdir");
        let root = tempdir.path().join("go-node");
        let active_prefix = 1u8;
        let stale_tip = test_hash(0x11);
        let active_tip = test_hash(0x22);

        create_go_leveldb(
            &root.join("kaspa-mainnet").join("datadir"),
            &[
                (b"active-prefix".to_vec(), vec![active_prefix]),
                (
                    go_bucketed_key(active_prefix, b"headers-selected-tip", None),
                    encode_db_hash(stale_tip),
                ),
                (
                    go_bucketed_key(active_prefix, b"tips", None),
                    encode_db_tips(&[stale_tip]),
                ),
            ],
        );
        create_go_leveldb(
            &root.join("kaspa-mainnet").join("datadir2"),
            &[
                (b"active-prefix".to_vec(), vec![active_prefix]),
                (
                    go_bucketed_key(active_prefix, b"headers-selected-tip", None),
                    encode_db_hash(active_tip),
                ),
                (
                    go_bucketed_key(active_prefix, b"tips", None),
                    encode_db_tips(&[active_tip]),
                ),
            ],
        );

        let mut store = GoStore::open(&root.join("kaspa-mainnet")).expect("open go store");
        let (tips, hst) = store.tips().expect("tips");

        assert_eq!(store.db_path, root.join("kaspa-mainnet").join("datadir2"));
        assert_eq!(hst, active_tip);
        assert_eq!(tips, vec![active_tip]);
    }

    #[test]
    fn go_store_tips_preserves_empty_proto_tip_set() {
        let tempdir = TempDir::new().expect("tempdir");
        let db_path = tempdir.path().join("datadir2");
        let active_prefix = 0u8;
        let selected_tip = test_hash(0x5a);

        create_go_leveldb(
            &db_path,
            &[
                (b"active-prefix".to_vec(), vec![active_prefix]),
                (
                    go_bucketed_key(active_prefix, b"headers-selected-tip", None),
                    encode_db_hash(selected_tip),
                ),
                (
                    go_bucketed_key(active_prefix, b"tips", None),
                    encode_db_tips(&[]),
                ),
            ],
        );

        let mut store = GoStore::open(&db_path).expect("open go store");
        let (tips, hst) = store.tips().expect("tips");

        assert_eq!(hst, selected_tip);
        assert!(tips.is_empty());
    }

    #[test]
    fn auto_detect_prefers_go_store_for_go_leveldb_datadir() {
        let tempdir = TempDir::new().expect("tempdir");
        let db_path = tempdir.path().join("datadir2");
        let active_prefix = 1u8;
        let selected_tip = test_hash(0x6b);

        create_go_leveldb(
            &db_path,
            &[
                (b"active-prefix".to_vec(), vec![active_prefix]),
                (
                    go_bucketed_key(active_prefix, b"headers-selected-tip", None),
                    encode_db_hash(selected_tip),
                ),
                (
                    go_bucketed_key(active_prefix, b"tips", None),
                    encode_db_tips(&[selected_tip]),
                ),
            ],
        );

        let cli = Cli {
            node_type: CliNodeType::Auto,
            datadir: Some(db_path.clone()),
            pre_checkpoint_datadir: None,
            json_out: None,
            verbose: false,
            no_input: true,
            pause_on_exit: false,
        };

        let result = open_store_with_resolved_input(&cli).expect("auto-detect store");

        assert_eq!(
            result.store.store_name(),
            "Go node store (LevelDB + Protobuf)"
        );
        assert_eq!(result.store.resolved_db_path(), db_path.as_path());
    }

    #[test]
    fn cli_accepts_json_out_flag() {
        let cli = Cli::try_parse_from([
            "rust-native-verifier",
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
        let cli = Cli::try_parse_from([
            "rust-native-verifier",
            "--pre-checkpoint-datadir",
            "/tmp/pre-checkpoint",
        ])
        .expect("parse cli");

        assert_eq!(
            cli.pre_checkpoint_datadir,
            Some(PathBuf::from("/tmp/pre-checkpoint"))
        );
    }
}
