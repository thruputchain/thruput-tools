//! Supervisor facade for the future hogd-backed local node.
//!
//! This crate owns Rust-side lifecycle concerns for `groundhog node`: process
//! supervision, eventual hogd-dev readiness wiring, and the JSON-RPC facade.
//! It intentionally starts small so the process contract can be settled before
//! hogd-specific command construction lands.

pub mod config;
pub mod error;
pub mod hogd_dev;
pub mod process;
pub mod server;

pub use config::{HogdDevConfig, NodeSupervisorConfig};
pub use error::NodeSupervisorError;
pub use hogd_dev::{
    DEFAULT_HOGD_DEV_BINARY, HOGD_DEV_BINARY_ENV, HogdDevEndpoints, HogdDevProcess,
    HogdDevReadiness, build_hogd_dev_spec, parse_readiness_line, resolve_hogd_dev_binary,
};
pub use process::{ChildProcessSpec, ChildSupervisor, SupervisedChild};
pub use server::NodeSupervisorHandle;
