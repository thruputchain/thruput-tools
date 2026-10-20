use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use thruput_json_rpc_server::{
    ChainPlane, SubmitPlane, TipReading, ValidatedTransaction, ViewPlane,
};
use thruput_json_rpc_types::ContractAddress;
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::protocol_token::protocol_text;
use thruput_scp::network::ProtocolCommit as Protocol;
use thruput_view::{
    ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Dispatcher, ViewGatewayIntroOutcome,
    ViewGatewayPayload, ViewGatewayResult,
};
use thruput_xdr::PROTOCOL_VERSION;
use thruput_xdr::transaction::InvocationFrameOutput;
use thruput_xdr::view::TransactionViewQuery;
use tokio::net::TcpListener;
use xdr_common::{BytesM, VecM};

use super::*;

#[derive(Clone)]
struct UnusedPlane;

#[async_trait]
impl SubmitPlane for UnusedPlane {
    async fn submit_raw_transaction(
        &self,
        _: RpcRequestContext,
        _: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        panic!("views must not submit transactions")
    }
}

#[async_trait]
impl ChainPlane for UnusedPlane {
    async fn chain_tip(&self, _: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        Err(RpcServerError::NotImplemented("chainTip".into()))
    }
    async fn get_block_by_number(
        &self,
        _: RpcRequestContext,
        _: thruput_json_rpc_types::BlockHeight,
    ) -> Result<Option<thruput_json_rpc_types::block::BlockInfo>, RpcServerError> {
        panic!("views must not fetch blocks")
    }
    async fn pending_transaction_count(&self, _: RpcRequestContext) -> Result<u64, RpcServerError> {
        panic!("views must not read the mempool")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeViewGatewayMode {
    IntroRejected,
    ViewSlow,
    Batch,
}

struct FakeViewGateway {
    mode: FakeViewGatewayMode,
}

#[async_trait]
impl ViewGateway_ViewGateway1 for FakeViewGateway {
    async fn viewgateway_view_batch(
        &self,
        query: thruput_view::ViewGatewayBatchQuery,
    ) -> Result<thruput_view::ViewGatewayBatchResult, ProgramError> {
        use thruput_view::{
            ViewGatewayBatchPayload, ViewGatewayBatchResult, ViewGatewayHeightUnavailable,
        };
        assert!(query.budget_millis > 0);
        if self.mode != FakeViewGatewayMode::Batch {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return Ok(ViewGatewayBatchResult::Default { outcome: 6 });
        }
        assert!(query.budget_millis > 30_000);
        assert_eq!(query.max_response_bytes, 256);
        assert_ne!(
            query.calls.0[0].auth_claim,
            shared_view::public_view_auth_claim()
        );
        assert_ne!(query.calls.0[0].auth_claim, query.calls.0[1].auth_claim);
        if let Some(requested) = query.min_height.filter(|height| **height > 42) {
            return Ok(ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
                height_unavailable: ViewGatewayHeightUnavailable {
                    requested,
                    observed: 42.into(),
                },
            });
        }
        Ok(ViewGatewayBatchResult::ViewGatewayBatchOk {
            payload: ViewGatewayBatchPayload {
                height: 42.into(),
                results: VecM(
                    query
                        .calls
                        .0
                        .into_iter()
                        .map(|call| thruput_xdr::view::TransactionViewResult {
                            output: InvocationFrameOutput::Return {
                                return_buffer: BytesM(call.invocation.calldata.0),
                            },
                            gas_consumed: call.invocation.gas_limit,
                            events: VecM(vec![]),
                        })
                        .collect(),
                ),
            },
        })
    }
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
        query: TransactionViewQuery,
    ) -> Result<ViewGatewayResult, ProgramError> {
        if self.mode == FakeViewGatewayMode::ViewSlow {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        Ok(ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
                gas_consumed: query.invocation.gas_limit,
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
        auth_claim: None,
        min_height: None,
        contract: ContractAddress::from(thruput_xdr::types::ContractAddress::from([7; 32])),
        entrypoint: HexBytes(vec![8; 8]),
        calldata: HexBytes(vec![1, 2, 3]),
        caller,
        gas_limit: None,
    }
}

#[tokio::test]
async fn batch_pending_io_respects_the_public_deadline() {
    let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::ViewSlow).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let result = backend
        .view_batch(
            RpcRequestContext::new(Some(Duration::from_millis(10))),
            thruput_json_rpc_types::view::ViewBatchRequest {
                calls: vec![thruput_json_rpc_types::view::ViewCall {
                    auth_claim: None,
                    contract: ContractAddress::from(thruput_xdr::types::ContractAddress::from(
                        [7; 32],
                    )),
                    entrypoint: HexBytes(vec![8; 8]),
                    calldata: HexBytes(vec![1, 2, 3]),
                    caller: None,
                    gas_limit: None,
                }],
                min_height: None,
            },
            10_000_000,
            thruput_json_rpc_server::ViewBatchConfig::default(),
        )
        .await;
    assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
}

#[tokio::test]
async fn public_views_preserve_order_gas_height_and_freshness_errors() {
    use crate::stub_backend::StubBackend;
    use thruput_json_rpc_api::GroundhogApiServer;
    use thruput_json_rpc_server::{BuildInfo, GroundhogRpcServer, RpcServerConfig};
    use thruput_json_rpc_types::{NetworkHash, network_info::NetworkInfo};
    use thruput_xdr::types::SHA256Hash;

    let (addr, fake) = start_fake_view_gateway(FakeViewGatewayMode::Batch).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_mins(1),
    )
    .await
    .unwrap();
    let module = GroundhogRpcServer::new(
        UnusedPlane,
        UnusedPlane,
        StubBackend,
        backend,
        NetworkInfo::new(NetworkHash::from(SHA256Hash::from([0; 32]))),
        RpcServerConfig::default()
            .with_view_timeout_ms(60_000)
            .with_view_batch(thruput_json_rpc_server::ViewBatchConfig {
                max_gas: 1_000,
                max_response_bytes: 256,
                ..Default::default()
            }),
        BuildInfo::new("test", None),
    )
    .into_rpc();
    for min_height in ["42", "43"] {
        let calls: Vec<_> = [("0x01", "123"), ("0x02", "456")].into_iter().map(|(calldata, gas)| serde_json::json!({
            "contract": thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([7; 32])), "entrypoint": "0x0808080808080808", "calldata": calldata, "gasLimit": gas,
            "authClaim": {"kind":"sourceAddress", "allowedKeys":5, "requiredWeight": if calldata == "0x01" { 3 } else { 4 }},
        })).collect();
        let request = serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"groundhog_viewBatch", "params":[{"calls":calls,"minHeight":min_height}]}).to_string();
        let (response, _) = module.raw_json_request(&request, 1).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(response.get()).unwrap();
        if min_height == "42" {
            assert_eq!(
                response["result"],
                serde_json::json!({"height":"42", "results":[
                    {"output":{"result":"return","returnData":"0x01"},"gasUsed":"123","events":[]},
                    {"output":{"result":"return","returnData":"0x02"},"gasUsed":"456","events":[]}
                ]})
            );
        } else {
            assert_eq!(response["error"]["code"], -32011);
            assert_eq!(
                response["error"]["data"],
                serde_json::json!({"requested":"43","observed":"42"})
            );
        }
    }
    for floor in [None, Some("0"), Some("41"), Some("42"), Some("43")] {
        let mut call = serde_json::to_value(request(None)).unwrap();
        call["gasLimit"] = serde_json::json!("2000");
        if let Some(floor) = floor {
            call["minHeight"] = serde_json::json!(floor);
        }
        let request = serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"groundhog_view", "params":[call]}).to_string();
        let (response, _) = module.raw_json_request(&request, 1).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(response.get()).unwrap();
        if floor == Some("43") {
            assert_eq!(response["error"]["code"], -32011);
            assert_eq!(
                response["error"]["data"],
                serde_json::json!({"requested":"43","observed":"42"})
            );
        } else {
            assert_eq!(
                response["result"],
                serde_json::json!({
                    "height":"42", "gasUsed":"2000", "events":[],
                    "output":{"result":"return", "returnData":"0x010203"},
                })
            );
        }
    }
    fake.abort();
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
