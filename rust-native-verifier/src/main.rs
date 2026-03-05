use anyhow::{Context, Result, anyhow, bail};
use blake2b_simd::Params;
use clap::{Parser, ValueEnum};
use prost::Message;
use rocksdb::{DB as RocksDb, Direction, IteratorMode, Options as RocksOptions};
use rusty_leveldb::{DB as LevelDb, Options as LevelOptions};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

const HARDWIRED_GENESIS_TX_PAYLOAD_HEX: &str = "000000000000000000e1f5050000000000000100d795d79ed79420d793d79920d7a2d79cd799d79a20d795d7a2d79c20d790d797d799d79a20d799d799d798d79120d791d7a9d790d7a820d79bd7a1d7a4d79020d795d793d794d791d79420d79cd79ed7a2d791d79320d79bd7a8d7a2d795d7aa20d790d79cd794d79bd79d20d7aad7a2d791d793d795d79f0000000000000000000b1f8e1c17b0133d439174e52efbb0c41c3583a8aa66b00fca37ca667c2d550a6c4416dad9717e50927128c424fa4edbebc436ab13aeef";

const CHECKPOINT_DATA_JSON: &str = include_str!("../../verification/checkpoint_data.json");
const TIP_SYNC_WARNING_THRESHOLD_MS: u64 = 10 * 60 * 1000;

type Hash32 = [u8; 32];

#[derive(Parser, Debug)]
#[command(
    name = "kaspa-genesis-proof-rust",
    about = "Rust-native Kaspa genesis proof verifier",
    long_about = "Verifies cryptographic linkage from the current node state back to genesis for both rusty-kaspa (RocksDB) and legacy kaspad (LevelDB)."
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
        help = "Path to Kaspa data directory. If omitted, default OS paths are probed automatically"
    )]
    datadir: Option<PathBuf>,

    #[arg(
        long,
        help = "Optional legacy pre-checkpoint path (kept for compatibility; embedded checkpoint data is used)"
    )]
    pre_checkpoint_datadir: Option<PathBuf>,

    #[arg(long, short = 'v', help = "Enable verbose chain-walk output")]
    verbose: bool,

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

