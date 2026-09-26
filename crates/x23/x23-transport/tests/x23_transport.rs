use rpc_driver::ConnectionConfig;
use rpc_driver::RpcError;
use rpc_driver::RpcSender;
use rpc_driver::ServiceMap;
use std::sync::Arc;

use async_trait::async_trait;
use rpc_driver::{ProgVers, RpcServer, handler::RpcHandler};
use tokio::net::TcpListener;

use groundhog_x23_transport::*;

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
    // The server stands in for hogd, whose replies exceed rpc_driver's
    // default message size. rpc_driver bounds a connection's unsent
    // outbound bytes at its configured message size, so the server must
    // carry the large-message limit or it closes the connection instead
    // of writing the reply.
    let config = ConnectionConfig::new(Some(HOGD_LARGE_MESSAGE_SIZE), services);
    let handle = RpcServer::new_with_config(listener, config).start().await;
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
        default
            .send_call(ProgVers::new(1, 1), 1, default.call_buf())
            .await,
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
        .send_call(ProgVers::new(1, 1), 1, accepting.call_buf())
        .await
        .expect("bug: larger configured limit should accept the response");

    assert_eq!(response.len(), response_size);
}

#[test]
fn hogd_large_message_limit_is_bounded() {
    assert_eq!(HOGD_LARGE_MESSAGE_SIZE, 64 * 1024 * 1024);
}

// --- multiplexed pools -------------------------------------------------

use std::sync::atomic::{AtomicUsize, Ordering};

use rpc_driver::CanceledCallPolicy;

/// Echoes after a short hold and records the most calls it had in flight at
/// once, so a test can see the transport's execution cap from the peer's side.
struct HoldingEcho {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

#[async_trait]
impl RpcHandler for HoldingEcho {
    async fn dispatch(&self, _proc: u32, data: &[u8]) -> Result<Vec<u8>, RpcError> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(data.to_vec())
    }

    fn prog_vers(&self) -> ProgVers {
        ProgVers::new(1, 1)
    }
}

/// Lets the test keep a handle on the handler the server owns.
struct SharedHandler(Arc<HoldingEcho>);

#[async_trait]
impl RpcHandler for SharedHandler {
    async fn dispatch(&self, proc: u32, data: &[u8]) -> Result<Vec<u8>, RpcError> {
        self.0.dispatch(proc, data).await
    }

    fn prog_vers(&self) -> ProgVers {
        self.0.prog_vers()
    }
}

async fn start_holding_echo_server() -> (std::net::SocketAddr, Arc<HoldingEcho>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: test RPC server should bind");
    let addr = listener
        .local_addr()
        .expect("bug: test RPC server should have a local address");
    let handler = Arc::new(HoldingEcho {
        in_flight: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    });
    let mut services = ServiceMap::new();
    services.add_service(Box::new(SharedHandler(handler.clone())));
    RpcServer::new(listener, services).start().await;
    (addr, handler)
}

async fn echo_calls(sender: &Arc<dyn RpcSender>, calls: usize) {
    let mut pending = Vec::new();
    for i in 0..calls {
        let sender = sender.clone();
        pending.push(tokio::spawn(async move {
            let mut call = sender.call_buf();
            call.extend_from_slice(format!("call-{i}").as_bytes());
            sender.send_call(ProgVers::new(1, 1), 1, call).await
        }));
    }
    for (i, call) in pending.into_iter().enumerate() {
        let response = call
            .await
            .expect("bug: call task should finish")
            .expect("bug: multiplexed call should succeed");
        assert_eq!(response, format!("call-{i}").as_bytes());
    }
}

#[tokio::test]
async fn multiplex_pool_carries_more_calls_than_connections() {
    let (addr, handler) = start_holding_echo_server().await;
    let sender = connect_pool(
        "multiplex",
        &addr.to_string(),
        PoolKind::Multiplex { max_in_flight: 16 },
        2,
        ConnectionConfig::default(),
        build_sender,
    )
    .await
    .expect("bug: multiplexed pool should connect");

    echo_calls(&sender, 16).await;

    // Two connections carried all sixteen at once: the peer saw more than
    // two in flight, which a serial pool of two could never produce.
    assert!(handler.peak.load(Ordering::SeqCst) > 2);
}

#[tokio::test]
async fn multiplex_pool_caps_calls_in_flight_below_connection_count() {
    let (addr, handler) = start_holding_echo_server().await;
    let sender = connect_pool(
        "capped",
        &addr.to_string(),
        PoolKind::Multiplex { max_in_flight: 3 },
        4,
        ConnectionConfig::default(),
        build_sender,
    )
    .await
    .expect("bug: capped pool should connect");

    echo_calls(&sender, 12).await;

    assert!(handler.peak.load(Ordering::SeqCst) <= 3);
}

#[tokio::test]
async fn multiplex_pool_rejects_zero_execution_width() {
    let (addr, _handler) = start_holding_echo_server().await;
    let err = connect_pool(
        "zero",
        &addr.to_string(),
        PoolKind::Multiplex { max_in_flight: 0 },
        1,
        ConnectionConfig::default(),
        build_dummy,
    )
    .await
    .expect_err("bug: a zero execution width should be a configuration error");
    assert!(matches!(
        err,
        ConnectError::ConnectFailed {
            source: RpcError::InvalidPoolConfiguration,
            ..
        }
    ));
}

#[tokio::test]
async fn try_connect_pool_returns_none_on_unreachable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: ephemeral bind should succeed");
    let addr = listener
        .local_addr()
        .expect("bug: bound listener should have a local addr")
        .to_string();
    drop(listener);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        try_connect_pool(
            "dummy",
            &addr,
            PoolKind::Multiplex { max_in_flight: 4 },
            2,
            ConnectionConfig::default(),
            build_dummy,
        ),
    )
    .await
    .expect("bug: try_connect_pool should return promptly on ECONNREFUSED");
    assert!(matches!(result, Ok(None)));
}

#[tokio::test]
async fn serial_pool_kind_matches_the_serial_connect_functions() {
    let (addr, handler) = start_holding_echo_server().await;
    let sender = connect_pool(
        "serial",
        &addr.to_string(),
        PoolKind::Serial {
            canceled_call_policy: CanceledCallPolicy::ReplaceConnection,
        },
        2,
        ConnectionConfig::default(),
        build_sender,
    )
    .await
    .expect("bug: serial pool should connect");

    echo_calls(&sender, 8).await;

    // One call per connection at a time.
    assert!(handler.peak.load(Ordering::SeqCst) <= 2);
}
