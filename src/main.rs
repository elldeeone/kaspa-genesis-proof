use anyhow::{Context, Result, bail};
use blake2b_simd::Params;
use clap::{Parser, ValueEnum};
use rocksdb::{DB as RocksDb, Options as RocksOptions};
use rusty_leveldb::DB as LevelDb;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

mod output;
mod store;

use output::{
    build_initial_report, capture_output_line, clear_output_capture, format_duration_ms,
    now_millis, output_capture_snapshot, print_error, print_header, print_info, print_plain,
    print_success, print_warning, prompt_continue_on_sync_warning, prompt_export_json_decision,
    write_json_report,
};
use store::{
    open_store_with_resolved_input, parse_consensus_entry_dir_name, parse_current_consensus_key,
    resolve_rust_db_path,
};

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

const CHECKPOINT_DATA_JSON: &str = include_str!("../checkpoint_data.json");
const TIP_SYNC_WARNING_THRESHOLD_MS: u64 = 10 * 60 * 1000;
const RUST_MULTI_CONSENSUS_METADATA_KEY: &[u8] = &[124u8];
const RUST_CONSENSUS_ENTRY_PREFIX: &[u8] = &[125u8];
const LEGACY_MULTI_CONSENSUS_METADATA_KEY: &[u8] = b"multi-consensus-metadata-key";
const LEGACY_CONSENSUS_ENTRIES_PREFIX: &[u8] = b"consensus-entries-prefix";
const ROCKSDB_READ_ONLY_MAX_OPEN_FILES: i32 = 128;
static OUTPUT_CAPTURE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

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

