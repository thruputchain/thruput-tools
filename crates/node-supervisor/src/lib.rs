//! Supervisor facade for the future hogd-backed local node.
//!
//! This crate owns Rust-side lifecycle concerns for `thruput node`: process
//! supervision, eventual hogd-dev readiness wiring, and the JSON-RPC facade.
//! It intentionally starts small so the process contract can be settled before
//! hogd-specific command construction lands.

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
pub use groundhog_json_rpc_server::batch::RpcBatchConcurrency;
pub use groundhog_tx_validation::{ConfigParamName, UnknownConfigParamName};
pub use hogd_dev::{
    DEFAULT_HOGD_DEV_BINARY, DEVNODE_BINARY_ENV, HogdDevEndpoints, HogdDevProcess,
    HogdDevReadiness, build_hogd_dev_spec_with_bootstrap_manifest, parse_readiness_line,
    resolve_hogd_dev_binary, resolve_hogd_dev_binary_before_path, resolve_hogd_dev_binary_on_path,
};
pub use process::{ChildProcessSpec, ChildSupervisor, SupervisedChild};
pub use server::{BackendPlanes, NodeSupervisorHandle, PlaneStatus, SupervisorStop};

/// Private chain files included with persistent local-state backups.
pub use local_state::{LOCAL_FAUCET_KEY_FILE, LOCAL_FUNDING_ATTEMPTS_FILE};