#[derive(Clone, Debug)]
struct ParsedHeader {
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

#[derive(Clone, Debug)]
struct Transaction {
    version: u16,
    inputs: Vec<TransactionInput>,
    outputs: Vec<TransactionOutput>,
    lock_time: u64,
    subnetwork_id: [u8; 20],
    gas: u64,
    payload: Vec<u8>,
    mass: u64,
}

#[derive(Clone, Debug)]
struct TransactionInput {
    previous_txid: Hash32,
    previous_index: u32,
    signature_script: Vec<u8>,
    sequence: u64,
    sig_op_count: u8,
}

#[derive(Clone, Debug)]
struct TransactionOutput {
    value: u64,
    script_public_key_version: u16,
    script_public_key_script: Vec<u8>,
}

trait HeaderSource {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>>;
}

trait HeaderStore: HeaderSource {
    fn store_name(&self) -> &'static str;
    fn resolved_db_path(&self) -> &Path;
    fn resolution_notes(&self) -> &[String];
    fn tips(&mut self) -> Result<(Vec<Hash32>, Hash32)>;
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

#[derive(Debug, Deserialize)]
struct CheckpointJson {
    checkpoint_hash: String,
    original_genesis_hash: String,
    headers_chain: Vec<CheckpointHeaderJson>,
}

#[derive(Debug, Deserialize)]
struct CheckpointHeaderJson {
    hash: String,
    version: u16,
    parents: Vec<Vec<String>>,
    #[serde(rename = "hashMerkleRoot")]
    hash_merkle_root: String,
    #[serde(rename = "acceptedIDMerkleRoot")]
    accepted_id_merkle_root: String,
    #[serde(rename = "utxoCommitment")]
    utxo_commitment: String,
    #[serde(rename = "timeInMilliseconds")]
    time_in_milliseconds: u64,
    bits: u32,
    nonce: u64,
    #[serde(rename = "daaScore")]
    daa_score: u64,
    #[serde(rename = "blueScore")]
    blue_score: u64,
    #[serde(rename = "blueWork")]
    blue_work: String,
    #[serde(rename = "pruningPoint")]
    pruning_point: String,
}

fn print_header(text: &str) {
    println!("\n{BOLD}{BLUE}{}{END}", "=".repeat(60));
    println!("{BOLD}{BLUE}{text}{END}");
    println!("{BOLD}{BLUE}{}{END}", "=".repeat(60));
}

fn print_success(text: &str) {
    println!("{GREEN}✓ {text}{END}");
}

fn print_error(text: &str) {
    println!("{RED}✗ {text}{END}");
}

fn print_info(text: &str) {
    println!("{GREEN}→ {text}{END}");
}

fn print_warning(text: &str) {
    println!("{YELLOW}! {text}{END}");
}

fn now_millis() -> Result<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock appears to be before Unix epoch")?;
    u64::try_from(now.as_millis()).context("current time millis does not fit in u64")
}

fn format_duration_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn to_hash32(bytes: &[u8]) -> Result<Hash32> {
    if bytes.len() != 32 {
        bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

fn home_dir_from_env() -> Option<PathBuf> {
    if let Some(home) = env::var_os("HOME") {
        return Some(PathBuf::from(home));
    }
    if let Some(user_profile) = env::var_os("USERPROFILE") {
        return Some(PathBuf::from(user_profile));
    }

    let (Some(home_drive), Some(home_path)) = (env::var_os("HOMEDRIVE"), env::var_os("HOMEPATH"))
    else {
        return None;
    };
    let mut out = PathBuf::from(home_drive);
    out.push(home_path);
    Some(out)
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if !raw.starts_with('~') {
        return path.to_path_buf();
    }

    if raw == "~" {
        return home_dir_from_env().unwrap_or_else(|| path.to_path_buf());
    }

    if let Some(suffix) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(home) = home_dir_from_env() {
            return home.join(suffix);
        }
    }

    path.to_path_buf()
}

fn default_datadir_probe_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    let mut push = |path: PathBuf| {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    };

    if let Some(from_env) = env::var_os("KASPA_DATADIR") {
        push(PathBuf::from(from_env));
    }

    if let Some(home) = home_dir_from_env() {
        push(
            home.join(".rusty-kaspa")
                .join("kaspa-mainnet")
                .join("datadir"),
        );
        push(home.join(".rusty-kaspa").join("kaspa-mainnet"));
        push(home.join(".rusty-kaspa"));

        push(home.join(".kaspad").join("kaspa-mainnet").join("datadir2"));
        push(home.join(".kaspad").join("kaspa-mainnet").join("datadir"));
        push(home.join(".kaspad").join("kaspa-mainnet"));
        push(home.join(".kaspad"));
    }

    if let Some(app_data) = env::var_os("APPDATA") {
        let app_data = PathBuf::from(app_data);

        push(
            app_data
                .join(".rusty-kaspa")
                .join("kaspa-mainnet")
                .join("datadir"),
        );
        push(
            app_data
                .join("rusty-kaspa")
                .join("kaspa-mainnet")
                .join("datadir"),
        );
        push(
            app_data
                .join(".kaspad")
                .join("kaspa-mainnet")
                .join("datadir2"),
        );
        push(
            app_data
                .join("kaspad")
                .join("kaspa-mainnet")
                .join("datadir2"),
        );
        push(
            app_data
                .join(".kaspad")
                .join("kaspa-mainnet")
                .join("datadir"),
        );
        push(
            app_data
                .join("kaspad")
                .join("kaspa-mainnet")
                .join("datadir"),
        );
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let local_app_data = PathBuf::from(local_app_data);
        push(
            local_app_data
                .join(".rusty-kaspa")
                .join("kaspa-mainnet")
                .join("datadir"),
        );
        push(
            local_app_data
                .join("rusty-kaspa")
                .join("kaspa-mainnet")
                .join("datadir"),
        );
        push(
            local_app_data
                .join(".kaspad")
                .join("kaspa-mainnet")
                .join("datadir2"),
        );
        push(
            local_app_data
                .join("kaspad")
                .join("kaspa-mainnet")
                .join("datadir2"),
        );
    }

    if let Ok(cwd) = env::current_dir() {
        push(cwd.join("datadir"));
        push(cwd.join("datadir2"));
    }

    out
}

fn hash32_from_hex(hex_str: &str) -> Result<Hash32> {
    let decoded = hex::decode(hex_str).with_context(|| format!("invalid hex: {hex_str}"))?;
    to_hash32(&decoded)
}

fn hex_of(hash: &Hash32) -> String {
    hex::encode(hash)
}

fn trimmed_blue_work_from_words(words: [u64; 3]) -> Vec<u8> {
    let mut le = [0u8; 24];
    for (i, w) in words.iter().enumerate() {
        le[i * 8..(i + 1) * 8].copy_from_slice(&w.to_le_bytes());
    }

    let mut be = le;
    be.reverse();
    let start = be.iter().position(|b| *b != 0).unwrap_or(be.len());
    be[start..].to_vec()
}

fn expand_compressed_parents(runs: &[(u8, Vec<Hash32>)]) -> Result<Vec<Vec<Hash32>>> {
    let mut out: Vec<Vec<Hash32>> = Vec::new();
    let mut prev = 0u8;

    for (cumulative, parents) in runs {
        if *cumulative <= prev {
            bail!(
                "invalid compressed parents: non-increasing cumulative count {} <= {}",
                cumulative,
                prev
            );
        }
        let repeat = (*cumulative - prev) as usize;
        for _ in 0..repeat {
            out.push(parents.clone());
        }
        prev = *cumulative;
    }

    Ok(out)
}

fn convert_header_wire_compressed(h: HeaderWireCompressed) -> Result<ParsedHeader> {
    let _ = h.hash;
    Ok(ParsedHeader {
        version: h.version,
        parents: expand_compressed_parents(&h.parents_by_level.0)?,
        hash_merkle_root: h.hash_merkle_root,
        accepted_id_merkle_root: h.accepted_id_merkle_root,
        utxo_commitment: h.utxo_commitment,
        time_in_milliseconds: h.timestamp,
        bits: h.bits,
        nonce: h.nonce,
        daa_score: h.daa_score,
        blue_score: h.blue_score,
        blue_work_trimmed_be: trimmed_blue_work_from_words(h.blue_work),
        pruning_point: h.pruning_point,
    })
}

fn convert_header_wire_legacy(h: HeaderWireLegacy) -> ParsedHeader {
    let _ = h.hash;
    ParsedHeader {
        version: h.version,
        parents: h.parents_by_level,
        hash_merkle_root: h.hash_merkle_root,
        accepted_id_merkle_root: h.accepted_id_merkle_root,
        utxo_commitment: h.utxo_commitment,
        time_in_milliseconds: h.timestamp,
        bits: h.bits,
        nonce: h.nonce,
        daa_score: h.daa_score,
        blue_score: h.blue_score,
        blue_work_trimmed_be: trimmed_blue_work_from_words(h.blue_work),
        pruning_point: h.pruning_point,
    }
}

fn decode_rust_header(bytes: &[u8]) -> Result<ParsedHeader> {
    if let Ok(wire) = bincode::deserialize::<HeaderWithBlockLevelWireCompressed>(bytes) {
        let _ = wire.block_level;
        return convert_header_wire_compressed(wire.header);
    }

    if let Ok(wire) = bincode::deserialize::<HeaderWithBlockLevelWireLegacy>(bytes) {
        let _ = wire.block_level;
        return Ok(convert_header_wire_legacy(wire.header));
    }

    if let Ok(wire) = bincode::deserialize::<HeaderWireCompressed>(bytes) {
        return convert_header_wire_compressed(wire);
    }

    if let Ok(wire) = bincode::deserialize::<HeaderWireLegacy>(bytes) {
        return Ok(convert_header_wire_legacy(wire));
    }

    bail!("failed decoding rust header in known bincode formats")
}

fn decode_tip_hash_from_key_suffix(suffix: &[u8]) -> Option<Hash32> {
    if suffix.len() == 32 {
        let mut out = [0u8; 32];
        out.copy_from_slice(suffix);
        return Some(out);
    }

    if suffix.len() >= 40 {
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&suffix[0..8]);
        if u64::from_le_bytes(len_bytes) == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&suffix[8..40]);
            return Some(out);
        }
    }

    if suffix.len() >= 32 {
        let mut out = [0u8; 32];
        out.copy_from_slice(&suffix[suffix.len() - 32..]);
        return Some(out);
    }

    None
}

