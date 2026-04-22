use anyhow::{Context, Result, anyhow, bail};
use prost::Message;
use rocksdb::{DB as RocksDb, Direction, IteratorMode, Options as RocksOptions};
use rusty_leveldb::{DB as LevelDb, Options as LevelOptions};
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::hashing::{
    decode_rust_header, decode_tip_hash_from_key_suffix, hash32_from_hex, to_hash32,
};
use crate::output::print_info;
use crate::{
    CHECKPOINT_DATA_JSON, CheckpointJson, CheckpointStore, Cli, CliNodeType, ConsensusEntry,
    GoStore, Hash32, HeaderSource, HeaderStore, LEGACY_CONSENSUS_ENTRIES_PREFIX,
    LEGACY_MULTI_CONSENSUS_METADATA_KEY, MultiConsensusMetadata, OpenStoreResult, ParsedHeader,
    ROCKSDB_READ_ONLY_MAX_OPEN_FILES, RUST_CONSENSUS_ENTRY_PREFIX,
    RUST_MULTI_CONSENSUS_METADATA_KEY, RustDbResolution, RustStore, proto,
};

impl RustStore {
    pub(crate) fn open(input_path: &Path) -> Result<Self> {
        let resolution = resolve_rust_db_path(input_path)?;
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
            if let Some(hash) = decode_tip_hash_from_key_suffix(&key[1..]) {
                if seen_tips.insert(hash) {
                    tips.push(hash);
                }
            }
        }

        Ok((tips, hst))
    }
}

