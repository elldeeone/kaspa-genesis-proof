use anyhow::{Context, Result, anyhow, bail};
use prost::Message;
use rusty_leveldb::{DB as LevelDb, Options as LevelOptions};
use std::path::{Path, PathBuf};

use crate::model::{Hash32, HeaderSource, HeaderStore, ParsedHeader};
use crate::proto;

use super::probe::{candidate_go_db_paths, is_db_dir};

pub(crate) struct GoStore {
    db: LevelDb,
    db_path: PathBuf,
    active_prefix: u8,
    notes: Vec<String>,
}

impl GoStore {
    pub(crate) fn open(input_path: &Path) -> Result<Self> {
        let candidates = candidate_go_db_paths(input_path)?;
        let mut errors = Vec::new();

        for candidate in candidates {
            if !is_db_dir(&candidate) {
                continue;
            }

            let candidate_str = candidate
                .to_str()
                .ok_or_else(|| anyhow!("non-utf8 db path: {}", candidate.display()))?;
            let opts = LevelOptions {
                create_if_missing: false,
                ..LevelOptions::default()
            };

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

                    return Ok(Self {
                        db,
                        db_path: candidate.clone(),
                        active_prefix: prefix_bytes[0],
                        notes: vec![format!(
                            "Resolved Go LevelDB path: {} (active-prefix={})",
                            candidate.display(),
                            prefix_bytes[0]
                        )],
                    });
                }
                Err(err) => errors.push(format!("{}: {err}", candidate.display())),
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

        let mut parents = Vec::with_capacity(db_header.parents.len());
        for level in db_header.parents {
            let mut level_hashes = Vec::with_capacity(level.parent_hashes.len());
            for parent in level.parent_hashes {
                level_hashes.push(
                    parent
                        .hash
                        .as_slice()
                        .try_into()
                        .map_err(|_| anyhow!("invalid parent hash length"))?,
                );
            }
            parents.push(level_hashes);
        }

        let hash_merkle_root = db_header
            .hash_merkle_root
            .ok_or_else(|| anyhow!("missing hash_merkle_root"))?
            .hash
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("invalid hash_merkle_root length"))?;

        let accepted_id_merkle_root = db_header
            .accepted_id_merkle_root
            .ok_or_else(|| anyhow!("missing accepted_id_merkle_root"))?
            .hash
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("invalid accepted_id_merkle_root length"))?;

        let utxo_commitment = db_header
            .utxo_commitment
            .ok_or_else(|| anyhow!("missing utxo_commitment"))?
            .hash
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("invalid utxo_commitment length"))?;

        let pruning_point = db_header
            .pruning_point
            .ok_or_else(|| anyhow!("missing pruning_point"))?
            .hash
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("invalid pruning_point length"))?;

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
        let hst = db_hst
            .hash
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("invalid headers-selected-tip hash length"))?;

        let tips_key = go_db_key(self.active_prefix, b"tips", None);
        let tips_bytes = self
            .db
            .get(&tips_key)
            .ok_or_else(|| anyhow!("tips key not found"))?;
        let db_tips =
            proto::DbTips::decode(tips_bytes.as_ref()).context("failed decoding DbTips")?;

        let mut tips = Vec::with_capacity(db_tips.tips.len());
        for tip in db_tips.tips {
            tips.push(
                tip.hash
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow!("invalid tip hash length"))?,
            );
        }

        Ok((tips, hst))
    }
}

pub(crate) fn go_db_key(prefix: u8, bucket: &[u8], suffix: Option<&[u8]>) -> Vec<u8> {
    let mut key = Vec::with_capacity(2 + bucket.len() + suffix.map(|s| 1 + s.len()).unwrap_or(0));
    key.push(prefix);
    key.push(b'/');
    key.extend_from_slice(bucket);
    if let Some(suffix) = suffix {
        key.push(b'/');
        key.extend_from_slice(suffix);
    }
    key
}