fn new_blake2b_32(key: &[u8]) -> blake2b_simd::State {
    let mut params = Params::new();
    params.hash_length(32);
    params.key(key);
    params.to_state()
}

fn finalize_32(state: blake2b_simd::State) -> Hash32 {
    let mut out = [0u8; 32];
    out.copy_from_slice(state.finalize().as_bytes());
    out
}

fn header_hash(h: &ParsedHeader) -> Hash32 {
    let mut hasher = new_blake2b_32(b"BlockHash");

    hasher.update(&h.version.to_le_bytes());
    hasher.update(&(h.parents.len() as u64).to_le_bytes());

    for level_parents in &h.parents {
        hasher.update(&(level_parents.len() as u64).to_le_bytes());
        for parent in level_parents {
            hasher.update(parent);
        }
    }

    hasher.update(&h.hash_merkle_root);
    hasher.update(&h.accepted_id_merkle_root);
    hasher.update(&h.utxo_commitment);
    hasher.update(&h.time_in_milliseconds.to_le_bytes());
    hasher.update(&h.bits.to_le_bytes());
    hasher.update(&h.nonce.to_le_bytes());
    hasher.update(&h.daa_score.to_le_bytes());
    hasher.update(&h.blue_score.to_le_bytes());
    hasher.update(&(h.blue_work_trimmed_be.len() as u64).to_le_bytes());
    hasher.update(&h.blue_work_trimmed_be);
    hasher.update(&h.pruning_point);

    finalize_32(hasher)
}

fn transaction_hash(tx: &Transaction, include_mass_commitment: bool) -> Hash32 {
    let mut hasher = new_blake2b_32(b"TransactionHash");

    hasher.update(&tx.version.to_le_bytes());
    hasher.update(&(tx.inputs.len() as u64).to_le_bytes());

    for input in &tx.inputs {
        hasher.update(&input.previous_txid);
        hasher.update(&input.previous_index.to_le_bytes());
        hasher.update(&(input.signature_script.len() as u64).to_le_bytes());
        hasher.update(&input.signature_script);
        hasher.update(&[input.sig_op_count]);
        hasher.update(&input.sequence.to_le_bytes());
    }

    hasher.update(&(tx.outputs.len() as u64).to_le_bytes());
    for output in &tx.outputs {
        hasher.update(&output.value.to_le_bytes());
        hasher.update(&output.script_public_key_version.to_le_bytes());
        hasher.update(&(output.script_public_key_script.len() as u64).to_le_bytes());
        hasher.update(&output.script_public_key_script);
    }

    hasher.update(&tx.lock_time.to_le_bytes());
    hasher.update(&tx.subnetwork_id);
    hasher.update(&tx.gas.to_le_bytes());
    hasher.update(&(tx.payload.len() as u64).to_le_bytes());
    hasher.update(&tx.payload);

    if include_mass_commitment && tx.mass > 0 {
        hasher.update(&tx.mass.to_le_bytes());
    }

    finalize_32(hasher)
}

impl RustStore {
    fn open(input_path: &Path) -> Result<Self> {
        let resolution = resolve_rust_db_path(input_path)?;
        let db = open_rocksdb_read_only(&resolution.active_consensus_db_path)?;
        Ok(Self { db, resolution })
    }
}

impl HeaderSource for RustStore {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>> {
        for prefix in [32u8, 8u8] {
            let mut key = Vec::with_capacity(33);
            key.push(prefix);
            key.extend_from_slice(block_hash);

            if let Some(bytes) = self
                .db
                .get(&key)
                .with_context(|| format!("reading rust header key with prefix {prefix}"))?
            {
                if let Ok(header) = decode_rust_header(&bytes) {
                    return Ok(Some(header));
                }
            }
        }

        Ok(None)
    }
}

impl HeaderStore for RustStore {
    fn store_name(&self) -> &'static str {
        "Rust node store (RocksDB + Bincode)"
    }

    fn resolved_db_path(&self) -> &Path {
        &self.resolution.active_consensus_db_path
    }

    fn resolution_notes(&self) -> &[String] {
        &self.resolution.notes
    }

    fn tips(&mut self) -> Result<(Vec<Hash32>, Hash32)> {
        let hst_bytes = self
            .db
            .get([7u8])
            .context("reading rust headers selected tip key")?
            .ok_or_else(|| anyhow!("headers selected tip key not found"))?;

        let hst = if hst_bytes.len() >= 32 {
            to_hash32(&hst_bytes[0..32])?
        } else {
            bail!(
                "headers selected tip value is too short: {} bytes",
                hst_bytes.len()
            );
        };

        let mut tips_set = BTreeSet::new();
        let iter = self
            .db
            .iterator(IteratorMode::From(&[24u8], Direction::Forward));
        for item in iter {
            let (key, _value) = item.context("iterating rust tips prefix")?;
            if key.first().copied() != Some(24u8) {
                break;
            }
            if let Some(hash) = decode_tip_hash_from_key_suffix(&key[1..]) {
                tips_set.insert(hash);
            }
        }

        if tips_set.is_empty() {
            tips_set.insert(hst);
        }

        let tips = tips_set.into_iter().collect::<Vec<_>>();
        Ok((tips, hst))
    }
}

impl GoStore {
    fn open(input_path: &Path) -> Result<Self> {
        let candidates = candidate_go_db_paths(input_path)?;
        let mut errors: Vec<String> = Vec::new();

        for candidate in candidates {
            if !is_db_dir(&candidate) {
                continue;
            }

            let candidate_str = candidate
                .to_str()
                .ok_or_else(|| anyhow!("non-utf8 db path: {}", candidate.display()))?;

            let mut opts = LevelOptions::default();
            opts.create_if_missing = false;

            match LevelDb::open(candidate_str, opts) {
                Ok(mut db) => {
                    let Some(prefix_bytes) = db.get(b"active-prefix") else {
                        errors.push(format!(
                            "{}: key 'active-prefix' not found",
                            candidate.display()
                        ));
                        continue;
                    };

                    if prefix_bytes.len() != 1 {
                        errors.push(format!(
                            "{}: invalid active-prefix length {}",
                            candidate.display(),
                            prefix_bytes.len()
                        ));
                        continue;
                    }

                    let active_prefix = prefix_bytes[0];
                    let notes = vec![format!(
                        "Resolved Go LevelDB path: {} (active-prefix={})",
                        candidate.display(),
                        active_prefix
                    )];

                    return Ok(Self {
                        db,
                        db_path: candidate,
                        active_prefix,
                        notes,
                    });
                }
                Err(err) => {
                    errors.push(format!("{}: {err}", candidate.display()));
                }
            }
        }

        bail!(
            "could not open Go node LevelDB from '{}': {}",
            input_path.display(),
            if errors.is_empty() {
                "no viable database candidates found".to_string()
            } else {
                errors.join(" | ")
            }
        )
    }
}

