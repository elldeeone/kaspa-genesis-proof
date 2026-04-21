use anyhow::{Context, Result, bail};
use blake2b_simd::Params;

use crate::{
    Hash32, HeaderWireCompressed, HeaderWireLegacy, HeaderWithBlockLevelWireCompressed,
    HeaderWithBlockLevelWireLegacy, ParsedHeader, Transaction,
};

pub(crate) fn to_hash32(bytes: &[u8]) -> Result<Hash32> {
    if bytes.len() != 32 {
        bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

pub(crate) fn hash32_from_hex(hex_str: &str) -> Result<Hash32> {
    let decoded = hex::decode(hex_str).with_context(|| format!("invalid hex: {hex_str}"))?;
    to_hash32(&decoded)
}

pub(crate) fn hex_of(hash: &Hash32) -> String {
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

pub(crate) fn decode_rust_header(bytes: &[u8]) -> Result<ParsedHeader> {
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

pub(crate) fn decode_tip_hash_from_key_suffix(suffix: &[u8]) -> Option<Hash32> {
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
