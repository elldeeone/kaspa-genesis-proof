mod app;
pub(crate) mod checkpoint_utxo;
pub mod cli;
pub(crate) mod constants;
pub(crate) mod hashing;
pub(crate) mod model;
pub(crate) mod output;
pub(crate) mod store;
pub(crate) mod verify;

#[cfg(test)]
pub(crate) mod test_support;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/serialization.rs"));
}

pub use app::run_cli;
pub use cli::{Cli, CliNodeType};