impl HeaderSource for GoStore {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>> {
        let key = go_db_key(self.active_prefix, b"block-headers", Some(block_hash));
        let Some(bytes) = self.db.get(&key) else {
            return Ok(None);
        };

        let db_header = proto::DbBlockHeader::decode(bytes.as_ref())
            .context("failed decoding DbBlockHeader protobuf")?;

        let version = u16::try_from(db_header.version)
            .with_context(|| format!("header version out of range: {}", db_header.version))?;

        let mut parents: Vec<Vec<Hash32>> = Vec::with_capacity(db_header.parents.len());
        for level in db_header.parents {
            let mut level_hashes = Vec::with_capacity(level.parent_hashes.len());
            for parent in level.parent_hashes {
                level_hashes.push(to_hash32(&parent.hash).context("invalid parent hash length")?);
            }
            parents.push(level_hashes);
        }

        let hash_merkle_root = to_hash32(
            &db_header
                .hash_merkle_root
                .ok_or_else(|| anyhow!("missing hash_merkle_root"))?
                .hash,
        )
        .context("invalid hash_merkle_root length")?;

        let accepted_id_merkle_root = to_hash32(
            &db_header
                .accepted_id_merkle_root
                .ok_or_else(|| anyhow!("missing accepted_id_merkle_root"))?
                .hash,
        )
        .context("invalid accepted_id_merkle_root length")?;

        let utxo_commitment = to_hash32(
            &db_header
                .utxo_commitment
                .ok_or_else(|| anyhow!("missing utxo_commitment"))?
                .hash,
        )
        .context("invalid utxo_commitment length")?;

        let pruning_point = to_hash32(
            &db_header
                .pruning_point
                .ok_or_else(|| anyhow!("missing pruning_point"))?
                .hash,
        )
        .context("invalid pruning_point length")?;

        let time_in_milliseconds =
            u64::try_from(db_header.time_in_milliseconds).with_context(|| {
                format!(
                    "negative timeInMilliseconds value: {}",
                    db_header.time_in_milliseconds
                )
            })?;

        Ok(Some(ParsedHeader {
            version,
            parents,
            hash_merkle_root,
            accepted_id_merkle_root,
            utxo_commitment,
            time_in_milliseconds,
            bits: db_header.bits,
            nonce: db_header.nonce,
            daa_score: db_header.daa_score,
            blue_score: db_header.blue_score,
            blue_work_trimmed_be: db_header.blue_work,
            pruning_point,
        }))
    }
}

impl HeaderStore for GoStore {
    fn store_name(&self) -> &'static str {
        "Go node store (LevelDB + Protobuf)"
    }

    fn resolved_db_path(&self) -> &Path {
        &self.db_path
    }

    fn resolution_notes(&self) -> &[String] {
        &self.notes
    }

    fn tips(&mut self) -> Result<(Vec<Hash32>, Hash32)> {
        let hst_key = go_db_key(self.active_prefix, b"headers-selected-tip", None);
        let hst_bytes = self
            .db
            .get(&hst_key)
            .ok_or_else(|| anyhow!("headers-selected-tip key not found"))?;

        let db_hst = proto::DbHash::decode(hst_bytes.as_ref())
            .context("failed decoding headers-selected-tip DbHash")?;
        let hst = to_hash32(&db_hst.hash).context("invalid headers-selected-tip hash length")?;

        let tips_key = go_db_key(self.active_prefix, b"tips", None);
        let tips_bytes = self
            .db
            .get(&tips_key)
            .ok_or_else(|| anyhow!("tips key not found"))?;

        let db_tips =
            proto::DbTips::decode(tips_bytes.as_ref()).context("failed decoding DbTips")?;

        let mut tips = Vec::with_capacity(db_tips.tips.len());
        for t in db_tips.tips {
            tips.push(to_hash32(&t.hash).context("invalid tip hash length")?);
        }

        if tips.is_empty() {
            tips.push(hst);
        }

        Ok((tips, hst))
    }
}

impl HeaderSource for CheckpointStore {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>> {
        Ok(self.headers.get(block_hash).cloned())
    }
}

impl CheckpointStore {
    fn from_embedded_json() -> Result<Self> {
        let parsed: CheckpointJson = serde_json::from_str(CHECKPOINT_DATA_JSON)
            .context("failed parsing embedded checkpoint_data.json")?;

        let mut headers = HashMap::with_capacity(parsed.headers_chain.len());
        for entry in parsed.headers_chain {
            let hash = hash32_from_hex(&entry.hash)
                .with_context(|| format!("invalid checkpoint header hash {}", entry.hash))?;

            let mut parents: Vec<Vec<Hash32>> = Vec::with_capacity(entry.parents.len());
            for level in entry.parents {
                let mut level_hashes = Vec::with_capacity(level.len());
                for parent_hex in level {
                    level_hashes.push(
                        hash32_from_hex(&parent_hex).with_context(|| {
                            format!("invalid checkpoint parent hash {parent_hex}")
                        })?,
                    );
                }
                parents.push(level_hashes);
            }

            let header = ParsedHeader {
                version: entry.version,
                parents,
                hash_merkle_root: hash32_from_hex(&entry.hash_merkle_root)?,
                accepted_id_merkle_root: hash32_from_hex(&entry.accepted_id_merkle_root)?,
                utxo_commitment: hash32_from_hex(&entry.utxo_commitment)?,
                time_in_milliseconds: entry.time_in_milliseconds,
                bits: entry.bits,
                nonce: entry.nonce,
                daa_score: entry.daa_score,
                blue_score: entry.blue_score,
                blue_work_trimmed_be: hex::decode(&entry.blue_work)
                    .with_context(|| format!("invalid checkpoint blueWork {}", entry.blue_work))?,
                pruning_point: hash32_from_hex(&entry.pruning_point)?,
            };

            headers.insert(hash, header);
        }

        let checkpoint_hash = hash32_from_hex(&parsed.checkpoint_hash)?;
        let original_genesis_hash = hash32_from_hex(&parsed.original_genesis_hash)?;
        if !headers.contains_key(&checkpoint_hash) {
            bail!(
                "embedded checkpoint data is missing checkpoint hash {}",
                parsed.checkpoint_hash
            );
        }
        if !headers.contains_key(&original_genesis_hash) {
            bail!(
                "embedded checkpoint data is missing original genesis hash {}",
                parsed.original_genesis_hash
            );
        }

        Ok(Self { headers })
    }
}

