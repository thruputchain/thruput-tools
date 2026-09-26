//! Shared x23 RPC transport helpers.
//!
//! Both the production gateway and the local node supervisor connect to
//! hogd-side x23 services using an `rpc_driver` connection pool and a
//! codegen client crate. This crate hoists the small amount of
//! plumbing — pool construction, address parsing, typed startup
//! errors — into one place so consumers do not copy it.
//!
//! Two pool shapes are available through [`PoolKind`]: the serial pool
//! that every `connect*` function below builds, with one call in flight
//! per connection, and the multiplexed pool that [`connect_pool`] and
//! [`try_connect_pool`] build on request, with many calls in flight per
//! connection and an execution width set separately from the connection
//! count.
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

use async_trait::async_trait;
use rpc_driver::{
    CallBuf, CanceledCallPolicy, ConnectionConfig, FixedSerialPool, MultiplexPool, ProgVers,
    RpcError, RpcResponse, RpcSender, ServiceMap,
};
use tokio::sync::Semaphore;

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

/// Which `rpc_driver` pool a backend uses, and what a canceled call means
/// to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolKind {
    /// `FixedSerialPool`: one call in flight per connection, FIFO per
    /// connection. The connection count is also the number of calls that
    /// execute at once; further calls queue in the pool.
    /// `canceled_call_policy` decides what happens to a connection whose
    /// active call is canceled.
    Serial {
        canceled_call_policy: CanceledCallPolicy,
    },
    /// `MultiplexPool`: many calls in flight per connection, each routed to
    /// the connection with the fewest calls outstanding. `max_in_flight`
    /// is the execution width, enforced here in the transport, and is
    /// independent of the connection count; a call past it waits for a
    /// slot, as it would queue in a serial pool.
    ///
    /// A canceled call's reply is discarded and its connection stays in
    /// service. There is no per-connection cancellation policy: a consumer
    /// that relied on [`CanceledCallPolicy::PoisonConnection`] to force a
    /// reconnect after a canceled call must invalidate its client itself,
    /// as the gateway does by scheduler generation.
    Multiplex { max_in_flight: usize },
}

/// Caps the calls in flight through a multiplexed pool so execution width
/// stays a consumer decision, as it is with a serial pool's connection
/// count.
struct BoundedSender {
    inner: Arc<dyn RpcSender>,
    slots: Arc<Semaphore>,
}

#[async_trait]
impl RpcSender for BoundedSender {
    fn call_buf(&self) -> CallBuf {
        self.inner.call_buf()
    }

    async fn send_call(
        &self,
        progvers: ProgVers,
        proc: u32,
        call: CallBuf,
    ) -> Result<RpcResponse, RpcError> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| RpcError::PoolClosed)?;
        self.inner.send_call(progvers, proc, call).await
    }

    async fn send_fire_and_forget(
        &self,
        progvers: ProgVers,
        proc: u32,
        call: CallBuf,
    ) -> Result<(), RpcError> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| RpcError::PoolClosed)?;
        self.inner.send_fire_and_forget(progvers, proc, call).await
    }
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
    connect_pool(
        service,
        addr,
        PoolKind::Serial {
            canceled_call_policy,
        },
        pool_size,
        connection_config,
        build,
    )
    .await
}

/// Establish a pool of `kind` over `connections` connections and construct
/// the typed client. Fails hard on any error.
///
/// For a serial pool `connections` is also the execution width. For a
/// multiplexed pool the width is the kind's `max_in_flight`, and a zero
/// width is rejected like a zero connection count.
pub async fn connect_pool<C, F>(
    service: &'static str,
    addr: &str,
    kind: PoolKind,
    connections: usize,
    connection_config: ConnectionConfig,
    build: F,
) -> Result<C, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    let socket_addr: std::net::SocketAddr = addr.parse().map_err(|e| ConnectError::BadAddress {
        addr: addr.to_string(),
        source: e,
    })?;
    let connect_failed = |source| ConnectError::ConnectFailed {
        service,
        addr: addr.to_string(),
        source,
    };
    let pool: Arc<dyn RpcSender> = match kind {
        PoolKind::Serial {
            canceled_call_policy,
        } => Arc::new(
            FixedSerialPool::connect_with_config_and_canceled_call_policy(
                &socket_addr,
                connections,
                connection_config,
                canceled_call_policy,
            )
            .await
            .map_err(connect_failed)?,
        ),
        PoolKind::Multiplex { max_in_flight } => {
            if max_in_flight == 0 {
                return Err(connect_failed(RpcError::InvalidPoolConfiguration));
            }
            let pool =
                MultiplexPool::connect_with_config(&socket_addr, connections, connection_config)
                    .await
                    .map_err(connect_failed)?;
            Arc::new(BoundedSender {
                inner: Arc::new(pool),
                slots: Arc::new(Semaphore::new(max_in_flight)),
            })
        }
    };
    tracing::info!(addr, service, pool = ?kind, connections, "connected");
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
    try_connect_pool(
        service,
        addr,
        PoolKind::Serial {
            canceled_call_policy,
        },
        pool_size,
        connection_config,
        build,
    )
    .await
}

/// Like [`connect_pool`], but tolerates only explicit connection failures.
pub async fn try_connect_pool<C, F>(
    service: &'static str,
    addr: &str,
    kind: PoolKind,
    connections: usize,
    connection_config: ConnectionConfig,
    build: F,
) -> Result<Option<C>, ConnectError>
where
    F: FnOnce(Arc<dyn RpcSender>) -> C,
{
    match connect_pool(service, addr, kind, connections, connection_config, build).await {
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
