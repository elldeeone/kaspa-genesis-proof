use anyhow::{Context, Result, anyhow, bail};
use rocksdb::DB as RocksDb;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::constants::{
    LEGACY_CONSENSUS_ENTRIES_PREFIX, LEGACY_MULTI_CONSENSUS_METADATA_KEY,
    RUST_CONSENSUS_ENTRY_PREFIX, RUST_MULTI_CONSENSUS_METADATA_KEY,
};

use super::rust::open_rocksdb_read_only;

#[derive(Debug)]
pub(crate) struct RustDbResolution {
    pub(crate) active_consensus_db_path: PathBuf,
    pub(crate) notes: Vec<String>,
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

pub(super) fn is_db_dir(path: &Path) -> bool {
    path.join("CURRENT").is_file()
}

fn list_consensus_dirs(consensus_root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let entries = fs::read_dir(consensus_root)
        .with_context(|| format!("failed reading {}", consensus_root.display()))?;

    for entry in entries {
        let entry = entry
            .with_context(|| format!("failed reading entry in {}", consensus_root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|segment| segment.to_str()) else {
            continue;
        };
        if name.starts_with("consensus-") {
            dirs.push(path);
        }
    }

    dirs.sort_by_key(|path| {
        path.file_name()
            .and_then(|segment| segment.to_str())
            .and_then(|segment| segment.strip_prefix("consensus-"))
            .and_then(|segment| segment.parse::<u64>().ok())
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

    let mut consensus_root = None;
    let mut meta_path = None;

    if input_path.join("consensus").is_dir() {
        consensus_root = Some(input_path.join("consensus"));
        if input_path.join("meta").is_dir() {
            meta_path = Some(input_path.join("meta"));
        }
    }

    if consensus_root.is_none() {
        let is_consensus_root = input_path
            .file_name()
            .and_then(|segment| segment.to_str())
            .map(|name| name == "consensus")
            .unwrap_or(false)
            || !list_consensus_dirs(input_path)?.is_empty();

        if is_consensus_root {
            consensus_root = Some(input_path.to_path_buf());
            if let Some(parent) = input_path.parent()
                && parent.join("meta").is_dir()
            {
                meta_path = Some(parent.join("meta"));
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

    let mut active_dir_name = None;
    let mut metadata_current_key = None;
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
                            Ok(Some(key)) => {
                                metadata_current_key = Some(key);
                                active_dir_name = Some(
                                    read_consensus_entry_dir_name(&meta_db, key)?.ok_or_else(
                                        || {
                                            anyhow!(
                                                "multi-consensus metadata referenced current consensus key {key}, but no matching consensus entry was found"
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
        active_dir_name = dirs.last().and_then(|path| {
            path.file_name()
                .and_then(|segment| segment.to_str())
                .map(|segment| segment.to_string())
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

pub(super) fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if !raw.starts_with('~') {
        return path.to_path_buf();
    }

    if raw == "~" {
        return home_dir_from_env().unwrap_or_else(|| path.to_path_buf());
    }

    if let Some(suffix) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        && let Some(home) = home_dir_from_env()
    {
        return home.join(suffix);
    }

    path.to_path_buf()
}

pub(super) fn default_datadir_probe_candidates() -> Vec<PathBuf> {
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

pub(super) fn candidate_go_db_paths(input_path: &Path) -> Result<Vec<PathBuf>> {
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
            if path.is_dir() {
                child_dirs.push(path);
            }
        }

        child_dirs.sort();
        for path in child_dirs {
            push(path.join("datadir2"));
            push(path.join("datadir"));
        }
    }

    Ok(out)
}