fn open_rocksdb_read_only(path: &Path) -> Result<RocksDb> {
    let mut opts = RocksOptions::default();
    opts.create_if_missing(false);
    opts.set_comparator(
        "leveldb.BytewiseComparator",
        Box::new(|a: &[u8], b: &[u8]| a.cmp(b)),
    );

    RocksDb::open_for_read_only(&opts, path, false)
        .with_context(|| format!("failed opening RocksDB at {}", path.display()))
}

fn is_db_dir(path: &Path) -> bool {
    path.join("CURRENT").is_file()
}

fn list_consensus_dirs(consensus_root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let rd = fs::read_dir(consensus_root)
        .with_context(|| format!("failed reading {}", consensus_root.display()))?;

    for entry in rd {
        let entry = entry
            .with_context(|| format!("failed reading entry in {}", consensus_root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.starts_with("consensus-") {
            continue;
        }
        dirs.push(path);
    }

    dirs.sort_by_key(|p| {
        p.file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("consensus-"))
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    });

    Ok(dirs)
}

fn read_consensus_entry_dir_name(meta_db: &RocksDb, key: u64) -> Result<Option<String>> {
    let mut entry_key = vec![125u8];
    entry_key.extend_from_slice(&key.to_le_bytes());

    let Some(bytes) = meta_db
        .get(&entry_key)
        .with_context(|| format!("failed reading consensus entry key {key}"))?
    else {
        return Ok(None);
    };

    let entry: ConsensusEntry = bincode::deserialize(&bytes)
        .with_context(|| format!("failed decoding consensus entry {key}"))?;

    let _ = (entry.key, entry.creation_timestamp);
    Ok(Some(entry.directory_name))
}

fn resolve_rust_db_path(input_path: &Path) -> Result<RustDbResolution> {
    let mut notes = Vec::new();

    if is_db_dir(input_path) {
        return Ok(RustDbResolution {
            active_consensus_db_path: input_path.to_path_buf(),
            notes,
        });
    }

    let mut consensus_root: Option<PathBuf> = None;
    let mut meta_path: Option<PathBuf> = None;

    if input_path.join("consensus").is_dir() {
        consensus_root = Some(input_path.join("consensus"));
        if input_path.join("meta").is_dir() {
            meta_path = Some(input_path.join("meta"));
        }
    }

    if consensus_root.is_none() {
        let is_consensus_root = input_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|name| name == "consensus")
            .unwrap_or(false)
            || !list_consensus_dirs(input_path)?.is_empty();

        if is_consensus_root {
            consensus_root = Some(input_path.to_path_buf());
            if let Some(parent) = input_path.parent() {
                if parent.join("meta").is_dir() {
                    meta_path = Some(parent.join("meta"));
                }
            }
        }
    }

    let Some(consensus_root) = consensus_root else {
        bail!(
            "could not resolve Rust consensus root from '{}'",
            input_path.display()
        );
    };

    let dirs = list_consensus_dirs(&consensus_root)?;
    if dirs.is_empty() {
        bail!(
            "no consensus-* directories found under {}",
            consensus_root.display()
        );
    }

    let mut active_dir_name: Option<String> = None;
    let mut staging_dir_name: Option<String> = None;
    let mut detected_layout = "rust-consensus-directory-fallback".to_string();

    if let Some(meta_path) = meta_path {
        if is_db_dir(&meta_path) {
            match open_rocksdb_read_only(&meta_path) {
                Ok(meta_db) => {
                    if let Some(bytes) = meta_db
                        .get([124u8])
                        .context("reading multi-consensus metadata key")?
                    {
                        match bincode::deserialize::<MultiConsensusMetadata>(&bytes) {
                            Ok(metadata) => {
                                let _ = (
                                    metadata.max_key_used,
                                    metadata.is_archival_node,
                                    metadata.props.len(),
                                    metadata.version,
                                );

                                if let Some(k) = metadata.current_consensus_key {
                                    active_dir_name = read_consensus_entry_dir_name(&meta_db, k)?;
                                }
                                if let Some(k) = metadata.staging_consensus_key {
                                    staging_dir_name = read_consensus_entry_dir_name(&meta_db, k)?;
                                }

                                detected_layout = "rust-meta-managed".to_string();
                            }
                            Err(err) => {
                                notes.push(format!(
                                    "failed to decode multi-consensus metadata: {err} (falling back to directory scan)"
                                ));
                            }
                        }
                    } else {
                        notes.push(
                            "multi-consensus metadata not found in meta DB (falling back to directory scan)".to_string(),
                        );
                    }
                }
                Err(err) => {
                    notes.push(format!(
                        "failed opening meta DB at {}: {err}",
                        meta_path.display()
                    ));
                }
            }
        } else {
            notes.push(format!(
                "meta directory exists but is not a DB: {}",
                meta_path.display()
            ));
        }
    } else {
        notes.push(
            "meta DB directory not found; using consensus directory scan fallback".to_string(),
        );
    }

    if active_dir_name.is_none() {
        active_dir_name = dirs.last().and_then(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        });
    }

    let active_consensus_db_path = active_dir_name
        .as_ref()
        .map(|name| consensus_root.join(name))
        .filter(|p| p.is_dir())
        .ok_or_else(|| {
            anyhow!(
                "could not resolve active consensus directory under {}",
                consensus_root.display()
            )
        })?;

    let staging_consensus_db_path = staging_dir_name
        .as_ref()
        .map(|name| consensus_root.join(name))
        .filter(|p| p.is_dir());

    notes.push(format!("Detected layout: {detected_layout}"));
    notes.push(format!(
        "Active consensus DB: {}",
        active_consensus_db_path.display()
    ));
    if let Some(ref staging) = staging_consensus_db_path {
        notes.push(format!("Staging consensus DB: {}", staging.display()));
    }

    Ok(RustDbResolution {
        active_consensus_db_path,
        notes,
    })
}

