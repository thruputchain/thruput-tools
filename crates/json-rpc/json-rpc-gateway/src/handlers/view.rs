//! `groundhog_view` handler backed by hogd `ViewGateway`.

use std::time::{Duration, Instant};

use thruput_hogd_backend::view as shared_view;
use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::view::{ViewRequest, ViewResponse};
use thruput_view::{ViewGatewayBatchOutcome, ViewGatewayBatchResult, ViewGatewayResult};
use thruput_xdr::view::TransactionViewQuery;

use crate::{
    config::HealthBackend,
    context::GatewayContext,
    metrics::{GatewayMetrics, REASON_PARSE_FAILED},
};

use super::{VIEW_GATEWAY, backend_deadline, timed_backend_call};

/// Execute one shared-snapshot batch with a strict public response deadline.
#[tracing::instrument(skip_all)]
pub async fn view_batch(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: thruput_json_rpc_types::view::ViewBatchRequest,
    gas_limit: u64,
    limits: thruput_json_rpc_server::ViewBatchConfig,
) -> Result<thruput_json_rpc_types::view::ViewBatchResponse, RpcServerError> {
    let (query, result) = batch_result(ctx, context, request, gas_limit, limits).await?;
    shared_view::view_batch_response(VIEW_GATEWAY, &query, result)
}

async fn batch_result(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: thruput_json_rpc_types::view::ViewBatchRequest,
    gas_limit: u64,
    limits: thruput_json_rpc_server::ViewBatchConfig,
) -> Result<(thruput_view::ViewGatewayBatchQuery, ViewGatewayBatchResult), RpcServerError> {
    let timeout =
        Duration::from_millis(ctx.config.view_timeout_ms).min(ctx.config.backend_request_timeout());
    let deadline = backend_deadline(context, timeout)?;
    let client = ctx.view_gateway.read().await.clone().ok_or_else(|| {
        if ctx.config.view_gateway_address.is_some() {
            RpcServerError::BackendUnavailable("view_gateway not connected".into())
        } else {
            RpcServerError::NotImplemented("viewBatch".into())
        }
    })?;
    let query = shared_view::view_batch_query(
        request,
        gas_limit,
        deadline.saturating_duration_since(Instant::now()),
        limits,
    )?;
    let result = timed_backend_call(
        &ctx.metrics,
        HealthBackend::ViewGateway,
        "viewgateway_view_batch",
        VIEW_GATEWAY,
        deadline,
        client.viewgateway_view_batch(&query),
        // Expected refusals are healthy backend round trips, even though the
        // public response remains an error for the caller.
        |result| match result {
            ViewGatewayBatchResult::ViewGatewayBatchOk { .. }
            | ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable { .. } => true,
            ViewGatewayBatchResult::Default { outcome } => {
                *outcome == ViewGatewayBatchOutcome::ViewGatewayBatchInvalidRequest.0
                    || *outcome == ViewGatewayBatchOutcome::ViewGatewayBatchResponseTooLarge.0
            }
            _ => false,
        },
    )
    .await;
    if matches!(result, Err(RpcServerError::BackendTimeout)) {
        ctx.metrics.observe_view_timeout();
    }
    Ok((query, result?))
}

/// Execute a sandboxed contract simulation through hogd `ViewGateway`.
#[tracing::instrument(
    skip_all,
    fields(
        ?request.contract,
        entrypoint_len = request.entrypoint.0.len(),
        calldata_len = request.calldata.0.len(),
        caller = ?request.caller,
        gas_limit,
    )
)]
pub async fn view(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: ViewRequest,
    gas_limit: u64,
) -> Result<ViewResponse, RpcServerError> {
    let client = ctx.view_gateway.read().await.clone().ok_or_else(|| {
        if ctx.config.view_gateway_address.is_some() {
            // Startup currently rejects configured-but-unreachable ViewGateway
            // endpoints; this covers future runtime disconnect/reconnect modes.
            RpcServerError::BackendUnavailable("view_gateway not connected".into())
        } else {
            RpcServerError::NotImplemented("view".into())
        }
    })?;

    let query = shared_view::view_query(request, gas_limit)?;
    let timeout =
        Duration::from_millis(ctx.config.view_timeout_ms).min(ctx.config.backend_request_timeout());
    let deadline = backend_deadline(context, timeout)?;
    view_response_from_client(&client, &ctx.metrics, &query, deadline).await
}

