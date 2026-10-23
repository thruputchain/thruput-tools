//! Reusable server-side JSON-RPC presentation layer.
//!
//! This crate owns common request validation, public error mapping, and the
//! [`native::service::NativeService`] implementation. Concrete binaries provide
//! backend adapters for the production gateway and hogd-dev node supervisor
//! compositions.

pub mod backend;
pub mod batch;
mod block_queries;
pub mod config;
pub mod error;
pub mod params;
pub mod server;
mod tip_cache;
pub mod tx;

pub use thruput_build_info::{BuildVersion, build_version};

pub use backend::{
    ChainPlane, ReadPlane, RpcRequestContext, SubmitPlane, TipReading, ValidatedTransaction,
    ViewPlane,
};
pub use block_queries::validate_page as validate_block_transaction_page;
pub use config::{DEFAULT_VIEW_GAS_LIMIT, RpcServerConfig, ViewBatchConfig};
pub use error::RpcServerError;
pub use server::GroundhogRpcServer;

/// Native JSON-RPC transport.
pub mod native;