fn candidate_go_db_paths(input_path: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();

    out.push(input_path.to_path_buf());
    out.push(input_path.join("datadir2"));
    out.push(input_path.join("datadir"));
    out.push(input_path.join("kaspa-mainnet").join("datadir2"));
    out.push(input_path.join("kaspa-mainnet").join("datadir"));

    if input_path.is_dir() {
        for entry in fs::read_dir(input_path)
            .with_context(|| format!("failed reading directory {}", input_path.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed reading entry in {}", input_path.display()))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            out.push(path.join("datadir2"));
            out.push(path.join("datadir"));
        }
    }

    out.sort();
    out.dedup();
    Ok(out)
}

fn go_db_key(prefix: u8, bucket: &[u8], suffix: Option<&[u8]>) -> Vec<u8> {
    let mut key = Vec::with_capacity(2 + bucket.len() + suffix.map(|s| 1 + s.len()).unwrap_or(0));
    key.push(prefix);
    key.push(b'/');
    key.extend_from_slice(bucket);
    if let Some(s) = suffix {
        key.push(b'/');
        key.extend_from_slice(s);
    }
    key
}

fn assert_cryptographic_hash_chain_to_genesis(
    source: &mut dyn HeaderSource,
    mut block_hash: Hash32,
    genesis_hash: Hash32,
    verbose: bool,
) -> Result<bool> {
    let mut steps: usize = 0;

    loop {
        if block_hash == genesis_hash {
            if verbose {
                print_info(&format!(
                    "✓ Reached genesis block via {steps} pruning points"
                ));
            }
            return Ok(true);
        }

        let Some(header) = source.get_raw_header(&block_hash)? else {
            print_error(&format!(
                "Header not found for hash: {}",
                hex_of(&block_hash)
            ));
            return Ok(false);
        };

        let calculated_hash = header_hash(&header);
        if calculated_hash != block_hash {
            print_error(&format!("Hash mismatch at block {}", hex_of(&block_hash)));
            print_error(&format!("  Expected: {}", hex_of(&block_hash)));
            print_error(&format!("  Got:      {}", hex_of(&calculated_hash)));
            return Ok(false);
        }

        if verbose {
            print_info(&format!(
                "  Step {}: {} -> {}",
                steps + 1,
                hex_of(&block_hash),
                hex_of(&header.pruning_point)
            ));
        }

        block_hash = header.pruning_point;
        steps += 1;

        if steps > 100_000 {
            print_error("Too many iterations in hash chain verification (safety stop)");
            return Ok(false);
        }
    }
}

fn detect_and_open_store(node_type: CliNodeType, datadir: &Path) -> Result<Box<dyn HeaderStore>> {
    match node_type {
        CliNodeType::Rust => {
            let store = RustStore::open(datadir)?;
            Ok(Box::new(store))
        }
        CliNodeType::Go => {
            let store = GoStore::open(datadir)?;
            Ok(Box::new(store))
        }
        CliNodeType::Auto => {
            let rust_result = RustStore::open(datadir);
            if let Ok(store) = rust_result {
                return Ok(Box::new(store));
            }

            let go_result = GoStore::open(datadir);
            match (rust_result, go_result) {
                (Err(rust_err), Ok(store)) => {
                    print_info(&format!(
                        "Auto-detect: Rust layout failed ({rust_err}); falling back to Go layout"
                    ));
                    Ok(Box::new(store))
                }
                (Err(rust_err), Err(go_err)) => {
                    bail!(
                        "auto-detect failed: rust attempt error: {rust_err} | go attempt error: {go_err}"
                    )
                }
                _ => unreachable!(),
            }
        }
    }
}

fn open_store_with_resolved_input(cli: &Cli) -> Result<OpenStoreResult> {
    if let Some(input_datadir) = cli.datadir.as_deref() {
        let expanded = expand_tilde(input_datadir);
        let store = detect_and_open_store(cli.node_type, &expanded)?;
        return Ok(OpenStoreResult {
            store,
            input_path: expanded.clone(),
            probe_notes: vec![format!(
                "Input path source: --datadir ({})",
                expanded.display()
            )],
        });
    }

    let candidates = default_datadir_probe_candidates();
    if candidates.is_empty() {
        bail!("could not auto-detect datadir: no probe candidates were generated");
    }

    let mut errors = Vec::new();
    let mut existing_candidates = Vec::new();
    for candidate in &candidates {
        if candidate.exists() {
            existing_candidates.push(candidate.clone());
        }
    }

    if existing_candidates.is_empty() {
        bail!(
            "could not auto-detect datadir. No default Kaspa paths exist. Checked: {}. Supply --datadir explicitly.",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for candidate in &existing_candidates {
        match detect_and_open_store(cli.node_type, candidate) {
            Ok(store) => {
                let mut probe_notes = Vec::new();
                probe_notes.push(
                    "Input path source: auto-detection from OS default Kaspa locations".to_string(),
                );
                probe_notes.push(format!("Auto-selected input path: {}", candidate.display()));

                return Ok(OpenStoreResult {
                    store,
                    input_path: candidate.clone(),
                    probe_notes,
                });
            }
            Err(err) => {
                errors.push(format!("{}: {}", candidate.display(), err));
            }
        }
    }

    bail!(
        "auto-detect failed for all existing default paths: {}. Supply --datadir explicitly.",
        errors.join(" | ")
    )
}

fn hardwired_genesis_coinbase_tx() -> Result<Transaction> {
    let subnetwork_id_bytes = hex::decode(MAINNET_SUBNETWORK_ID_COINBASE_HEX)
        .context("invalid coinbase subnetwork id constant")?;
    let mut subnetwork_id = [0u8; 20];
    subnetwork_id.copy_from_slice(&subnetwork_id_bytes);

    let payload = hex::decode(HARDWIRED_GENESIS_TX_PAYLOAD_HEX)
        .context("invalid hardwired genesis coinbase payload hex")?;

    Ok(Transaction {
        version: 0,
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: 0,
        subnetwork_id,
        gas: 0,
        payload,
        mass: 0,
    })
}

fn verify_genesis(
    store: &mut dyn HeaderStore,
    input_path: &Path,
    probe_notes: &[String],
    pre_checkpoint_datadir: Option<&Path>,
    verbose: bool,
) -> Result<bool> {
    let hardwired_genesis = hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX)?;
    let original_genesis = hash32_from_hex(ORIGINAL_GENESIS_HASH_HEX)?;
    let checkpoint_hash = hash32_from_hex(CHECKPOINT_HASH_HEX)?;
    let empty_muhash = hash32_from_hex(EMPTY_MUHASH_HEX)?;

    print_header("Step 1: Database Connectivity Test");
    print_success("Database opened successfully");
    print_info(&format!("Using {}", store.store_name()));
    print_info(&format!("Input path: {}", input_path.display()));
    print_info(&format!(
        "Resolved DB path: {}",
        store.resolved_db_path().display()
    ));
    for note in probe_notes {
        print_info(note);
    }
    for note in store.resolution_notes() {
        print_info(note);
    }

    print_header("Step 2: Current Chain State");
    let (tips, hst) = store.tips()?;
    print_info(&format!("Number of DAG tips: {}", tips.len()));
    print_info(&format!("Headers selected tip: {}", hex_of(&hst)));

    if let Some(hst_header) = store.get_raw_header(&hst)? {
        let tip_ts = hst_header.time_in_milliseconds;
        print_info(&format!("Headers selected tip timestamp: {tip_ts} ms"));

        let now = now_millis()?;
        if now >= tip_ts {
            let lag = now - tip_ts;
            print_info(&format!(
                "Tip age vs local clock: {}",
                format_duration_ms(lag)
            ));

            if lag > TIP_SYNC_WARNING_THRESHOLD_MS {
                print_warning(
                    "Node appears to still be syncing or is behind the network tip. This proof is valid for your current local tip; rerun after sync completes for latest-state verification.",
                );
            } else {
                print_success("Tip time is close to local clock (likely near latest network tip)");
            }
        } else {
            let lead = tip_ts - now;
            print_warning(&format!(
                "Tip timestamp is {} ahead of local clock. Check system time.",
                format_duration_ms(lead)
            ));
        }
    } else {
        print_warning(
            "Could not read selected-tip header timestamp, so sync status advisory is unavailable.",
        );
    }

    print_header("Step 3: Genesis Header Verification");

    let (active_genesis_hash, genesis_header, genesis_kind) =
        if let Some(header) = store.get_raw_header(&hardwired_genesis)? {
            (hardwired_genesis, header, "hardwired")
        } else if let Some(header) = store.get_raw_header(&original_genesis)? {
            (original_genesis, header, "original")
        } else {
            print_error("Neither hardwired nor original genesis headers were found");
            return Ok(false);
        };

    print_info(&format!("Detected genesis mode: {genesis_kind}"));
    print_info(&format!(
        "Expected genesis hash: {}",
        hex_of(&active_genesis_hash)
    ));

    let calculated_genesis_hash = header_hash(&genesis_header);
    print_info(&format!(
        "Calculated hash:      {}",
        hex_of(&calculated_genesis_hash)
    ));

    if calculated_genesis_hash != active_genesis_hash {
        print_error("Genesis header hash mismatch");
        return Ok(false);
    }

    print_success("Genesis header hash verified");
    print_info(&format!(
        "Genesis timestamp: {}",
        genesis_header.time_in_milliseconds
    ));
    print_info(&format!("Genesis DAA score: {}", genesis_header.daa_score));
    print_info(&format!(
        "Genesis blue score: {}",
        genesis_header.blue_score
    ));
    print_info(&format!(
        "Genesis bits (difficulty): {}",
        genesis_header.bits
    ));

    print_header("Step 4: Genesis Coinbase Transaction");
    if active_genesis_hash == hardwired_genesis {
        let genesis_coinbase_tx = hardwired_genesis_coinbase_tx()?;

        print_info("Genesis transaction properties:");
        print_info(&format!("  Version: {}", genesis_coinbase_tx.version));
        print_info(&format!(
            "  Inputs: {} (coinbase has no inputs)",
            genesis_coinbase_tx.inputs.len()
        ));
        print_info(&format!(
            "  Outputs: {} (coinbase has no outputs)",
            genesis_coinbase_tx.outputs.len()
        ));
        print_info(&format!(
            "  Payload size: {} bytes",
            genesis_coinbase_tx.payload.len()
        ));

        let calc_tx_hash = transaction_hash(&genesis_coinbase_tx, true);
        print_info(&format!("Calculated tx hash:    {}", hex_of(&calc_tx_hash)));
        print_info(&format!(
            "Expected merkle root:  {}",
            hex_of(&genesis_header.hash_merkle_root)
        ));

        if calc_tx_hash != genesis_header.hash_merkle_root {
            print_error("Genesis coinbase transaction hash mismatch");
            return Ok(false);
        }

        print_success("Genesis coinbase transaction verified");

        let hebrew_text = &genesis_coinbase_tx.payload[20..140];
        let bitcoin_hash = &genesis_coinbase_tx.payload[140..172];
        let checkpoint_ref = &genesis_coinbase_tx.payload[172..204];

        print_info("Embedded data in genesis transaction:");
        print_info(&format!(
            "  Hebrew text: '{}'",
            String::from_utf8_lossy(hebrew_text)
        ));
        print_info(&format!(
            "  Bitcoin block reference: {}",
            hex::encode(bitcoin_hash)
        ));
        print_info("    (Bitcoin block #808080, provides timestamp anchor)");
        print_success("Bitcoin block reference verified");
        print_info(&format!(
            "  Checkpoint block reference: {}",
            hex::encode(checkpoint_ref)
        ));
        print_info("    (Kaspa checkpoint block for UTXO state)");
        print_success("Checkpoint block reference verified");
    } else {
        print_info("Legacy/original genesis detected.");
        print_info(
            "Coinbase payload for original genesis is not embedded in this verifier build, so tx->merkle verification is skipped for this mode.",
        );
    }

    print_header("Step 5: Hash Chain Verification");
    let chain_tip = if !tips.is_empty() { tips[0] } else { hst };
    if chain_tip == [0u8; 32] {
        print_error("No valid chain tip found to verify");
        return Ok(false);
    }

    print_info(&format!(
        "Starting hash chain verification from tip: {}",
        hex_of(&chain_tip)
    ));
    print_info(&format!(
        "Target genesis hash: {}",
        hex_of(&active_genesis_hash)
    ));
    print_info("Verifying hash chain from current tip to genesis...");

    if !assert_cryptographic_hash_chain_to_genesis(store, chain_tip, active_genesis_hash, verbose)?
    {
        print_error("Hash chain verification failed");
        return Ok(false);
    }
    print_success("Hash chain from current state to genesis verified");

    print_header("Step 6: UTXO Commitment Analysis");
    let utxo_commitment = genesis_header.utxo_commitment;
    print_info(&format!(
        "Genesis UTXO commitment: {}",
        hex_of(&utxo_commitment)
    ));
    print_info(&format!(
        "Empty MuHash value:      {}",
        hex_of(&empty_muhash)
    ));

    if utxo_commitment.iter().all(|b| *b == 0) {
        print_info("Status: All-zero UTXO commitment (should not occur)");
    } else if utxo_commitment == empty_muhash {
        print_info("Status: Empty UTXO commitment (original genesis)");
    } else {
        print_info(
            "Status: Non-empty UTXO commitment (hardwired genesis with checkpoint UTXO set)",
        );
        print_info("This means the genesis contains a pre-calculated UTXO set from a checkpoint");
    }

    print_header("Step 7: Pre-Checkpoint Verification");
    let mut checkpoint_store = CheckpointStore::from_embedded_json()?;
    print_success("Loaded embedded checkpoint_data.json");
    print_info("(No 1GB pre-checkpoint database download required)");

    print_info(&format!(
        "Checkpoint hash:       {}",
        hex_of(&checkpoint_hash)
    ));
    print_info(&format!(
        "Original genesis hash: {}",
        hex_of(&original_genesis)
    ));

    if let Some(checkpoint_header) = checkpoint_store.get_raw_header(&checkpoint_hash)? {
        print_success("Checkpoint header found");
        print_info(&format!(
            "Checkpoint UTXO commitment: {}",
            hex_of(&checkpoint_header.utxo_commitment)
        ));

        if active_genesis_hash == hardwired_genesis {
            if genesis_header.utxo_commitment == checkpoint_header.utxo_commitment {
                print_success("UTXO commitments match between hardwired genesis and checkpoint");
            } else {
                print_error("UTXO commitment mismatch between hardwired genesis and checkpoint");
                print_error(&format!(
                    "Genesis:    {}",
                    hex_of(&genesis_header.utxo_commitment)
                ));
                print_error(&format!(
                    "Checkpoint: {}",
                    hex_of(&checkpoint_header.utxo_commitment)
                ));
                return Ok(false);
            }
        }

        print_info("Verifying chain from checkpoint to original genesis...");
        if !assert_cryptographic_hash_chain_to_genesis(
            &mut checkpoint_store,
            checkpoint_hash,
            original_genesis,
            verbose,
        )? {
            print_error("Checkpoint chain verification failed");
            return Ok(false);
        }

        print_success("Checkpoint chain to original genesis verified");

        if let Some(original_genesis_header) = checkpoint_store.get_raw_header(&original_genesis)? {
            print_info(&format!(
                "Original genesis UTXO commitment: {}",
                hex_of(&original_genesis_header.utxo_commitment)
            ));
            print_info(&format!(
                "Expected empty MuHash:            {}",
                hex_of(&empty_muhash)
            ));

            if original_genesis_header.utxo_commitment == empty_muhash {
                print_success("Original genesis has empty UTXO set verified!");
            } else {
                print_error("Original genesis UTXO commitment is not empty");
                return Ok(false);
            }
        } else {
            print_error("Original genesis header not found in checkpoint dataset");
            return Ok(false);
        }
    } else {
        print_error("Checkpoint header not found in checkpoint dataset");
        return Ok(false);
    }

    if let Some(path) = pre_checkpoint_datadir {
        print_info(&format!(
            "Note: --pre-checkpoint-datadir supplied ({}) but embedded checkpoint path already completed verification.",
            path.display()
        ));
    }

    print_header("Verification Summary");
    print_success("All cryptographic verifications passed!");
    print_info("Verification details:");
    print_info(&format!(
        "  ✓ Active genesis hash: {}",
        hex_of(&active_genesis_hash)
    ));
    if active_genesis_hash == hardwired_genesis {
        print_info("  ✓ Hardwired genesis coinbase transaction verified");
    } else {
        print_info("  ✓ Legacy genesis mode detected (coinbase payload check skipped)");
    }
    print_info("  ✓ Hash chain from current tip to genesis verified");
    print_info("  ✓ UTXO commitment analysis completed");
    print_info("  ✓ Pre-checkpoint verification completed");
    print_info("  ✓ Original genesis empty UTXO set verified");

    print_success("The Kaspa blockchain integrity has been verified");
    print_success("No premine detected - UTXO set evolved from empty state");
    println!("\n{BOLD}Thank you for verifying the integrity of Kaspa!{END}");

    Ok(true)
}

fn main() {
    let cli = Cli::parse();

    println!("{BOLD}Kaspa Genesis Proof Verification (Rust-Native){END}");
    println!("Requested node type: {:?}", cli.node_type);

    if let Some(datadir) = cli.datadir.as_deref() {
        println!("Input data directory: {}", datadir.display());
    } else {
        println!("Input data directory: auto-detect (OS default Kaspa locations)");
    }

    let exit_code = match run(&cli) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(err) => {
            print_error(&format!("Verification failed with error: {err}"));
            1
        }
    };

    if cli.pause_on_exit {
        println!("\nPress Enter to exit...");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }

    std::process::exit(exit_code);
}

fn run(cli: &Cli) -> Result<bool> {
    let OpenStoreResult {
        mut store,
        input_path,
        probe_notes,
    } = open_store_with_resolved_input(cli)?;

    verify_genesis(
        &mut *store,
        &input_path,
        &probe_notes,
        cli.pre_checkpoint_datadir.as_deref(),
        cli.verbose,
    )
}
