use crate::cli::{Cli, CliNodeType};
use crate::constants::{
    CHECKPOINT_HASH_HEX, EMPTY_MUHASH_HEX, ORIGINAL_GENESIS_HASH_HEX,
    ROCKSDB_READ_ONLY_MAX_OPEN_FILES,
};
use crate::hashing::{hash32_from_hex, header_hash, hex_of};
use crate::model::{HeaderSource, HeaderStore};
use crate::test_support::{
    create_consensus_db, create_go_leveldb, create_meta_db, create_temp_datadir,
    encode_consensus_entry, encode_db_block_header, encode_db_hash, encode_db_tips,
    encode_option_u64, go_bucketed_key, sample_go_header, test_hash,
};

use super::go::go_db_key;
use super::probe::{
    parse_consensus_entry_dir_name, parse_current_consensus_key, resolve_rust_db_path,
};
use super::rust::{decode_tip_hash_from_key_suffix, is_transient_rocksdb_open_failure};
use super::{CheckpointStore, GoStore, RustStore, open_store_with_resolved_input};

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
    let live_entry_bytes =
        hex::decode("02000000000000000d00000000000000636f6e73656e7375732d3030327b2340189d010000")
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
fn detects_transient_rocksdb_open_failures_from_missing_sst_files() {
    let err = "Corruption: Can't access /5174030.sst: IO error: No such file or directory";
    assert!(is_transient_rocksdb_open_failure(err));
    assert!(!is_transient_rocksdb_open_failure(
        "IO error: lock hold by current process"
    ));
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
}

#[test]
fn go_store_open_finds_nested_datadir2_and_reads_upstream_layout() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
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
                go_db_key(active_prefix, b"headers-selected-tip", None),
                encode_db_hash(selected_tip),
            ),
            (
                go_db_key(active_prefix, b"tips", None),
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

    assert_eq!(store.resolved_db_path(), db_path.as_path());
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
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let root = tempdir.path().join("go-node");
    let active_prefix = 1u8;
    let stale_tip = test_hash(0x11);
    let active_tip = test_hash(0x22);

    create_go_leveldb(
        &root.join("kaspa-mainnet").join("datadir"),
        &[
            (b"active-prefix".to_vec(), vec![active_prefix]),
            (
                go_db_key(active_prefix, b"headers-selected-tip", None),
                encode_db_hash(stale_tip),
            ),
            (
                go_db_key(active_prefix, b"tips", None),
                encode_db_tips(&[stale_tip]),
            ),
        ],
    );
    create_go_leveldb(
        &root.join("kaspa-mainnet").join("datadir2"),
        &[
            (b"active-prefix".to_vec(), vec![active_prefix]),
            (
                go_db_key(active_prefix, b"headers-selected-tip", None),
                encode_db_hash(active_tip),
            ),
            (
                go_db_key(active_prefix, b"tips", None),
                encode_db_tips(&[active_tip]),
            ),
        ],
    );

    let mut store = GoStore::open(&root.join("kaspa-mainnet")).expect("open go store");
    let (tips, hst) = store.tips().expect("tips");

    assert_eq!(
        store.resolved_db_path(),
        root.join("kaspa-mainnet").join("datadir2")
    );
    assert_eq!(hst, active_tip);
    assert_eq!(tips, vec![active_tip]);
}

#[test]
fn go_store_tips_preserves_empty_proto_tip_set() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("datadir2");
    let active_prefix = 0u8;
    let selected_tip = test_hash(0x5a);

    create_go_leveldb(
        &db_path,
        &[
            (b"active-prefix".to_vec(), vec![active_prefix]),
            (
                go_db_key(active_prefix, b"headers-selected-tip", None),
                encode_db_hash(selected_tip),
            ),
            (go_db_key(active_prefix, b"tips", None), encode_db_tips(&[])),
        ],
    );

    let mut store = GoStore::open(&db_path).expect("open go store");
    let (tips, hst) = store.tips().expect("tips");

    assert_eq!(hst, selected_tip);
    assert!(tips.is_empty());
}

#[test]
fn auto_detect_prefers_go_store_for_go_leveldb_datadir() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("datadir2");
    let active_prefix = 1u8;
    let selected_tip = test_hash(0x6b);

    create_go_leveldb(
        &db_path,
        &[
            (b"active-prefix".to_vec(), vec![active_prefix]),
            (
                go_db_key(active_prefix, b"headers-selected-tip", None),
                encode_db_hash(selected_tip),
            ),
            (
                go_db_key(active_prefix, b"tips", None),
                encode_db_tips(&[selected_tip]),
            ),
        ],
    );

    let cli = Cli {
        node_type: CliNodeType::Auto,
        datadir: Some(db_path.clone()),
        pre_checkpoint_datadir: None,
        checkpoint_utxos_gz: None,
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
