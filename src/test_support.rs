use prost::Message;
use rocksdb::{DB as RocksDb, Options as RocksOptions};
use rusty_leveldb::{DB as LevelDb, Options as LevelOptions};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

use crate::constants::ORIGINAL_GENESIS_HASH_HEX;
use crate::hashing::{hash32_from_hex, header_hash};
use crate::model::{Hash32, HeaderSource, HeaderStore, ParsedHeader, VerificationReport};
use crate::proto;
use crate::store::CheckpointStore;

pub(crate) struct FakeStore {
    pub(crate) headers: HashMap<Hash32, ParsedHeader>,
    pub(crate) tips: Vec<Hash32>,
    pub(crate) headers_selected_tip: Hash32,
    pub(crate) db_path: PathBuf,
    pub(crate) notes: Vec<String>,
}

impl HeaderSource for FakeStore {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> anyhow::Result<Option<ParsedHeader>> {
        Ok(self.headers.get(block_hash).cloned())
    }
}

impl HeaderStore for FakeStore {
    fn store_name(&self) -> &'static str {
        "Fake test store"
    }

    fn resolved_db_path(&self) -> &Path {
        &self.db_path
    }

    fn resolution_notes(&self) -> &[String] {
        &self.notes
    }

    fn tips(&mut self) -> anyhow::Result<(Vec<Hash32>, Hash32)> {
        Ok((self.tips.clone(), self.headers_selected_tip))
    }
}

pub(crate) fn test_hash(fill: u8) -> Hash32 {
    [fill; 32]
}

pub(crate) fn base_report() -> VerificationReport {
    VerificationReport {
        requested_node_type: "test".to_string(),
        ..VerificationReport::default()
    }
}

pub(crate) fn hardwired_genesis_header() -> ParsedHeader {
    ParsedHeader {
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
    }
}

pub(crate) fn original_genesis_header() -> ParsedHeader {
    let mut checkpoint_store = CheckpointStore::from_embedded_json().expect("checkpoint store");
    let original_genesis_hash =
        hash32_from_hex(ORIGINAL_GENESIS_HASH_HEX).expect("original genesis hash");
    checkpoint_store
        .get_raw_header(&original_genesis_hash)
        .expect("read original genesis")
        .expect("original genesis header")
}

pub(crate) fn make_tip_header_with_blue_work(
    pruning_point: Hash32,
    time_in_milliseconds: u64,
    blue_work_trimmed_be: Vec<u8>,
) -> (Hash32, ParsedHeader) {
    let header = ParsedHeader {
        version: 1,
        parents: vec![vec![test_hash(0x11)]],
        hash_merkle_root: test_hash(0x22),
        accepted_id_merkle_root: test_hash(0x33),
        utxo_commitment: test_hash(0x44),
        time_in_milliseconds,
        bits: 0x1d00ffff,
        nonce: 42,
        daa_score: 7,
        blue_score: 8,
        blue_work_trimmed_be,
        pruning_point,
    };
    let hash = header_hash(&header);
    (hash, header)
}

pub(crate) fn make_tip_header(
    pruning_point: Hash32,
    time_in_milliseconds: u64,
) -> (Hash32, ParsedHeader) {
    make_tip_header_with_blue_work(pruning_point, time_in_milliseconds, vec![0x01, 0x02, 0x03])
}

pub(crate) fn fake_store_with_tip(
    genesis_hash: Hash32,
    genesis_header: ParsedHeader,
    tip_time_in_milliseconds: u64,
) -> FakeStore {
    let (tip_hash, tip_header) = make_tip_header(genesis_hash, tip_time_in_milliseconds);
    let mut headers = HashMap::new();
    headers.insert(genesis_hash, genesis_header);
    headers.insert(tip_hash, tip_header);

    FakeStore {
        headers,
        tips: vec![tip_hash],
        headers_selected_tip: tip_hash,
        db_path: PathBuf::from("/tmp/fake-db"),
        notes: vec!["fixture note".to_string()],
    }
}

pub(crate) fn create_temp_datadir() -> (TempDir, PathBuf, PathBuf) {
    let tempdir = TempDir::new().expect("tempdir");
    let datadir = tempdir.path().join("datadir");
    let consensus_root = datadir.join("consensus");

    fs::create_dir_all(consensus_root.join("consensus-001")).expect("consensus-001");
    fs::create_dir_all(consensus_root.join("consensus-002")).expect("consensus-002");

    (tempdir, datadir, consensus_root)
}

pub(crate) fn create_meta_db(meta_path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
    fs::create_dir_all(meta_path).expect("create meta dir");

    let mut opts = RocksOptions::default();
    opts.create_if_missing(true);

    let db = RocksDb::open(&opts, meta_path).expect("open meta db");
    for (key, value) in entries {
        db.put(key, value).expect("write meta key");
    }
    drop(db);
}

pub(crate) fn create_consensus_db(db_path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
    fs::create_dir_all(db_path).expect("create consensus db dir");

    let mut opts = RocksOptions::default();
    opts.create_if_missing(true);

    let db = RocksDb::open(&opts, db_path).expect("open consensus db");
    for (key, value) in entries {
        db.put(key, value).expect("write consensus key");
    }
    drop(db);
}

pub(crate) fn create_go_leveldb(db_path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
    fs::create_dir_all(db_path).expect("create go db dir");

    let opts = LevelOptions {
        create_if_missing: true,
        ..LevelOptions::default()
    };

    let mut db = LevelDb::open(db_path, opts).expect("open go db");
    for (key, value) in entries {
        db.put(key, value).expect("write go db key");
    }
    db.flush().expect("flush go db");
    db.close().expect("close go db");
}

pub(crate) fn encode_option_u64(value: Option<u64>) -> Vec<u8> {
    match value {
        None => vec![0],
        Some(value) => {
            let mut bytes = vec![1];
            bytes.extend_from_slice(&value.to_le_bytes());
            bytes
        }
    }
}

pub(crate) fn encode_consensus_entry(
    key: u64,
    directory_name: &str,
    creation_timestamp: u64,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&key.to_le_bytes());
    bytes.extend_from_slice(&(directory_name.len() as u64).to_le_bytes());
    bytes.extend_from_slice(directory_name.as_bytes());
    bytes.extend_from_slice(&creation_timestamp.to_le_bytes());
    bytes
}

pub(crate) fn encode_db_hash(hash: Hash32) -> Vec<u8> {
    proto::DbHash {
        hash: hash.to_vec(),
    }
    .encode_to_vec()
}

pub(crate) fn encode_db_tips(tips: &[Hash32]) -> Vec<u8> {
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

pub(crate) fn encode_db_block_header(header: &ParsedHeader) -> Vec<u8> {
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

pub(crate) fn go_bucketed_key(active_prefix: u8, bucket: &[u8], suffix: Option<&[u8]>) -> Vec<u8> {
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

pub(crate) fn sample_go_header(selected_tip: Hash32) -> ParsedHeader {
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

pub(crate) fn embedded_checkpoint_headers_for_external_store() -> Vec<(Vec<u8>, Vec<u8>)> {
    let active_prefix = 0u8;
    let checkpoint_store = CheckpointStore::from_embedded_json().expect("checkpoint store");
    let mut entries = vec![(b"active-prefix".to_vec(), vec![active_prefix])];

    for (hash, header) in checkpoint_store.iter_headers() {
        entries.push((
            go_bucketed_key(active_prefix, b"block-headers", Some(hash)),
            encode_db_block_header(header),
        ));
    }

    entries
}
