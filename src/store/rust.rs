use anyhow::{Context, Result, anyhow, bail};
use rocksdb::{DB as RocksDb, Direction, IteratorMode, Options as RocksOptions};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::constants::ROCKSDB_READ_ONLY_MAX_OPEN_FILES;
use crate::model::{Hash32, HeaderSource, HeaderStore, ParsedHeader};

use super::probe::RustDbResolution;

#[derive(Debug)]
pub(crate) struct RustStore {
    db: RocksDb,
    resolution: RustDbResolution,
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

impl RustStore {
    pub(crate) fn open(input_path: &Path) -> Result<Self> {
        let resolution = super::probe::resolve_rust_db_path(input_path)?;
        let db = open_rocksdb_read_only(&resolution.active_consensus_db_path)?;
        validate_rust_consensus_db(&db, &resolution.active_consensus_db_path)?;
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
                && let Ok(header) = decode_rust_header(&bytes)
            {
                return Ok(Some(header));
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

        let hst = hst_bytes
            .get(..32)
            .ok_or_else(|| {
                anyhow!(
                    "headers selected tip value is too short: {} bytes",
                    hst_bytes.len()
                )
            })?
            .try_into()
            .map_err(|_| {
                anyhow!(
                    "headers selected tip value is too short: {} bytes",
                    hst_bytes.len()
                )
            })?;

        let mut seen_tips = BTreeSet::new();
        let mut tips = Vec::new();
        let iter = self
            .db
            .iterator(IteratorMode::From(&[24u8], Direction::Forward));

        for item in iter {
            let (key, _value) = item.context("iterating rust tips prefix")?;
            if key.first().copied() != Some(24u8) {
                break;
            }
            if let Some(hash) = decode_tip_hash_from_key_suffix(&key[1..])
                && seen_tips.insert(hash)
            {
                tips.push(hash);
            }
        }

        Ok((tips, hst))
    }
}

fn trimmed_blue_work_from_words(words: [u64; 3]) -> Vec<u8> {
    let mut le = [0u8; 24];
    for (index, word) in words.iter().enumerate() {
        le[index * 8..(index + 1) * 8].copy_from_slice(&word.to_le_bytes());
    }

    let mut be = le;
    be.reverse();
    let start = be.iter().position(|byte| *byte != 0).unwrap_or(be.len());
    be[start..].to_vec()
}

fn expand_compressed_parents(runs: &[(u8, Vec<Hash32>)]) -> Result<Vec<Vec<Hash32>>> {
    let mut out = Vec::new();
    let mut previous = 0u8;

    for (cumulative, parents) in runs {
        if *cumulative <= previous {
            bail!(
                "invalid compressed parents: non-increasing cumulative count {} <= {}",
                cumulative,
                previous
            );
        }

        for _ in 0..(*cumulative - previous) {
            out.push(parents.clone());
        }
        previous = *cumulative;
    }

    Ok(out)
}

fn convert_header_wire_compressed(header: HeaderWireCompressed) -> Result<ParsedHeader> {
    let _ = header.hash;
    Ok(ParsedHeader {
        version: header.version,
        parents: expand_compressed_parents(&header.parents_by_level.0)?,
        hash_merkle_root: header.hash_merkle_root,
        accepted_id_merkle_root: header.accepted_id_merkle_root,
        utxo_commitment: header.utxo_commitment,
        time_in_milliseconds: header.timestamp,
        bits: header.bits,
        nonce: header.nonce,
        daa_score: header.daa_score,
        blue_score: header.blue_score,
        blue_work_trimmed_be: trimmed_blue_work_from_words(header.blue_work),
        pruning_point: header.pruning_point,
    })
}

fn convert_header_wire_legacy(header: HeaderWireLegacy) -> ParsedHeader {
    let _ = header.hash;
    ParsedHeader {
        version: header.version,
        parents: header.parents_by_level,
        hash_merkle_root: header.hash_merkle_root,
        accepted_id_merkle_root: header.accepted_id_merkle_root,
        utxo_commitment: header.utxo_commitment,
        time_in_milliseconds: header.timestamp,
        bits: header.bits,
        nonce: header.nonce,
        daa_score: header.daa_score,
        blue_score: header.blue_score,
        blue_work_trimmed_be: trimmed_blue_work_from_words(header.blue_work),
        pruning_point: header.pruning_point,
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

pub(crate) fn decode_tip_hash_from_key_suffix(suffix: &[u8]) -> Option<Hash32> {
    if suffix.len() == 32 {
        return suffix.try_into().ok();
    }

    if suffix.len() >= 40 {
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&suffix[0..8]);
        if u64::from_le_bytes(len_bytes) == 32 {
            return suffix[8..40].try_into().ok();
        }
    }

    if suffix.len() >= 32 {
        return suffix[suffix.len() - 32..].try_into().ok();
    }

    None
}

pub(super) fn open_rocksdb_read_only(path: &Path) -> Result<RocksDb> {
    const ROCKSDB_OPEN_ATTEMPTS: usize = 4;

    let mut opts = RocksOptions::default();
    opts.create_if_missing(false);
    opts.set_max_open_files(ROCKSDB_READ_ONLY_MAX_OPEN_FILES);
    opts.set_comparator(
        "leveldb.BytewiseComparator",
        Box::new(|left: &[u8], right: &[u8]| left.cmp(right)),
    );

    let mut last_err_text = None;
    for attempt in 0..ROCKSDB_OPEN_ATTEMPTS {
        match RocksDb::open_for_read_only(&opts, path, false) {
            Ok(db) => return Ok(db),
            Err(err) => {
                let err_text = err.to_string();
                let should_retry = attempt + 1 < ROCKSDB_OPEN_ATTEMPTS
                    && is_transient_rocksdb_open_failure(&err_text);
                if should_retry {
                    last_err_text = Some(err_text);
                    thread::sleep(Duration::from_millis(200 * (attempt as u64 + 1)));
                    continue;
                }

                return Err(err)
                    .with_context(|| format!("failed opening RocksDB at {}", path.display()));
            }
        }
    }

    bail!(
        "failed opening RocksDB at {} after {} attempts: {}",
        path.display(),
        ROCKSDB_OPEN_ATTEMPTS,
        last_err_text.unwrap_or_else(|| "unknown RocksDB open failure".to_string())
    )
}

pub(crate) fn is_transient_rocksdb_open_failure(err_text: &str) -> bool {
    err_text.contains(".sst") && err_text.contains("No such file or directory")
}

fn validate_rust_consensus_db(db: &RocksDb, path: &Path) -> Result<()> {
    let Some(headers_selected_tip_bytes) = db.get([7u8]).with_context(|| {
        format!(
            "reading rust headers selected tip key from {}",
            path.display()
        )
    })?
    else {
        bail!(
            "{} is not a valid rusty-kaspa consensus DB: missing headers selected tip key",
            path.display()
        );
    };

    if headers_selected_tip_bytes.len() < 32 {
        bail!(
            "{} is not a valid rusty-kaspa consensus DB: headers selected tip value is too short ({})",
            path.display(),
            headers_selected_tip_bytes.len()
        );
    }

    Ok(())
}
