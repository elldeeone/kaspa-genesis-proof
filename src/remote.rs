use std::path::PathBuf;

use anyhow::Result;

use crate::checkpoint_utxo::scan_embedded_checkpoint_utxo_dump;
use crate::cli::{Cli, CliNodeType};
use crate::model::VerificationReport;
use crate::output::{build_initial_report, clear_output_capture, output_capture_snapshot};
use crate::store::{
    refresh_p2p_pruning_proof_cache, seed_p2p_pruning_proof_cache, warm_p2p_pruning_proof_cache,
};
use crate::verify;

#[derive(Clone, Debug)]
pub struct RemoteProofOptions {
    pub rpc_url: String,
    pub p2p_addr: Option<String>,
    pub pre_checkpoint_datadir: Option<PathBuf>,
    pub checkpoint_utxos_gz: Option<PathBuf>,
}

pub fn run_remote_proof(options: RemoteProofOptions) -> VerificationReport {
    clear_output_capture();

    let cli = Cli {
        node_type: CliNodeType::Auto,
        datadir: None,
        rpc_url: Some(options.rpc_url),
        p2p_addr: options.p2p_addr,
        pre_checkpoint_datadir: options.pre_checkpoint_datadir,
        checkpoint_utxos_gz: options.checkpoint_utxos_gz,
        json_out: None,
        verbose: false,
        no_input: true,
        pause_on_exit: false,
    };

    let mut report = build_initial_report(&cli);
    match verify::run(&cli, &mut report) {
        Ok(success) => {
            report.success = success;
        }
        Err(err) => {
            report.success = false;
            report.error = Some(format!("{err:#}"));
        }
    }
    report.screen_output_lines = output_capture_snapshot();
    report
}

pub fn current_remote_proof_output_lines() -> Vec<String> {
    output_capture_snapshot()
}

pub fn warm_up_remote_proof_caches() -> Result<()> {
    scan_embedded_checkpoint_utxo_dump()?;
    Ok(())
}

pub fn warm_up_remote_proof_caches_from_p2p(p2p_addr: &str) -> Result<usize> {
    scan_embedded_checkpoint_utxo_dump()?;
    warm_p2p_pruning_proof_cache(p2p_addr)
}

pub fn refresh_remote_pruning_proof_cache_from_p2p(p2p_addr: &str) -> Result<usize> {
    refresh_p2p_pruning_proof_cache(p2p_addr)
}

pub fn seed_remote_pruning_proof_cache_from_p2p(p2p_addr: &str) -> Result<usize> {
    seed_p2p_pruning_proof_cache(p2p_addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_proof_options_drive_rpc_cli_mode() {
        let options = RemoteProofOptions {
            rpc_url: "grpc://127.0.0.1:16110".to_string(),
            p2p_addr: Some("127.0.0.1:16111".to_string()),
            pre_checkpoint_datadir: None,
            checkpoint_utxos_gz: None,
        };

        assert_eq!(options.rpc_url, "grpc://127.0.0.1:16110");
        assert_eq!(options.p2p_addr.as_deref(), Some("127.0.0.1:16111"));
    }
}