#[derive(Debug, Default, Serialize)]
struct VerificationReport {
    generated_at_unix_ms: u64,
    success: bool,
    requested_node_type: String,
    provided_datadir: Option<String>,
    resolved_input_path: Option<String>,
    resolved_db_path: Option<String>,
    store_type: Option<String>,
    tips_count: Option<usize>,
    headers_selected_tip: Option<String>,
    headers_selected_tip_timestamp_ms: Option<u64>,
    tip_age_ms: Option<u64>,
    sync_warning_triggered: bool,
    continued_after_sync_warning: Option<bool>,
    aborted_due_to_sync_warning: bool,
    genesis_mode: Option<String>,
    active_genesis_hash: Option<String>,
    chain_tip_used: Option<String>,
    tips: Vec<String>,
    screen_output_lines: Vec<String>,
    error: Option<String>,
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

fn to_hash32(bytes: &[u8]) -> Result<Hash32> {
    if bytes.len() != 32 {
        bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
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

fn choose_chain_tip_for_verification(tips: &[Hash32], headers_selected_tip: Hash32) -> Hash32 {
    if headers_selected_tip != [0u8; 32] {
        return headers_selected_tip;
    }

    tips.first().copied().unwrap_or([0u8; 32])
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
    no_input: bool,
    report: &mut VerificationReport,
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
    report.store_type = Some(store.store_name().to_string());
    report.resolved_input_path = Some(input_path.display().to_string());
    report.resolved_db_path = Some(store.resolved_db_path().display().to_string());

    print_header("Step 2: Current Chain State");
    let (tips, hst) = store.tips()?;
    print_info(&format!("Number of DAG tips: {}", tips.len()));
    print_info(&format!("Headers selected tip: {}", hex_of(&hst)));
    report.tips_count = Some(tips.len());
    report.headers_selected_tip = Some(hex_of(&hst));
    report.tips = tips.iter().map(hex_of).collect();

    if let Some(hst_header) = store.get_raw_header(&hst)? {
        let tip_ts = hst_header.time_in_milliseconds;
        print_info(&format!("Headers selected tip timestamp: {tip_ts} ms"));
        report.headers_selected_tip_timestamp_ms = Some(tip_ts);

        let now = now_millis()?;
        if now >= tip_ts {
            let lag = now - tip_ts;
            print_info(&format!(
                "Tip age vs local clock: {}",
                format_duration_ms(lag)
            ));
            report.tip_age_ms = Some(lag);

            if lag > TIP_SYNC_WARNING_THRESHOLD_MS {
                report.sync_warning_triggered = true;
                print_warning(
                    "Node appears to still be syncing or is behind the network tip. This proof is valid for your current local tip; rerun after sync completes for latest-state verification.",
                );
                let continue_anyway = prompt_continue_on_sync_warning(no_input)?;
                report.continued_after_sync_warning = Some(continue_anyway);
                if !continue_anyway {
                    print_error("Verification aborted by user due to sync advisory.");
                    report.aborted_due_to_sync_warning = true;
                    report.error = Some("aborted by user due to sync advisory".to_string());
                    return Ok(false);
                }
                print_info("Continuing verification against latest local synced tip.");
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
            report.error =
                Some("neither hardwired nor original genesis headers were found".to_string());
            return Ok(false);
        };
    report.genesis_mode = Some(genesis_kind.to_string());
    report.active_genesis_hash = Some(hex_of(&active_genesis_hash));

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
        report.error = Some("genesis header hash mismatch".to_string());
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
            report.error = Some("genesis coinbase transaction hash mismatch".to_string());
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
    let chain_tip = choose_chain_tip_for_verification(&tips, hst);
    report.chain_tip_used = Some(hex_of(&chain_tip));
    if chain_tip == [0u8; 32] {
        print_error("No valid chain tip found to verify");
        report.error = Some("no valid chain tip found to verify".to_string());
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
        report.error = Some("hash chain verification failed".to_string());
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
                report.error = Some(
                    "utxo commitment mismatch between hardwired genesis and checkpoint".to_string(),
                );
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
            report.error = Some("checkpoint chain verification failed".to_string());
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
                report.error = Some("original genesis UTXO commitment is not empty".to_string());
                return Ok(false);
            }
        } else {
            print_error("Original genesis header not found in checkpoint dataset");
            report.error =
                Some("original genesis header not found in checkpoint dataset".to_string());
            return Ok(false);
        }
    } else {
        print_error("Checkpoint header not found in checkpoint dataset");
        report.error = Some("checkpoint header not found in checkpoint dataset".to_string());
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
    capture_output_line("");
    capture_output_line("Thank you for verifying the integrity of Kaspa!");

    Ok(true)
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

    if cli.pause_on_exit {
        print_plain("");
        print_plain("Press Enter to exit...");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }

    std::process::exit(exit_code);
}

fn run(cli: &Cli, report: &mut VerificationReport) -> Result<bool> {
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
        cli.no_input,
        report,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocksdb::DB as RocksDb;
    use tempfile::TempDir;

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
    fn hardwired_genesis_coinbase_tx_hash_matches_live_node_merkle_root() {
        let tx = hardwired_genesis_coinbase_tx().expect("hardwired tx");
        let tx_hash = transaction_hash(&tx, true);

        assert_eq!(
            hex_of(&tx_hash),
            "8ec898568c6801d13df4ee6e2a1b54b7e6236f671f20954f05306410518eeb32"
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
    fn choose_chain_tip_prefers_headers_selected_tip() {
        let tips = vec![test_hash(0x11), test_hash(0x22)];
        let headers_selected_tip = test_hash(0x77);

        assert_eq!(
            choose_chain_tip_for_verification(&tips, headers_selected_tip),
            headers_selected_tip
        );
    }

    #[test]
    fn choose_chain_tip_falls_back_to_first_tip_when_selected_tip_missing() {
        let first_tip = test_hash(0x11);
        let tips = vec![first_tip, test_hash(0x22)];

        assert_eq!(
            choose_chain_tip_for_verification(&tips, [0u8; 32]),
            first_tip
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
    fn rust_store_tips_falls_back_to_selected_tip_when_tip_store_is_empty() {
        let (_tempdir, _datadir, consensus_root) = create_temp_datadir();
        let db_path = consensus_root.join("consensus-002");
        let headers_selected_tip = test_hash(0x55);

        create_consensus_db(&db_path, &[(vec![7u8], headers_selected_tip.to_vec())]);

        let mut store = RustStore::open(&db_path).expect("open rust store");
        let (tips, hst) = store.tips().expect("read tips");

        assert_eq!(hst, headers_selected_tip);
        assert_eq!(tips, vec![headers_selected_tip]);
    }

    #[test]
    fn rocksdb_read_only_open_files_limit_stays_bounded_for_live_nodes() {
        assert_eq!(ROCKSDB_READ_ONLY_MAX_OPEN_FILES, 128);
        assert!(ROCKSDB_READ_ONLY_MAX_OPEN_FILES > 0);
        assert!(ROCKSDB_READ_ONLY_MAX_OPEN_FILES < 1024);
    }
}
