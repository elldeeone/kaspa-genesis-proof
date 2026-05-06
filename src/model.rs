use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(crate) type Hash32 = [u8; 32];

#[derive(Clone, Debug)]
pub(crate) struct ParsedHeader {
    pub(crate) version: u16,
    pub(crate) parents: Vec<Vec<Hash32>>,
    pub(crate) hash_merkle_root: Hash32,
    pub(crate) accepted_id_merkle_root: Hash32,
    pub(crate) utxo_commitment: Hash32,
    pub(crate) time_in_milliseconds: u64,
    pub(crate) bits: u32,
    pub(crate) nonce: u64,
    pub(crate) daa_score: u64,
    pub(crate) blue_score: u64,
    pub(crate) blue_work_trimmed_be: Vec<u8>,
    pub(crate) pruning_point: Hash32,
}

#[derive(Clone, Debug)]
pub(crate) struct Transaction {
    pub(crate) version: u16,
    pub(crate) inputs: Vec<TransactionInput>,
    pub(crate) outputs: Vec<TransactionOutput>,
    pub(crate) lock_time: u64,
    pub(crate) subnetwork_id: [u8; 20],
    pub(crate) gas: u64,
    pub(crate) payload: Vec<u8>,
    pub(crate) mass: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct TransactionInput {
    pub(crate) previous_txid: Hash32,
    pub(crate) previous_index: u32,
    pub(crate) signature_script: Vec<u8>,
    pub(crate) sequence: u64,
    pub(crate) sig_op_count: u8,
}

#[derive(Clone, Debug)]
pub(crate) struct TransactionOutput {
    pub(crate) value: u64,
    pub(crate) script_public_key_version: u16,
    pub(crate) script_public_key_script: Vec<u8>,
}

pub(crate) trait HeaderSource {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>>;
}

pub(crate) trait HeaderStore: HeaderSource {
    fn store_name(&self) -> &'static str;
    fn resolved_db_path(&self) -> &Path;
    fn resolution_notes(&self) -> &[String];
    fn tips(&mut self) -> Result<(Vec<Hash32>, Hash32)>;
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VerificationReport {
    pub generated_at_unix_ms: u64,
    pub success: bool,
    pub requested_node_type: String,
    pub provided_datadir: Option<String>,
    pub pre_checkpoint_datadir: Option<String>,
    pub checkpoint_utxos_gz: Option<String>,
    pub resolved_input_path: Option<String>,
    pub resolved_db_path: Option<String>,
    pub store_type: Option<String>,
    pub tips_count: Option<usize>,
    pub headers_selected_tip: Option<String>,
    pub headers_selected_tip_timestamp_ms: Option<u64>,
    pub chain_tip_timestamp_ms: Option<u64>,
    pub tip_age_ms: Option<u64>,
    pub sync_warning_triggered: bool,
    pub continued_after_sync_warning: Option<bool>,
    pub aborted_due_to_sync_warning: bool,
    pub genesis_mode: Option<String>,
    pub active_genesis_hash: Option<String>,
    pub chain_tip_used: Option<String>,
    pub tips: Vec<String>,
    pub checkpoint_utxo_dump_verified: bool,
    pub checkpoint_utxo_dump_source: Option<String>,
    pub checkpoint_utxo_dump_source_url: Option<String>,
    pub checkpoint_utxo_dump_records: Option<u64>,
    pub checkpoint_utxo_commitment: Option<String>,
    pub checkpoint_daa_score: Option<u64>,
    pub checkpoint_total_sompi: Option<String>,
    pub checkpoint_total_kas: Option<String>,
    pub checkpoint_reference_baseline_kas: Option<String>,
    pub checkpoint_excess_over_reference_kas: Option<String>,
    pub screen_output_lines: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CheckpointJson {
    pub(crate) checkpoint_hash: String,
    pub(crate) original_genesis_hash: String,
    pub(crate) headers_chain: Vec<CheckpointHeaderJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CheckpointHeaderJson {
    pub(crate) hash: String,
    pub(crate) version: u16,
    pub(crate) parents: Vec<Vec<String>>,
    #[serde(rename = "hashMerkleRoot")]
    pub(crate) hash_merkle_root: String,
    #[serde(rename = "acceptedIDMerkleRoot")]
    pub(crate) accepted_id_merkle_root: String,
    #[serde(rename = "utxoCommitment")]
    pub(crate) utxo_commitment: String,
    #[serde(rename = "timeInMilliseconds")]
    pub(crate) time_in_milliseconds: u64,
    pub(crate) bits: u32,
    pub(crate) nonce: u64,
    #[serde(rename = "daaScore")]
    pub(crate) daa_score: u64,
    #[serde(rename = "blueScore")]
    pub(crate) blue_score: u64,
    #[serde(rename = "blueWork")]
    pub(crate) blue_work: String,
    #[serde(rename = "pruningPoint")]
    pub(crate) pruning_point: String,
}
