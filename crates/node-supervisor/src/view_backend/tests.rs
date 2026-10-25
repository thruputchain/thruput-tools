use std::net::SocketAddr;
use std::time::Duration;

use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use thruput_json_rpc_server::native::read::NativeSimulationPlane;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeViewGatewayMode {
    IntroRejected,
    ViewSlow,
    Batch,
}

struct FakeViewGateway {
    mode: FakeViewGatewayMode,
}

#[allow(refining_impl_trait)]
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

fn invocation() -> thruput_json_rpc_types::requests::Invocation {
    thruput_json_rpc_types::requests::Invocation {
        address: ContractAddress::from(thruput_xdr::types::ContractAddress::from([7; 32])),
        entrypoint: HexBytes(vec![8; 8]),
        calldata: HexBytes(vec![1, 2, 3]),
        caller: None,
        assumed_auth: None,
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
        .simulate_batch(
            RpcRequestContext::new(Some(Duration::from_millis(10))),
            thruput_json_rpc_types::requests::SimulateInvocationsRequest {
                invocations: vec![invocation()],
                min_state_block_height: None,
            },
            10_000_000,
            thruput_json_rpc_server::ViewBatchConfig::default(),
        )
        .await;
    assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
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
    let dead_addr = crate::test_support::DEAD_LOOPBACK_ADDR;

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
        .simulate(
            RpcRequestContext::unbounded(),
            thruput_json_rpc_types::requests::SimulateInvocationRequest {
                address: ContractAddress::from(thruput_xdr::types::ContractAddress::from([7; 32])),
                entrypoint: HexBytes(vec![8; 8]),
                calldata: HexBytes(vec![1, 2, 3]),
                caller: None,
                assumed_auth: None,
                gas_limit: None,
                min_state_block_height: None,
            },
            123,
        )
        .await;

    assert!(
        matches!(result, Err(RpcServerError::BackendTimeout)),
        "ViewGateway timeout should surface BackendTimeout, got {result:?}"
    );
}

#[tokio::test]
async fn native_local_simulation_preserves_snapshot_and_deadline() {
    use thruput_json_rpc_types::{requests, responses};
    for mode in [FakeViewGatewayMode::Batch, FakeViewGatewayMode::ViewSlow] {
        let (addr, task) = start_fake_view_gateway(mode).await;
        let backend = HogdDevViewBackend::connect_optional(
            Some(addr),
            &protocol_text(&PROTOCOL_VERSION),
            None,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        let request = requests::SimulateInvocationRequest {
            address: thruput_json_rpc_types::base58::Base58(
                thruput_xdr::types::ContractAddress::from([7; 32]),
            ),
            entrypoint: HexBytes(vec![8; 8]),
            calldata: HexBytes(vec![1, 2, 3]),
            caller: None,
            assumed_auth: None,
            gas_limit: Some(100),
            min_state_block_height: Some(42.into()),
        };
        // Only the slow gateway gets a short deadline; a tight one would also
        // cut off a healthy reply on a loaded machine.
        let context = if mode == FakeViewGatewayMode::ViewSlow {
            RpcRequestContext::new(Some(Duration::from_millis(50)))
        } else {
            RpcRequestContext::unbounded()
        };
        let result = backend.simulate(context, request, 100).await;
        if mode == FakeViewGatewayMode::ViewSlow {
            assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
        } else {
            let result = result.unwrap();
            assert_eq!(*result.state_block_height, 42);
            assert_eq!(result.gas_used, 100);
            assert!(matches!(
                result.output,
                responses::InvocationOutput::Return { .. }
            ));
        }
        task.abort();
    }
}

#[tokio::test]
async fn native_local_batch_preserves_order_and_freshness_evidence() {
    use thruput_json_rpc_server::ViewBatchConfig;
    use thruput_json_rpc_types::{requests as input, responses};
    let (addr, task) = start_fake_view_gateway(FakeViewGatewayMode::Batch).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_mins(1),
    )
    .await
    .unwrap();
    for floor in [42, 43] {
        let request = input::SimulateInvocationsRequest {
            min_state_block_height: Some(floor.into()),
            invocations: [3, 4]
                .into_iter()
                .map(|weight| input::Invocation {
                    address: thruput_json_rpc_types::base58::Base58(
                        thruput_xdr::types::ContractAddress::from([7; 32]),
                    ),
                    entrypoint: HexBytes(vec![8; 8]),
                    calldata: HexBytes(vec![u8::try_from(weight).unwrap()]),
                    caller: None,
                    assumed_auth: Some(input::AssumedAuth::SourceAddress {
                        allowed_keys: 5,
                        required_weight: weight,
                    }),
                    gas_limit: Some(100 * u64::from(weight)),
                })
                .collect(),
        };
        let result = backend
            .simulate_batch(
                RpcRequestContext::unbounded(),
                request,
                1000,
                ViewBatchConfig {
                    max_response_bytes: 256,
                    ..Default::default()
                },
            )
            .await;
        if floor == 42 {
            let result = result.unwrap();
            assert_eq!(result.results.len(), 2);
            assert_eq!(*result.state_block_height, 42);
            for (index, item) in result.results.iter().enumerate() {
                assert_eq!(item.gas_used, 300 + 100 * u64::try_from(index).unwrap());
                assert!(
                    matches!(&item.output,responses::InvocationOutput::Return { return_data } if return_data.0 == vec![u8::try_from(index).unwrap() + 3])
                );
            }
        } else {
            assert!(matches!(
                result,
                Err(RpcServerError::ViewHeightUnavailable(_))
            ));
        }
    }
    task.abort();
}
