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

#[derive(Debug, Default, Serialize)]
pub(crate) struct VerificationReport {
    pub(crate) generated_at_unix_ms: u64,
    pub(crate) success: bool,
    pub(crate) requested_node_type: String,
    pub(crate) provided_datadir: Option<String>,
    pub(crate) resolved_input_path: Option<String>,
    pub(crate) resolved_db_path: Option<String>,
    pub(crate) store_type: Option<String>,
    pub(crate) tips_count: Option<usize>,
    pub(crate) headers_selected_tip: Option<String>,
    pub(crate) headers_selected_tip_timestamp_ms: Option<u64>,
    pub(crate) tip_age_ms: Option<u64>,
    pub(crate) sync_warning_triggered: bool,
    pub(crate) continued_after_sync_warning: Option<bool>,
    pub(crate) aborted_due_to_sync_warning: bool,
    pub(crate) genesis_mode: Option<String>,
    pub(crate) active_genesis_hash: Option<String>,
    pub(crate) chain_tip_used: Option<String>,
    pub(crate) tips: Vec<String>,
    pub(crate) screen_output_lines: Vec<String>,
    pub(crate) error: Option<String>,
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
