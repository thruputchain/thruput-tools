//! Reusable server-side JSON-RPC presentation layer.
//!
//! This crate owns common request validation, public error mapping, and the
//! [`native::service::NativeService`] implementation. Concrete binaries provide
//! backend adapters for the production gateway and hogd-dev node supervisor
//! compositions.

pub mod backend;
pub mod batch;
pub mod config;
pub mod error;
pub mod params;
mod tip_cache;
pub mod tx;

pub use thruput_build_info::{BuildVersion, build_version};

pub use backend::{ChainPlane, RpcRequestContext, TipReading, ValidatedTransaction};
pub use config::{DEFAULT_VIEW_GAS_LIMIT, RpcServerConfig, ViewBatchConfig};
pub use error::RpcServerError;

/// Native JSON-RPC transport.
pub mod native;
