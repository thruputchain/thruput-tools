//! Supervisor facade for the future hogd-backed local node.
//!
//! This crate owns Rust-side lifecycle concerns for `groundhog node`: process
//! supervision, eventual hogd-dev readiness wiring, and the JSON-RPC facade.
//! It intentionally starts small so the process contract can be settled before
//! hogd-specific command construction lands.

mod block_driver;
pub mod config;
pub mod error;
mod genesis;
pub mod hogd_dev;
mod pid_file;
pub mod process;
mod read_backend;
mod rpc_backend;
pub mod server;
mod stub_backend;
mod view_backend;

#[cfg(test)]
mod test_support;

pub use config::{
    BlockDriverConfig, BlockDriverMode, HogdDevConfig, LogLevel, NodeSupervisorConfig,
};
pub use error::NodeSupervisorError;
pub use hogd_dev::{
    DEFAULT_HOGD_DEV_BINARY, DEVNODE_BINARY_ENV, HogdDevEndpoints, HogdDevProcess,
    HogdDevReadiness, build_hogd_dev_spec, parse_readiness_line, resolve_hogd_dev_binary,
};
pub use process::{ChildProcessSpec, ChildSupervisor, SupervisedChild};
pub use server::{BackendPlanes, NodeSupervisorHandle, PlaneStatus, SupervisorStop};
