//! Shared hogd x23 backend adapters.
//!
//! The production gateway and the local node supervisor both serve the same
//! public JSON-RPC surface from the same hogd x23 services: `SchedGateway`
//! for submission and chain tip, `IndexerBlockExplorer` for receipts, status,
//! and events, `DbGateway` for latest-state reads, and `ViewGateway` for
//! simulated calls. Everything in the seam between those wire types and the
//! `thruput-json-rpc-types` payloads is identical work, so it lives here
//! once instead of in each binary.
//!
//! This crate is the decoding and mapping half of a backend adapter. It owns
//! no connections, no timeouts, no retry policy, and no metrics: callers keep
//! those, because that is where the gateway and the supervisor genuinely
//! differ (the gateway reconnects, coalesces, meters, and degrades; the
//! supervisor owns a child process and fails fast). Mapping functions are
//! synchronous and total — given a decoded wire value they return a public
//! payload or an [`RpcServerError`], and never block. The one place that
//! composes several backend reads into an answer, the [`status`] probe,
//! takes the reads from the caller and owns only their order and precedence.
//!
//! `thruput-x23-transport` remains the layer below: it builds the
//! connection pools these adapters are driven with.
//!
//! [`RpcServerError`]: thruput_json_rpc_server::RpcServerError
//!
//! # Service labels
//!
//! Error text names the backend that failed, and the two consumers name the
//! same service differently — the gateway talks to "indexer", the supervisor
//! to "hogd-dev indexer". Mapping functions that can name a backend take a
//! `service` label for that reason; pass the same label the rest of the
//! binary's diagnostics use.

pub mod db_gateway;
pub mod event;
pub mod indexer;
pub mod intro;
pub mod receipt;
pub mod scheduler;
pub mod status;
pub mod submit;
pub mod view;

/// Re-exported because it appears in consumers' own error types; everything
/// else is reached through its module.
pub use intro::IntroOutcome;