async fn view_response_from_client(
    client: &thruput_view::ViewGateway_ViewGateway1_Client,
    metrics: &GatewayMetrics,
    query: &TransactionViewQuery,
    deadline: Instant,
) -> Result<ViewResponse, RpcServerError> {
    view_response_from_gateway_result(
        view_result_from_client(client, metrics, query, deadline).await?,
        metrics,
    )
}

async fn view_result_from_client(
    client: &thruput_view::ViewGateway_ViewGateway1_Client,
    metrics: &GatewayMetrics,
    query: &TransactionViewQuery,
    deadline: Instant,
) -> Result<ViewGatewayResult, RpcServerError> {
    let result = timed_backend_call(
        metrics,
        HealthBackend::ViewGateway,
        "viewgateway_view",
        VIEW_GATEWAY,
        deadline,
        client.viewgateway_view(query),
        // A rejected request is the caller's error, not the backend's: the
        // backend answered correctly, so it counts as a healthy round trip.
        |result| {
            matches!(
                result,
                ViewGatewayResult::ViewGatewayOk { .. }
                    | ViewGatewayResult::ViewGatewayInvalidInput { .. }
            )
        },
    )
    .await;
    if matches!(result, Err(RpcServerError::BackendTimeout)) {
        metrics.observe_view_timeout();
    }
    result
}

/// Decode the `ViewGateway` result and meter any events dropped as malformed.
fn view_response_from_gateway_result(
    result: ViewGatewayResult,
    metrics: &GatewayMetrics,
) -> Result<ViewResponse, RpcServerError> {
    let outcome = shared_view::view_outcome(VIEW_GATEWAY, result)?;
    metrics.observe_malformed_events_dropped(REASON_PARSE_FAILED, outcome.dropped_events);
    Ok(outcome.response)
}

/// Native simulation uses the raw backend result, before legacy event filtering.
#[tracing::instrument(skip_all)]
pub async fn native_simulate(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: thruput_json_rpc_types::requests::SimulateInvocationRequest,
    gas_limit: u64,
) -> Result<thruput_json_rpc_types::responses::SimulationResult, RpcServerError> {
    let client = ctx.view_gateway.read().await.clone().ok_or_else(|| {
        if ctx.config.view_gateway_address.is_some() {
            RpcServerError::BackendUnavailable("view_gateway not connected".into())
        } else {
            RpcServerError::NotImplemented("simulateInvocation".into())
        }
    })?;
    let min = request.min_state_block_height.clone();
    let query = shared_view::view_query(
        thruput_hogd_backend::native::simulation_request(request)?,
        gas_limit,
    )?;
    let timeout =
        Duration::from_millis(ctx.config.view_timeout_ms).min(ctx.config.backend_request_timeout());
    let result = view_result_from_client(
        &client,
        &ctx.metrics,
        &query,
        backend_deadline(context, timeout)?,
    )
    .await?;
    thruput_hogd_backend::native::simulation(result, min)
}

