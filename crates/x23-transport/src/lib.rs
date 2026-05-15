//! Shared x23 RPC transport helpers.
//!
//! Both the production gateway and the local node supervisor connect to
//! hogd-side x23 services using `rpc_driver::FixedSerialPool` and a
//! codegen client crate. This crate hoists the small amount of
//! plumbing — pool construction, address parsing, typed startup
//! errors — into one place so consumers do not copy it.
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

use std::sync::Arc;

use rpc_driver::{FixedSerialPool, RpcSender};

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
    let socket_addr: std::net::SocketAddr = addr.parse().map_err(|e| ConnectError::BadAddress {
        addr: addr.to_string(),
        source: e,
    })?;
    let pool: Arc<dyn RpcSender> = Arc::new(
        FixedSerialPool::connect(&socket_addr, pool_size)
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
    match connect(service, addr, pool_size, build).await {
        Ok(client) => Ok(Some(client)),
        Err(e @ (ConnectError::BadAddress { .. } | ConnectError::StartupRejected { .. })) => Err(e),
        Err(e) => {
            tracing::warn!(addr, service, error = %e, "unreachable, starting in degraded mode");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct DummyClient;

    fn build_dummy(_pool: Arc<dyn RpcSender>) -> DummyClient {
        DummyClient
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
}
