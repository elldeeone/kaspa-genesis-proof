mod checkpoint;
mod go;
mod probe;
mod rpc;
mod rust;

#[cfg(test)]
mod tests;

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

use crate::cli::{Cli, CliNodeType};
use crate::model::HeaderStore;
use crate::output::print_info;

pub(crate) use checkpoint::CheckpointStore;
pub(crate) use go::GoStore;
pub(crate) use rpc::{
    RpcStore, refresh_p2p_pruning_proof_cache, seed_p2p_pruning_proof_cache,
    warm_p2p_pruning_proof_cache,
};
pub(crate) use rust::RustStore;

pub(crate) struct OpenStoreResult {
    pub(crate) store: Box<dyn HeaderStore>,
    pub(crate) input_path: PathBuf,
    pub(crate) probe_notes: Vec<String>,
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
    if let Some(rpc_url) = cli.rpc_url.as_deref() {
        let store = RpcStore::connect(rpc_url, cli.p2p_addr.as_deref())?;
        return Ok(OpenStoreResult {
            store: Box::new(store),
            input_path: PathBuf::from(rpc_url),
            probe_notes: vec![format!("Input source: --rpc-url ({rpc_url})")],
        });
    }

    if let Some(input_datadir) = cli.datadir.as_deref() {
        let expanded = probe::expand_tilde(input_datadir);
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

    let candidates = probe::default_datadir_probe_candidates();
    if candidates.is_empty() {
        bail!("could not auto-detect datadir: no probe candidates were generated");
    }

    let existing_candidates = candidates
        .iter()
        .filter(|candidate| candidate.exists())
        .cloned()
        .collect::<Vec<_>>();

    if existing_candidates.is_empty() {
        bail!(
            "could not auto-detect datadir. No default Kaspa paths exist. Checked: {}. Supply --datadir explicitly.",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let mut errors = Vec::new();
    for candidate in &existing_candidates {
        match detect_and_open_store(cli.node_type, candidate) {
            Ok(store) => {
                return Ok(OpenStoreResult {
                    store,
                    input_path: candidate.clone(),
                    probe_notes: vec![
                        "Input path source: auto-detection from OS default Kaspa locations"
                            .to_string(),
                        format!("Auto-selected input path: {}", candidate.display()),
                    ],
                });
            }
            Err(err) => {
                errors.push(format!("{}: {err}", candidate.display()));
            }
        }
    }

    bail!(
        "auto-detect failed for all existing default paths: {}. Supply --datadir explicitly.",
        errors.join(" | ")
    )
}
