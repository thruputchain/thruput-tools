//! Reusable server-side JSON-RPC presentation layer.
//!
//! This crate owns common request validation, public error mapping, and the
//! `GroundhogApiServer` implementation. Concrete binaries provide backend
//! adapters for the production gateway and hogd-dev node supervisor
//! compositions.

pub mod backend;
pub mod batch;
pub mod config;
pub mod error;
pub mod server;
mod tip_cache;
pub mod tx;

pub use groundhog_build_info::{BuildVersion, build_version};

pub use backend::{
    ChainPlane, ReadPlane, RpcRequestContext, SubmitPlane, TipReading, ValidatedTransaction,
    ViewPlane,
};
pub use config::{DEFAULT_VIEW_GAS_LIMIT, RpcServerConfig, ViewBatchConfig};
pub use error::RpcServerError;
pub use server::{BuildInfo, GroundhogRpcServer};
