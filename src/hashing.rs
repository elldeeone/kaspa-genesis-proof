use anyhow::{Context, Result};
use blake2b_simd::Params;

use crate::model::{Hash32, ParsedHeader, Transaction};

pub(crate) fn hash32_from_hex(hex_str: &str) -> Result<Hash32> {
    let decoded = hex::decode(hex_str).with_context(|| format!("invalid hex: {hex_str}"))?;
    decoded
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected 32 bytes, got {}", decoded.len()))
}

pub(crate) fn hex_of(hash: &Hash32) -> String {
    hex::encode(hash)
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

pub(crate) fn header_hash(h: &ParsedHeader) -> Hash32 {
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

pub(crate) fn transaction_hash(tx: &Transaction, include_mass_commitment: bool) -> Hash32 {
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
