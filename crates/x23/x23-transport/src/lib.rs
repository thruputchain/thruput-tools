//! Shared x23 RPC transport helpers.
//!
//! Both the production gateway and the local node supervisor connect to
//! hogd-side x23 services using `rpc_driver::FixedSerialPool` and a
//! codegen client crate. This crate hoists the small amount of
//! plumbing — pool construction, address parsing, typed startup
//! errors — into one place so consumers do not copy it.
//! It is an internal workspace boundary and follows the Groundhog workspace
//! release rather than maintaining an independent public API contract.
//!
//! Consumers pass a builder closure to construct their typed client
//! from the pool. The closure receives an `Arc<dyn RpcSender>`, which
//! matches codegen client constructors directly: a typical call site
//! is just `Client::new` passed as a function pointer.
//!
//! Each consumer also picks the policy that fits them:
//!
//! - [`connect`] fails on any error. Use this when an unreachable
//!   backend is a hard startup failure (for example, the local node
//!   supervisor talking to a child it just spawned).
//! - [`try_connect`] tolerates transient connection failures and
//!   returns `Ok(None)`. Use this when the binary should start in a
//!   degraded mode and reconnect later (the production gateway uses
//!   this so that an indexer or scheduler outage at startup does not
//!   take the gateway down).

use std::{sync::Arc, time::Duration};

use rpc_driver::{
    CanceledCallPolicy, ConnectionConfig, FixedSerialPool, RpcError, RpcSender, ServiceMap,
};

/// Maximum RPC message accepted from trusted hogd data-plane endpoints.
///
/// This provides headroom for full transactions and nomination batches while
/// bounding the allocation triggered by a peer-supplied record header. It
/// changes the decode limit, not the connection's preallocated read-buffer
/// size. x23 uses ONC-RPC `AUTH_NONE`, so these endpoints must remain on a
/// private, trusted network.
pub const HOGD_LARGE_MESSAGE_SIZE: u32 = 64 * 1024 * 1024;

/// Build a client configuration that accepts protocol-sized responses from a
/// trusted hogd endpoint.
pub fn hogd_large_message_config() -> ConnectionConfig {
    ConnectionConfig::new(Some(HOGD_LARGE_MESSAGE_SIZE), ServiceMap::new())
}

/// Errors from establishing an x23 connection pool.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    /// The configured address could not be parsed as a `SocketAddr`.
    /// Treat this as a hard configuration failure.
    /// The configured protocol override cannot name a protocol token.
    #[error("invalid protocol override: {reason}")]
    InvalidProtocolOverride {
        /// Why the configured text cannot name a token.
        reason: String,
    },
    #[error("invalid address '{addr}': {source}")]
    BadAddress {
        addr: String,
        source: std::net::AddrParseError,
    },

    /// The pool failed to connect to the remote service. May be
    /// transient (remote not yet listening) or terminal.
    #[error("failed to connect to {service} at {addr}: {source}")]
    ConnectFailed {
        service: &'static str,
        addr: String,
        source: rpc_driver::RpcError,
    },

    /// The remote service accepted a connection but rejected the startup
    /// handshake/probe.
    #[error("{service} at {addr} rejected startup probe: {reason}")]
    StartupRejected {
        service: &'static str,
        addr: String,
        reason: String,
    },

    /// The remote service accepted a connection but did not answer the startup
    /// handshake/probe before the configured deadline.
    #[error("{service} at {addr} timed out during startup probe after {timeout:?}")]
    StartupProbeTimedOut {
        service: &'static str,
        addr: String,
        timeout: Duration,
    },
}

/// Establish a connection pool and construct the typed client. Fails
/// hard on any error.
///
/// `service` is a static label used only for diagnostics; it appears
/// in the error message and logs.
pub async fn connect<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    build: F,
) -> Result<C, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    connect_with_config_and_canceled_call_policy(
        service,
        addr,
        pool_size,
        ConnectionConfig::default(),
        CanceledCallPolicy::ReplaceConnection,
        build,
    )
    .await
}

/// Establish a fixed pool using `connection_config`.
pub async fn connect_with_config<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    connection_config: ConnectionConfig,
    build: F,
) -> Result<C, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    connect_with_config_and_canceled_call_policy(
        service,
        addr,
        pool_size,
        connection_config,
        CanceledCallPolicy::ReplaceConnection,
        build,
    )
    .await
}

/// Establish a fixed pool with explicit connection and cancellation policies.
pub async fn connect_with_config_and_canceled_call_policy<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    connection_config: ConnectionConfig,
    canceled_call_policy: CanceledCallPolicy,
    build: F,
) -> Result<C, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    let socket_addr: std::net::SocketAddr = addr.parse().map_err(|e| ConnectError::BadAddress {
        addr: addr.to_string(),
        source: e,
    })?;
    let pool: Arc<dyn RpcSender> = Arc::new(
        FixedSerialPool::connect_with_config_and_canceled_call_policy(
            &socket_addr,
            pool_size,
            connection_config,
            canceled_call_policy,
        )
        .await
        .map_err(|e| ConnectError::ConnectFailed {
            service,
            addr: addr.to_string(),
            source: e,
        })?,
    );
    tracing::info!(addr, service, "connected");
    Ok(build(pool))
}

/// Like [`connect`], but treats transient connection failures as a
/// degraded startup: returns `Ok(None)` and logs a warning. Address
/// parsing errors still fail hard, since those are configuration bugs.
pub async fn try_connect<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    build: F,
) -> Result<Option<C>, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    try_connect_with_config_and_canceled_call_policy(
        service,
        addr,
        pool_size,
        ConnectionConfig::default(),
        CanceledCallPolicy::ReplaceConnection,
        build,
    )
    .await
}

/// Like [`connect_with_config`], but tolerates connection refusal.
pub async fn try_connect_with_config<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    connection_config: ConnectionConfig,
    build: F,
) -> Result<Option<C>, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    try_connect_with_config_and_canceled_call_policy(
        service,
        addr,
        pool_size,
        connection_config,
        CanceledCallPolicy::ReplaceConnection,
        build,
    )
    .await
}

/// Like [`try_connect`], with an explicit active-call cancellation policy.
pub async fn try_connect_with_canceled_call_policy<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    canceled_call_policy: CanceledCallPolicy,
    build: F,
) -> Result<Option<C>, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    try_connect_with_config_and_canceled_call_policy(
        service,
        addr,
        pool_size,
        ConnectionConfig::default(),
        canceled_call_policy,
        build,
    )
    .await
}

/// Like [`connect_with_config_and_canceled_call_policy`], but tolerates only
/// explicit connection failures.
pub async fn try_connect_with_config_and_canceled_call_policy<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    connection_config: ConnectionConfig,
    canceled_call_policy: CanceledCallPolicy,
    build: F,
) -> Result<Option<C>, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    match connect_with_config_and_canceled_call_policy(
        service,
        addr,
        pool_size,
        connection_config,
        canceled_call_policy,
        build,
    )
    .await
    {
        Ok(client) => Ok(Some(client)),
        Err(
            error @ ConnectError::ConnectFailed {
                source: RpcError::ConnectFailed,
                ..
            },
        ) => {
            tracing::warn!(addr, service, error = %error, "unreachable, starting in degraded mode");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