/// Native batches reuse transport policy and retain typed decoder failures.
#[tracing::instrument(skip_all)]
pub async fn native_simulate_batch(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: thruput_json_rpc_types::requests::SimulateInvocationsRequest,
    gas_limit: u64,
    limits: thruput_json_rpc_server::ViewBatchConfig,
) -> Result<thruput_json_rpc_types::responses::SimulationResults, RpcServerError> {
    let (query, result) = batch_result(
        ctx,
        context,
        thruput_hogd_backend::native::simulations_request(request)?,
        gas_limit,
        limits,
    )
    .await?;
    thruput_hogd_backend::native::simulations(&query, result)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use async_trait::async_trait;
    use rpc_driver::{FixedSerialPool, ProgramError, RpcServer, ServiceMap};
    use thruput_json_rpc_types::ContractAddress;
    use thruput_json_rpc_types::hex::HexBytes;
    use thruput_json_rpc_types::view::RootInvocationOutput;
    use thruput_scp::network::ProtocolCommit as Protocol;
    use thruput_view::{
        ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Client,
        ViewGateway_ViewGateway1_Dispatcher, ViewGatewayIntroOutcome, ViewGatewayPayload,
    };
    use thruput_xdr::event::TxEmittedEvent;
    use thruput_xdr::transaction::InvocationFrameOutput;
    use thruput_xdr::types::ContractAddress as XdrContractAddress;
    use tokio::net::TcpListener;
    use xdr_common::{BytesM, VecM};

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeViewGatewayMode {
        Slow,
        Batch,
        BatchOutcome(i32),
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
            if let FakeViewGatewayMode::BatchOutcome(outcome) = self.mode {
                return Ok(ViewGatewayBatchResult::Default { outcome });
            }
            if self.mode == FakeViewGatewayMode::Slow {
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Ok(ViewGatewayBatchResult::Default { outcome: 6 });
            }
            assert!(query.budget_millis > 30_000);
            assert_eq!(query.max_response_bytes, 256);
            if let Some(requested) = query.min_height.filter(|height| **height > 42) {
                return Ok(ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
                    height_unavailable: ViewGatewayHeightUnavailable {
                        requested,
                        observed: 42.into(),
                    },
                });
            }
            assert_ne!(
                query.calls.0[0].auth_claim,
                shared_view::public_view_auth_claim()
            );
            assert_ne!(query.calls.0[0].auth_claim, query.calls.0[1].auth_claim);
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
            Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
        }

        async fn viewgateway_view(
            &self,
            query: TransactionViewQuery,
        ) -> Result<ViewGatewayResult, ProgramError> {
            if self.mode == FakeViewGatewayMode::Slow {
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
            contract: ContractAddress::from(XdrContractAddress::from([7; 32])),
            entrypoint: HexBytes(vec![8; 8]),
            calldata: HexBytes(vec![1, 2, 3]),
            caller,
            gas_limit: None,
        }
    }

    #[test]
    fn gateway_return_maps_to_public_view_response() {
        let metrics = GatewayMetrics::for_tests();
        let topic0 = [10; 32];
        let topic1 = [11; 32];
        let data = vec![12, 13, 14];
        let mut serialized_event = Vec::new();
        serialized_event.extend_from_slice(&topic0);
        serialized_event.extend_from_slice(&topic1);
        serialized_event.extend_from_slice(&data);
        let result = ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
                gas_consumed: 0,
                height: 42.into(),
                output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(vec![1, 2, 3]),
                },
                events: VecM(vec![TxEmittedEvent {
                    contract_address: XdrContractAddress::from([15; 32]),
                    num_topics: 2,
                    serialized_event: BytesM(serialized_event),
                }]),
            },
        };

        let response = view_response_from_gateway_result(result, &metrics).expect("ok response");

        assert_eq!(response.height, 42.into());
        match response.output {
            RootInvocationOutput::Return { return_data } => {
                assert_eq!(return_data.0, vec![1, 2, 3]);
            }
            other => panic!("unexpected output: {other:?}"),
        }
        assert_eq!(response.events.len(), 1);
        assert_eq!(response.events[0].contract_address.0.as_ref(), &[15; 32]);
        assert_eq!(response.events[0].topics[0].0, topic0);
        assert_eq!(response.events[0].topics[1].0, topic1);
        assert_eq!(response.events[0].data.0, data);
    }

    #[tokio::test]
    async fn batch_metrics_separate_expected_refusals_from_backend_failures() {
        use crate::{config::GatewayConfig, context::BackendClient};
        use std::sync::Arc;
        use thruput_json_rpc_server::ViewBatchConfig;
        use thruput_json_rpc_types::{
            error::codes,
            view::{ViewBatchRequest, ViewCall},
        };
        use thruput_view::ViewGatewayBatchOutcome as Outcome;
        use tokio::sync::RwLock;

        let cases = [
            (
                FakeViewGatewayMode::Batch,
                codes::VIEW_HEIGHT_UNAVAILABLE,
                "ok",
            ),
            (
                FakeViewGatewayMode::BatchOutcome(Outcome::ViewGatewayBatchInvalidRequest.0),
                codes::INVALID_PARAMS,
                "ok",
            ),
            (
                FakeViewGatewayMode::BatchOutcome(Outcome::ViewGatewayBatchResponseTooLarge.0),
                codes::INVALID_PARAMS,
                "ok",
            ),
            (
                FakeViewGatewayMode::BatchOutcome(Outcome::ViewGatewayBatchInternalError.0),
                codes::BACKEND_UNAVAILABLE,
                "error",
            ),
            (
                FakeViewGatewayMode::BatchOutcome(Outcome::ViewGatewayBatchAbandoned.0),
                codes::BACKEND_UNAVAILABLE,
                "error",
            ),
            (
                FakeViewGatewayMode::BatchOutcome(Outcome::ViewGatewayBatchDeadlineExceeded.0),
                codes::BACKEND_TIMEOUT,
                "error",
            ),
            (
                FakeViewGatewayMode::BatchOutcome(99),
                codes::BACKEND_UNAVAILABLE,
                "error",
            ),
        ];
        for (mode, code, outcome) in cases {
            let (addr, fake) = start_fake_view_gateway(mode).await;
            let socket = FixedSerialPool::connect(&addr, 1).await.unwrap();
            let client = ViewGateway_ViewGateway1_Client::new(Arc::new(socket));
            let context = GatewayContext::assemble(
                thruput_xdr::PROTOCOL_VERSION,
                Arc::new(RwLock::new(None)),
                Arc::new(RwLock::new(None)),
                Arc::new(RwLock::new(Some(BackendClient::new(
                    client.clone(),
                    client,
                )))),
                Arc::new(RwLock::new(None)),
                Arc::new(GatewayConfig {
                    view_timeout_ms: 60_000,
                    request_timeout_ms: 90_000,
                    ..Default::default()
                }),
                GatewayMetrics::for_tests(),
            );
            let error = view_batch(
                &context,
                RpcRequestContext::unbounded(),
                ViewBatchRequest {
                    calls: vec![ViewCall {
                        auth_claim: None,
                        contract: request(None).contract,
                        entrypoint: HexBytes(vec![8; 8]),
                        calldata: HexBytes(vec![]),
                        caller: None,
                        gas_limit: Some(123),
                    }],
                    min_height: Some(43.into()),
                },
                1_000,
                ViewBatchConfig {
                    max_response_bytes: 256,
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
            fake.abort();
            assert_eq!(error.code(), code, "{mode:?}");
            assert_batch_metrics(&context.metrics, outcome);
        }
    }

    fn assert_batch_metrics(metrics: &GatewayMetrics, outcome: &str) {
        use crate::health::{GatewayHealthSnapshot, HealthStatus};

        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics.gather(&snapshot).unwrap();
        assert!(body.contains(&format!(
            "groundhog_gateway_backend_requests_total{{backend=\"view_gateway\",operation=\"viewgateway_view_batch\",outcome=\"{outcome}\"}} 1"
        )), "{body}");
        assert!(body.contains(&format!(
            "groundhog_gateway_backend_request_duration_seconds_count{{backend=\"view_gateway\",operation=\"viewgateway_view_batch\",result=\"{outcome}\"}} 1"
        )), "{body}");
        let other = if outcome == "ok" { "error" } else { "ok" };
        assert!(!body.contains(&format!(
            "groundhog_gateway_backend_requests_total{{backend=\"view_gateway\",operation=\"viewgateway_view_batch\",outcome=\"{other}\"}} 1"
        )), "{body}");
    }

    #[test]
    fn malformed_view_events_are_dropped_and_metered() {
        let metrics = GatewayMetrics::for_tests();
        let result = ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
                gas_consumed: 0,
                height: 7.into(),
                output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(vec![]),
                },
                events: VecM(vec![TxEmittedEvent {
                    contract_address: XdrContractAddress::from([1; 32]),
                    // Declares three topics but carries one topic's worth of bytes.
                    num_topics: 3,
                    serialized_event: BytesM(vec![0; 32]),
                }]),
            },
        };

        let response = view_response_from_gateway_result(result, &metrics).expect("ok response");

        assert!(response.events.is_empty());
        assert_eq!(metrics.malformed_events_dropped(REASON_PARSE_FAILED), 1);
    }

    #[test]
    fn gateway_service_failures_map_to_backend_errors() {
        let metrics = GatewayMetrics::for_tests();
        assert!(matches!(
            view_response_from_gateway_result(ViewGatewayResult::ViewGatewayAbandoned, &metrics),
            Err(RpcServerError::BackendUnavailable(_))
        ));
        assert!(matches!(
            view_response_from_gateway_result(
                ViewGatewayResult::ViewGatewayInternalError,
                &metrics
            ),
            Err(RpcServerError::BackendUnavailable(_))
        ));
        assert!(matches!(
            view_response_from_gateway_result(ViewGatewayResult::Default { outcome: 99 }, &metrics),
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn view_gateway_rpc_timeout_maps_to_backend_timeout() {
        let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::Slow).await;
        let socket = FixedSerialPool::connect(&addr, 1)
            .await
            .expect("bug: fake ViewGateway should connect");
        let client = ViewGateway_ViewGateway1_Client::new(std::sync::Arc::new(socket));
        let query = shared_view::view_query(request(None), 123).expect("query maps");
        let metrics = GatewayMetrics::for_tests();

        let result = view_response_from_client(
            &client,
            &metrics,
            &query,
            Instant::now() + Duration::from_millis(10),
        )
        .await;

        assert!(
            matches!(result, Err(RpcServerError::BackendTimeout)),
            "ViewGateway timeout should surface BackendTimeout, got {result:?}"
        );
    }
    async fn native_view_backend(addr: SocketAddr) -> crate::backend::GatewayBackend {
        use crate::{config::GatewayConfig, context::BackendClient};
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let socket = FixedSerialPool::connect(&addr, 1).await.unwrap();
        let client = ViewGateway_ViewGateway1_Client::new(Arc::new(socket));
        crate::backend::GatewayBackend::new(Arc::new(GatewayContext::assemble(
            thruput_xdr::PROTOCOL_VERSION,
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(Some(BackendClient::new(
                client.clone(),
                client,
            )))),
            Arc::new(RwLock::new(None)),
            Arc::new(GatewayConfig {
                view_timeout_ms: 60_000,
                request_timeout_ms: 90_000,
                ..Default::default()
            }),
            GatewayMetrics::for_tests(),
        )))
    }

    #[tokio::test]
    async fn native_gateway_simulation_preserves_snapshot_and_deadline() {
        use thruput_json_rpc_server::native::read::NativeSimulationPlane;
        use thruput_json_rpc_types::{requests, responses};
        for mode in [FakeViewGatewayMode::Batch, FakeViewGatewayMode::Slow] {
            let (addr, fake) = start_fake_view_gateway(mode).await;
            let backend = native_view_backend(addr).await;
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
            let context = RpcRequestContext::new(Some(Duration::from_millis(50)));
            let result = backend.simulate(context, request, 100).await;
            if mode == FakeViewGatewayMode::Slow {
                assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
            } else {
                let result = result.unwrap();
                assert_eq!(*result.state_block_height, 42);
                assert_eq!(result.gas_used, 100);
                assert!(matches!(
                    result.output,
                    responses::InvocationOutput::Return { return_data } if return_data.0 == [1, 2, 3]
                ));
            }
            fake.abort();
        }
    }

    #[tokio::test]
    async fn native_gateway_batch_uses_existing_transport_and_preserves_freshness() {
        use thruput_json_rpc_server::{ViewBatchConfig, native::read::NativeSimulationPlane};
        use thruput_json_rpc_types::{requests as input, responses};
        let (addr, fake) = start_fake_view_gateway(FakeViewGatewayMode::Batch).await;
        let backend = native_view_backend(addr).await;
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
                    assert!(matches!(
                        &item.output,
                        responses::InvocationOutput::Return { return_data }
                            if return_data.0 == [u8::try_from(index).unwrap() + 3]
                    ));
                }
            } else {
                assert!(matches!(
                    result,
                    Err(RpcServerError::ViewHeightUnavailable(_))
                ));
            }
        }
        fake.abort();
    }
}
