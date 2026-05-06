mod app;
pub(crate) mod checkpoint_utxo;
pub mod cli;
pub(crate) mod constants;
pub(crate) mod hashing;
pub(crate) mod model;
pub(crate) mod output;
pub mod remote;
pub(crate) mod store;
pub(crate) mod verify;

#[cfg(test)]
pub(crate) mod test_support;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/serialization.rs"));
}

pub(crate) mod rpcwire {
    include!(concat!(env!("OUT_DIR"), "/rpcwire/protowire.rs"));
}

pub(crate) mod p2pwire {
    include!(concat!(env!("OUT_DIR"), "/p2pwire/protowire.rs"));
}

pub use app::run_cli;
pub use cli::{Cli, CliNodeType};
pub use model::VerificationReport;
pub use remote::{
    RemoteProofOptions, current_remote_proof_output_lines,
    refresh_remote_pruning_proof_cache_from_p2p, run_remote_proof,
    seed_remote_pruning_proof_cache_from_p2p, warm_up_remote_proof_caches,
    warm_up_remote_proof_caches_from_p2p,
};