impl GoStore {
    pub(crate) fn open(input_path: &Path) -> Result<Self> {
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

        Ok((tips, hst))
    }
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
    const ROCKSDB_OPEN_ATTEMPTS: usize = 4;
    let mut opts = RocksOptions::default();
    opts.create_if_missing(false);
    // Keep verifier FD usage bounded so it can run alongside a live node.
    opts.set_max_open_files(ROCKSDB_READ_ONLY_MAX_OPEN_FILES);
    opts.set_comparator(
        "leveldb.BytewiseComparator",
        Box::new(|a: &[u8], b: &[u8]| a.cmp(b)),
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
    let key_suffix = key.to_le_bytes();

    for entry_key in [
        [RUST_CONSENSUS_ENTRY_PREFIX, key_suffix.as_slice()].concat(),
        [LEGACY_CONSENSUS_ENTRIES_PREFIX, key_suffix.as_slice()].concat(),
    ] {
        let Some(bytes) = meta_db
            .get(&entry_key)
            .with_context(|| format!("failed reading consensus entry key {key}"))?
        else {
            continue;
        };

        return parse_consensus_entry_dir_name(&bytes)
            .with_context(|| format!("failed decoding consensus entry {key}"))
            .map(Some);
    }

    Ok(None)
}

pub(crate) fn parse_current_consensus_key(metadata_bytes: &[u8]) -> Result<Option<u64>> {
    if metadata_bytes.is_empty() {
        return Ok(None);
    }

    if let Ok(metadata) = bincode::deserialize::<MultiConsensusMetadata>(metadata_bytes) {
        let _ = (
            metadata.max_key_used,
            metadata.is_archival_node,
            metadata.props.len(),
            metadata.version,
            metadata.staging_consensus_key,
        );
        return Ok(metadata.current_consensus_key);
    }

    match metadata_bytes[0] {
        0 => Ok(None),
        1 => {
            if metadata_bytes.len() < 9 {
                bail!("metadata ended before Option<u64> value");
            }

            Ok(Some(u64::from_le_bytes(
                metadata_bytes[1..9]
                    .try_into()
                    .map_err(|_| anyhow!("invalid Option<u64> payload length"))?,
            )))
        }
        tag => bail!("invalid Option<u64> tag: {tag}"),
    }
}

pub(crate) fn parse_consensus_entry_dir_name(entry_bytes: &[u8]) -> Result<String> {
    if let Ok(entry) = bincode::deserialize::<ConsensusEntry>(entry_bytes) {
        let _ = (entry.key, entry.creation_timestamp);
        return Ok(entry.directory_name);
    }

    if entry_bytes.len() < 24 {
        bail!("consensus entry shorter than minimum struct size");
    }

    let name_len = u64::from_le_bytes(
        entry_bytes[8..16]
            .try_into()
            .map_err(|_| anyhow!("invalid consensus entry name length bytes"))?,
    ) as usize;
    let name_end = 16 + name_len;
    if name_end + 8 > entry_bytes.len() {
        bail!("consensus entry ended before directory name/timestamp");
    }

    let directory_name = std::str::from_utf8(&entry_bytes[16..name_end])
        .context("consensus entry directory name is not valid utf-8")?;

    Ok(directory_name.to_string())
}

pub(crate) fn resolve_rust_db_path(input_path: &Path) -> Result<RustDbResolution> {
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
    let mut metadata_current_key: Option<u64> = None;
    let mut detected_layout = "rust-consensus-directory-fallback".to_string();

    if let Some(meta_path) = meta_path {
        if is_db_dir(&meta_path) {
            match open_rocksdb_read_only(&meta_path) {
                Ok(meta_db) => {
                    let mut metadata_bytes = None;
                    for metadata_key in [
                        RUST_MULTI_CONSENSUS_METADATA_KEY,
                        LEGACY_MULTI_CONSENSUS_METADATA_KEY,
                    ] {
                        if let Some(bytes) = meta_db.get(metadata_key).with_context(|| {
                            format!(
                                "reading multi-consensus metadata key {}",
                                String::from_utf8_lossy(metadata_key)
                            )
                        })? {
                            metadata_bytes = Some(bytes);
                            break;
                        }
                    }

                    if let Some(bytes) = metadata_bytes {
                        match parse_current_consensus_key(&bytes) {
                            Ok(Some(k)) => {
                                metadata_current_key = Some(k);
                                active_dir_name = Some(
                                    read_consensus_entry_dir_name(&meta_db, k)?.ok_or_else(
                                        || {
                                            anyhow!(
                                                "multi-consensus metadata referenced current consensus key {k}, but no matching consensus entry was found"
                                            )
                                        },
                                    )?,
                                );
                                detected_layout = "rust-meta-managed".to_string();
                            }
                            Ok(None) => {
                                notes.push(
                                    "multi-consensus metadata did not specify a current consensus key (falling back to directory scan)".to_string(),
                                );
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

    let active_dir_name = active_dir_name.ok_or_else(|| {
        anyhow!(
            "could not resolve active consensus directory under {}",
            consensus_root.display()
        )
    })?;
    let active_consensus_db_path = consensus_root.join(&active_dir_name);

    if !active_consensus_db_path.is_dir() {
        if let Some(current_key) = metadata_current_key {
            bail!(
                "multi-consensus metadata referenced current consensus key {current_key} with directory '{active_dir_name}', but {} does not exist",
                active_consensus_db_path.display()
            );
        }

        bail!(
            "could not resolve active consensus directory under {}",
            consensus_root.display()
        );
    }

    notes.push(format!("Detected layout: {detected_layout}"));
    notes.push(format!(
        "Active consensus DB: {}",
        active_consensus_db_path.display()
    ));

    Ok(RustDbResolution {
        active_consensus_db_path,
        notes,
    })
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

fn candidate_go_db_paths(input_path: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    let mut push = |path: PathBuf| {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    };

    push(input_path.to_path_buf());
    push(input_path.join("datadir2"));
    push(input_path.join("datadir"));
    push(input_path.join("kaspa-mainnet").join("datadir2"));
    push(input_path.join("kaspa-mainnet").join("datadir"));

    if input_path.is_dir() {
        let mut child_dirs = Vec::new();
        for entry in fs::read_dir(input_path)
            .with_context(|| format!("failed reading directory {}", input_path.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed reading entry in {}", input_path.display()))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            child_dirs.push(path);
        }

        child_dirs.sort();
        for path in child_dirs {
            push(path.join("datadir2"));
            push(path.join("datadir"));
        }
    }

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

pub(crate) fn open_store_with_resolved_input(cli: &Cli) -> Result<OpenStoreResult> {
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
