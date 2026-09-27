//! Shared x23 RPC transport helpers.
//!
//! Both the production gateway and the local node supervisor connect to
//! hogd-side x23 services using an `rpc_driver` connection pool and a
//! codegen client crate. This crate hoists the small amount of
//! plumbing (pool construction, address parsing, typed startup errors)
//! into one place so consumers do not copy it.
//!
//! [`PoolKind`] picks the pool shape: the serial pool every `connect*`
//! function below builds, or the multiplexed pool [`connect_pool`] and
//! [`try_connect_pool`] build on request.
//!
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

use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, PoisonError, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};

use async_trait::async_trait;
use rpc_driver::{
    CallBuf, CanceledCallPolicy, ConnectionConfig, FixedSerialPool, MultiplexPool, ProgVers,
    RpcError, RpcResponse, RpcSender, ServiceMap,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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
    /// `FixedSerialPool`: one call in flight per connection, so the
    /// connection count is the execution width; `canceled_call_policy`
    /// decides what a canceled call does to its connection.
    Serial {
        canceled_call_policy: CanceledCallPolicy,
    },
    /// `MultiplexPool`: many calls in flight per connection, routed to the
    /// least-loaded one; `max_in_flight` caps the execution width
    /// independently of the connection count.
    ///
    /// A canceled call keeps its share of that width until its connection
    /// delivers the late reply or fails, so the cap counts every call the
    /// peer may still be executing. A canceled call still outstanding
    /// `stalled_call_timeout` after its cancellation means its connection
    /// is not delivering: the pool shuts every connection down, failing
    /// the calls on them, and reconnects with backoff, as a serial pool
    /// replaces a canceled call's connection.
    Multiplex {
        max_in_flight: usize,
        stalled_call_timeout: Duration,
    },
}

/// A multiplexed pool with an execution width and stall recovery, so both
/// stay consumer decisions as they are with a serial pool's connection
/// count and cancellation policy.
struct MultiplexSender {
    shared: Arc<MultiplexShared>,
}

struct MultiplexShared {
    service: &'static str,
    addr: SocketAddr,
    connections: usize,
    connection_config: ConnectionConfig,
    stalled_call_timeout: Duration,
    pool: RwLock<Arc<MultiplexPool>>,
    slots: Arc<Semaphore>,
    /// Set while a replacement pool is being opened.
    replacing: AtomicBool,
    /// Set once the sender is dropped; no replacement is installed after.
    closed: AtomicBool,
}

type PoolCall = Pin<Box<dyn Future<Output = Result<RpcResponse, RpcError>> + Send>>;

impl MultiplexShared {
    fn current_pool(&self) -> Arc<MultiplexPool> {
        self.pool
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Keep driving a call whose caller gave up, holding its execution
    /// slot until the pool resolves it, and treat one the pool never
    /// resolves as a stalled connection.
    fn detach(self: &Arc<Self>, call: PoolCall, slot: OwnedSemaphorePermit) {
        // Outside a runtime nothing can drive the call, so it is dropped
        // with its slot; that only happens at teardown.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let shared = self.clone();
        runtime.spawn(async move {
            let _slot = slot;
            if tokio::time::timeout(shared.stalled_call_timeout, call)
                .await
                .is_err()
            {
                shared.replace_pool();
            }
        });
    }

    /// Shut the current pool down, failing every call on it, and open a
    /// replacement in the background with backoff. Calls made meanwhile
    /// fail fast rather than waiting for it.
    fn replace_pool(self: &Arc<Self>) {
        if self.closed.load(Ordering::SeqCst) || self.replacing.swap(true, Ordering::AcqRel) {
            return;
        }
        tracing::warn!(
            addr = %self.addr,
            service = self.service,
            timeout = ?self.stalled_call_timeout,
            "canceled call still outstanding after its stall timeout; replacing the pool"
        );
        let shared = self.clone();
        tokio::spawn(async move {
            shared.current_pool().shutdown().await;
            let mut retry_delay = Duration::from_millis(100);
            loop {
                if shared.closed.load(Ordering::SeqCst) {
                    break;
                }
                match MultiplexPool::connect_with_config(
                    &shared.addr,
                    shared.connections,
                    shared.connection_config.clone(),
                )
                .await
                {
                    Ok(pool) => {
                        let pool = Arc::new(pool);
                        let stale = {
                            let mut installed =
                                shared.pool.write().unwrap_or_else(PoisonError::into_inner);
                            if shared.closed.load(Ordering::SeqCst) {
                                Some(pool)
                            } else {
                                *installed = pool;
                                None
                            }
                        };
                        if let Some(pool) = stale {
                            // The sender was dropped while this pool was
                            // being opened; it is ours to close.
                            pool.shutdown().await;
                        } else {
                            tracing::info!(
                                addr = %shared.addr,
                                service = shared.service,
                                connections = shared.connections,
                                "multiplexed pool replaced"
                            );
                        }
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(
                            addr = %shared.addr,
                            service = shared.service,
                            %error,
                            retry_in = ?retry_delay,
                            "multiplexed pool replacement failed"
                        );
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(5));
                    }
                }
            }
            shared.replacing.store(false, Ordering::Release);
        });
    }
}

