use anyhow::{Context, Result, bail};
use std::collections::HashMap;

use crate::constants::CHECKPOINT_DATA_JSON;
use crate::hashing::hash32_from_hex;
use crate::model::{CheckpointJson, Hash32, HeaderSource, ParsedHeader};

#[derive(Default)]
pub(crate) struct CheckpointStore {
    headers: HashMap<Hash32, ParsedHeader>,
}

impl HeaderSource for CheckpointStore {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>> {
        Ok(self.headers.get(block_hash).cloned())
    }
}

impl CheckpointStore {
    pub(crate) fn from_embedded_json() -> Result<Self> {
        let parsed: CheckpointJson = serde_json::from_str(CHECKPOINT_DATA_JSON)
            .context("failed parsing embedded checkpoint_data.json")?;

        let mut headers = HashMap::with_capacity(parsed.headers_chain.len());
        for entry in parsed.headers_chain {
            let hash = hash32_from_hex(&entry.hash)
                .with_context(|| format!("invalid checkpoint header hash {}", entry.hash))?;

            let mut parents = Vec::with_capacity(entry.parents.len());
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

            headers.insert(
                hash,
                ParsedHeader {
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
                    blue_work_trimmed_be: hex::decode(&entry.blue_work).with_context(|| {
                        format!("invalid checkpoint blueWork {}", entry.blue_work)
                    })?,
                    pruning_point: hash32_from_hex(&entry.pruning_point)?,
                },
            );
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

    #[cfg(test)]
    pub(crate) fn iter_headers(&self) -> impl Iterator<Item = (&Hash32, &ParsedHeader)> {
        self.headers.iter()
    }
}
