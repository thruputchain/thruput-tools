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
