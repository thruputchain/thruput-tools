//! Supervisor for the hogd-backed local node behind `thruput node`.
//!
//! This crate owns the Rust-side lifecycle of the local runtime: process
//! supervision, readiness and block-driver wiring, local funding state, and
//! the JSON-RPC facade that serves the same public API as the gateway.

mod airdrop;
mod block_driver;
pub mod config;
pub mod error;
mod funding_journal;
mod genesis;
pub mod hogd_dev;
mod local_state;
mod pid_file;
pub mod process;
mod read_backend;
mod request_deadline;
mod rpc_backend;
pub mod server;
mod stub_backend;
mod submit_batcher;
mod view_backend;

#[cfg(test)]
mod test_support;

/// The dataset the `fake_hogd_dev` executable serves, shared with the unit-test fakes.
/// Declared here rather than in `test_support`, which other crates' tests include by path.
#[cfg(test)]
#[path = "../tests/fixtures/canned_x23.rs"]
mod canned_x23;

/// How this crate names hogd-dev's x23 services in diagnostics, so a
/// message always says which of the child's endpoints answered.
mod service {
    pub(crate) const INDEXER: &str = "hogd-dev indexer";
    pub(crate) const DB_GATEWAY: &str = "hogd-dev DbGateway";
    pub(crate) const VIEW_GATEWAY: &str = "hogd-dev ViewGateway";
    pub(crate) const SCHEDULER_GATEWAY: &str = "hogd-dev scheduler gateway";
}

pub use block_driver::{BlockProductionError, ManualBlockProducer, ProducedBlock};
pub use config::{
    BlockDriverConfig, BlockDriverMode, HogdDevConfig, LocalAirdropConfig, LogLevel,
    NodeSupervisorConfig,
};
pub use error::NodeSupervisorError;
pub use hogd_dev::{
    DEFAULT_HOGD_DEV_BINARY, DEVNODE_BINARY_ENV, HogdDevEndpoints, HogdDevProcess,
    HogdDevReadiness, build_hogd_dev_spec_with_bootstrap_manifest, parse_readiness_line,
    resolve_hogd_dev_binary, resolve_hogd_dev_binary_before_path, resolve_hogd_dev_binary_on_path,
};
pub use process::{ChildProcessSpec, ChildSupervisor, SupervisedChild};
pub use server::{BackendPlanes, NodeSupervisorHandle, PlaneStatus, SupervisorStop};
pub use thruput_json_rpc_server::batch::RpcBatchConcurrency;
pub use thruput_tx_validation::{ConfigParamName, UnknownConfigParamName};

/// Private chain files included with persistent local-state backups.
pub use local_state::{LOCAL_FAUCET_KEY_FILE, LOCAL_FUNDING_ATTEMPTS_FILE};
