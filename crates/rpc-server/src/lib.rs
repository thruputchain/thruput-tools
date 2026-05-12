//! Reusable server-side JSON-RPC presentation layer.
//!
//! This crate owns common request validation, public error mapping, and the
//! `GroundhogApiServer` implementation. Concrete binaries provide backend
//! adapters for the production gateway and hogd-dev node supervisor
//! compositions.

pub mod backend;
pub mod config;
pub mod error;
pub mod server;
pub mod tx;

pub use backend::{ChainPlane, ReadPlane, SubmitPlane, ValidatedTransaction};
pub use config::RpcServerConfig;
pub use error::RpcServerError;
pub use server::{BuildInfo, GroundhogRpcServer};
