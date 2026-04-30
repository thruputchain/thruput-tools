//! Supervisor facade for the future hogd-backed local node.
//!
//! This crate owns Rust-side lifecycle concerns for `groundhog node`: process
//! supervision, eventual hogd-dev readiness wiring, and the JSON-RPC facade.
//! It intentionally starts small so the process contract can be settled before
//! hogd-specific command construction lands.

pub mod config;
pub mod error;
pub mod process;
pub mod server;

pub use config::{HogdDevConfig, NodeSupervisorConfig};
pub use error::NodeSupervisorError;
pub use process::{ChildProcessSpec, ChildSupervisor, SupervisedChild};
pub use server::NodeSupervisorHandle;