/// A call holding one execution slot. Dropped before the pool resolves it,
/// it hands the call and the slot to a background task instead of
/// releasing the slot.
struct TrackedCall {
    shared: Arc<MultiplexShared>,
    call: Option<PoolCall>,
    slot: Option<OwnedSemaphorePermit>,
}

impl Future for TrackedCall {
    type Output = Result<RpcResponse, RpcError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let call = self
            .call
            .as_mut()
            .expect("bug: tracked call polled after completion");
        let output = ready!(call.as_mut().poll(cx));
        self.call = None;
        self.slot = None;
        Poll::Ready(output)
    }
}

impl Drop for TrackedCall {
    fn drop(&mut self) {
        if let (Some(call), Some(slot)) = (self.call.take(), self.slot.take()) {
            self.shared.detach(call, slot);
        }
    }
}

#[async_trait]
impl RpcSender for MultiplexSender {
    fn call_buf(&self) -> CallBuf {
        self.shared.current_pool().call_buf()
    }

    async fn send_call(
        &self,
        progvers: ProgVers,
        proc: u32,
        call: CallBuf,
    ) -> Result<RpcResponse, RpcError> {
        let slot = self
            .shared
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RpcError::PoolClosed)?;
        let pool = self.shared.current_pool();
        TrackedCall {
            shared: self.shared.clone(),
            call: Some(Box::pin(async move {
                pool.send_call(progvers, proc, call).await
            })),
            slot: Some(slot),
        }
        .await
    }

    /// Completes once the call is queued, so the slot is held only that long.
    async fn send_fire_and_forget(
        &self,
        progvers: ProgVers,
        proc: u32,
        call: CallBuf,
    ) -> Result<(), RpcError> {
        let _slot = self
            .shared
            .slots
            .acquire()
            .await
            .map_err(|_| RpcError::PoolClosed)?;
        self.shared
            .current_pool()
            .send_fire_and_forget(progvers, proc, call)
            .await
    }
}

impl Drop for MultiplexSender {
    fn drop(&mut self) {
        let pool = {
            let installed = self
                .shared
                .pool
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            self.shared.closed.store(true, Ordering::SeqCst);
            installed.clone()
        };
        self.shared.slots.close();
        // Resolve detached calls now rather than when their stall timeout
        // elapses. Without a runtime the pool's own drop begins this once
        // the last detached call lets go of it.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { pool.shutdown().await });
        }
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
/// the typed client. Fails hard on any error. A serial pool's width is
/// `connections`; a multiplexed pool's is its `max_in_flight`.
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
        PoolKind::Multiplex {
            max_in_flight,
            stalled_call_timeout,
        } => {
            if max_in_flight == 0 {
                return Err(connect_failed(RpcError::InvalidPoolConfiguration));
            }
            let pool = MultiplexPool::connect_with_config(
                &socket_addr,
                connections,
                connection_config.clone(),
            )
            .await
            .map_err(connect_failed)?;
            Arc::new(MultiplexSender {
                shared: Arc::new(MultiplexShared {
                    service,
                    addr: socket_addr,
                    connections,
                    connection_config,
                    stalled_call_timeout,
                    pool: RwLock::new(Arc::new(pool)),
                    slots: Arc::new(Semaphore::new(max_in_flight)),
                    replacing: AtomicBool::new(false),
                    closed: AtomicBool::new(false),
                }),
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
