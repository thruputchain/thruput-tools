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

/// Establish a fixed pool with an explicit active-call cancellation policy.
pub async fn connect_with_canceled_call_policy<C, F>(
    service: &'static str,
    addr: &str,
    pool_size: usize,
    canceled_call_policy: CanceledCallPolicy,
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
        canceled_call_policy,
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

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use rpc_driver::{ProgVers, RpcServer, handler::RpcHandler};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Debug)]
    struct DummyClient;

    fn build_dummy(_pool: Arc<dyn RpcSender>) -> DummyClient {
        DummyClient
    }

    fn build_sender(pool: Arc<dyn RpcSender>) -> Arc<dyn RpcSender> {
        pool
    }

    struct SizedResponse(usize);

    #[async_trait]
    impl RpcHandler for SizedResponse {
        async fn dispatch(&self, _proc: u32, _data: &[u8]) -> Result<Vec<u8>, RpcError> {
            Ok(vec![0; self.0])
        }

        fn prog_vers(&self) -> ProgVers {
            ProgVers::new(1, 1)
        }
    }

    async fn start_sized_response_server(
        response_size: usize,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test RPC server should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test RPC server should have a local address");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SizedResponse(response_size)));
        let handle = RpcServer::new(listener, services).start().await;
        (addr, handle)
    }

    #[tokio::test]
    async fn connect_rejects_bad_address() {
        let err = connect("dummy", "not-an-address", 1, build_dummy)
            .await
            .expect_err("bug: malformed address should fail");
        assert!(matches!(err, ConnectError::BadAddress { .. }));
    }

    #[tokio::test]
    async fn try_connect_propagates_bad_address() {
        let err = try_connect("dummy", "not-an-address", 1, build_dummy)
            .await
            .expect_err("bug: malformed address should still fail try_connect");
        assert!(matches!(err, ConnectError::BadAddress { .. }));
    }

    #[tokio::test]
    async fn try_connect_returns_none_on_unreachable() {
        use std::time::Duration;

        // Bind an ephemeral loopback port and drop the listener so the
        // kernel refuses subsequent connections at that address. More
        // reliable than assuming a fixed port (e.g. 1) refuses, since
        // sandboxed CI runners and local firewalls vary on those.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: ephemeral bind should succeed");
        let addr = listener
            .local_addr()
            .expect("bug: bound listener should have a local addr")
            .to_string();
        drop(listener);

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            try_connect("dummy", &addr, 1, build_dummy),
        )
        .await
        .expect("bug: try_connect should return promptly on ECONNREFUSED");
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn try_connect_rejects_invalid_pool_configuration() {
        let err = try_connect("dummy", "127.0.0.1:1", 0, build_dummy)
            .await
            .expect_err("bug: invalid pool configuration should not degrade startup");

        assert!(matches!(
            err,
            ConnectError::ConnectFailed {
                source: RpcError::InvalidPoolConfiguration,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn large_connection_limit_crosses_default_boundary() {
        const DEFAULT_MESSAGE_SIZE: usize = 16 * 1024 * 1024;
        let response_size = DEFAULT_MESSAGE_SIZE + 1024;
        let (addr, _server) = start_sized_response_server(response_size).await;
        let default = connect("default", &addr.to_string(), 1, build_sender)
            .await
            .expect("bug: default client should connect");

        assert_eq!(
            default.send_call(ProgVers::new(1, 1), 1, &[]).await,
            Err(RpcError::SystemError)
        );

        let accepting = connect_with_config(
            "accepting",
            &addr.to_string(),
            1,
            hogd_large_message_config(),
            build_sender,
        )
        .await
        .expect("bug: accepting client should connect");
        let response = accepting
            .send_call(ProgVers::new(1, 1), 1, &[])
            .await
            .expect("bug: larger configured limit should accept the response");

        assert_eq!(response.len(), response_size);
    }

    #[test]
    fn hogd_large_message_limit_is_bounded() {
        assert_eq!(HOGD_LARGE_MESSAGE_SIZE, 64 * 1024 * 1024);
    }
}
