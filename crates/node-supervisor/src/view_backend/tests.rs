use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use groundhog_json_rpc_server::ViewPlane;
use groundhog_json_rpc_types::ContractAddress;
use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::protocol_token::protocol_text;
use groundhog_scp::network::ProtocolCommit as Protocol;
use groundhog_view::{
    ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Dispatcher, ViewGatewayIntroOutcome,
    ViewGatewayPayload, ViewGatewayResult,
};
use groundhog_xdr::PROTOCOL_VERSION;
use groundhog_xdr::transaction::InvocationFrameOutput;
use groundhog_xdr::view::TransactionViewQuery;
use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use tokio::net::TcpListener;
use xdr_common::{BytesM, VecM};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeViewGatewayMode {
    IntroRejected,
    ViewSlow,
}

struct FakeViewGateway {
    mode: FakeViewGatewayMode,
}

#[async_trait]
impl ViewGateway_ViewGateway1 for FakeViewGateway {
    async fn viewgateway_intro(
        &self,
        _arg0: Protocol,
    ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
        if self.mode == FakeViewGatewayMode::IntroRejected {
            return Ok(ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol);
        }
        Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
    }

    async fn viewgateway_view(
        &self,
        _arg0: TransactionViewQuery,
    ) -> Result<ViewGatewayResult, ProgramError> {
        if self.mode == FakeViewGatewayMode::ViewSlow {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Ok(ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
                gas_consumed: 0,
                height: 42.into(),
                output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(vec![1, 2, 3]),
                },
                events: VecM(vec![]),
            },
        })
    }
}

async fn start_fake_view_gateway(
    mode: FakeViewGatewayMode,
) -> (
    SocketAddr,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: test ViewGateway should bind");
    let addr = listener
        .local_addr()
        .expect("bug: test ViewGateway should have local addr");
    let mut services = ServiceMap::new();
    services.add_service(Box::new(ViewGateway_ViewGateway1_Dispatcher::new(
        Box::new(FakeViewGateway { mode }),
    )));
    let server = RpcServer::new(listener, services);
    let handle = server.start().await;
    (addr, handle)
}

fn request(caller: Option<ContractAddress>) -> ViewRequest {
    ViewRequest {
        contract: ContractAddress::from(groundhog_xdr::types::ContractAddress::from([7; 32])),
        entrypoint: HexBytes(vec![8; 8]),
        calldata: HexBytes(vec![1, 2, 3]),
        caller,
        gas_limit: None,
    }
}

#[tokio::test]
async fn connect_optional_without_endpoint_uses_stub() {
    let backend = HogdDevViewBackend::connect_optional(
        None,
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_secs(1),
    )
    .await
    .expect("missing endpoint should not fail startup");

    assert_eq!(
        backend.plane_status(),
        crate::server::PlaneStatus::Stub,
        "missing ViewGateway endpoint should degrade to stub"
    );
}

#[tokio::test]
async fn connect_to_dead_view_gateway_port_fails_startup_probe() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: ephemeral bind should succeed");
    let dead_addr = listener
        .local_addr()
        .expect("bug: bound listener should have local addr");
    drop(listener);

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        HogdDevViewBackend::connect_optional(
            Some(dead_addr),
            &protocol_text(&PROTOCOL_VERSION),
            None,
            Duration::from_secs(1),
        ),
    )
    .await
    .expect("bug: connect to dead port should return promptly");

    // A concurrent test can rebind the freed port before we connect, in
    // which case the probe reaches a live listener and fails at the intro
    // RPC instead of the connect.
    assert!(
        matches!(
            result,
            Err(crate::NodeSupervisorError::ViewGatewayConnect { .. }
                | crate::NodeSupervisorError::ViewGatewayRpc {
                    op: "viewgateway_intro",
                    ..
                })
        ),
        "expected ViewGateway startup connection failure, got {result:?}"
    );
}

#[tokio::test]
async fn connect_to_intro_rejected_view_gateway_fails_startup_probe() {
    let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::IntroRejected).await;

    let result = HogdDevViewBackend::connect_optional(
        Some(addr),
        "hogp/sent00000000000",
        Some("hogp/expected0000000"),
        Duration::from_secs(1),
    )
    .await;

    let err = result.expect_err("intro rejection should fail startup");
    let message = err.to_string();
    match err {
        crate::NodeSupervisorError::ProtocolIntroRejected {
            service,
            outcome,
            outcome_name,
            is_protocol_mismatch,
            protocol,
            expected_protocol,
        } => {
            assert_eq!(service, "ViewGateway");
            assert_eq!(
                outcome,
                ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol.0
            );
            assert_eq!(outcome_name, "ViewGatewayIntroMismatchedProtocol");
            assert!(is_protocol_mismatch);
            assert_eq!(protocol, "hogp/sent00000000000");
            assert_eq!(expected_protocol.as_deref(), Some("hogp/expected0000000"));
            assert!(message.contains("ViewGateway intro mismatch"));
            assert!(message.contains("hogp/sent00000000000"));
            assert!(message.contains("hogp/expected0000000"));
            assert!(message.contains("ViewGatewayIntroMismatchedProtocol"));
        }
        other => panic!("expected ViewGateway intro rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn view_gateway_rpc_timeout_maps_to_backend_timeout() {
    let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::ViewSlow).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_millis(10),
    )
    .await
    .expect("fake ViewGateway should connect");

    let result = backend
        .view(RpcRequestContext::unbounded(), request(None), 123)
        .await;

    assert!(
        matches!(result, Err(RpcServerError::BackendTimeout)),
        "ViewGateway timeout should surface BackendTimeout, got {result:?}"
    );
}

#[tokio::test]
async fn request_deadline_caps_view_gateway_timeout() {
    let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::ViewSlow).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_secs(30),
    )
    .await
    .expect("fake ViewGateway should connect");

    let result = backend
        .view(
            RpcRequestContext::new(Some(Duration::from_millis(10))),
            request(None),
            123,
        )
        .await;

    assert!(
        matches!(result, Err(RpcServerError::BackendTimeout)),
        "request deadline should cap ViewGateway timeout, got {result:?}"
    );
}
