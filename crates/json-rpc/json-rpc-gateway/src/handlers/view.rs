//! Native simulation handlers backed by hogd `ViewGateway`.

use std::time::{Duration, Instant};

use thruput_hogd_backend::view as shared_view;
use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};
use thruput_view::{ViewGatewayBatchOutcome, ViewGatewayBatchResult, ViewGatewayResult};
use thruput_xdr::{types::TimePoint, view::TransactionViewQuery};

use crate::{config::HealthBackend, context::GatewayContext, metrics::GatewayMetrics};

use super::{VIEW_GATEWAY, backend_deadline, timed_backend_call};

async fn batch_result(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: thruput_json_rpc_types::view::ViewBatchRequest,
    gas_limit: u64,
    limits: thruput_json_rpc_server::ViewBatchConfig,
    block_timepoint: &TimePoint,
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
        block_timepoint,
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
                *outcome == ViewGatewayBatchOutcome::ViewGatewayBatchInvalidRequest
                    || *outcome == ViewGatewayBatchOutcome::ViewGatewayBatchResponseTooLarge
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

/// A hosted view runs at the gateway's wall clock: validators stamp blocks
/// with theirs, so it is normally at or after the tip. hogd refuses a view
/// stated before its snapshot's tip as invalid input. When that may be why,
/// return the tip's timepoint so the caller can retry once at it.
async fn tip_after(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    stated: &TimePoint,
) -> Option<TimePoint> {
    let tip = ctx
        .tip_coalescer
        .tip_reading(context)
        .await
        .ok()?
        .block_timepoint?;
    shared_view::view_timepoint_refused(stated, &tip).then_some(tip)
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

/// Simulate one invocation through hogd `ViewGateway`.
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
    let request = thruput_hogd_backend::native::simulation_request(request)?;
    let stated = shared_view::wall_clock_timepoint();
    let query = shared_view::view_query(request.clone(), gas_limit, stated.clone())?;
    let timeout =
        Duration::from_millis(ctx.config.view_timeout_ms).min(ctx.config.backend_request_timeout());
    let deadline = backend_deadline(context, timeout)?;
    let mut result = view_result_from_client(&client, &ctx.metrics, &query, deadline).await?;
    if matches!(result, ViewGatewayResult::ViewGatewayInvalidInput { .. })
        && let Some(tip) = tip_after(ctx, context, &stated).await
    {
        let retry = shared_view::view_query(request, gas_limit, tip)?;
        result = view_result_from_client(&client, &ctx.metrics, &retry, deadline).await?;
    }
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
    let request = thruput_hogd_backend::native::simulations_request(request)?;
    let stated = shared_view::wall_clock_timepoint();
    let (mut query, mut result) =
        batch_result(ctx, context, request.clone(), gas_limit, limits, &stated).await?;
    if matches!(
        result,
        ViewGatewayBatchResult::Default { outcome }
            if outcome == ViewGatewayBatchOutcome::ViewGatewayBatchInvalidRequest
    ) && let Some(tip) = tip_after(ctx, context, &stated).await
    {
        (query, result) = batch_result(ctx, context, request, gas_limit, limits, &tip).await?;
    }
    thruput_hogd_backend::native::simulations(&query, result)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use rpc_driver::{FixedSerialPool, ProgramError, RpcServer, ServiceMap};
    use thruput_json_rpc_types::ContractAddress;
    use thruput_json_rpc_types::hex::HexBytes;
    use thruput_json_rpc_types::view::ViewRequest;
    use thruput_scp::network::ProtocolCommit as Protocol;
    use thruput_view::{
        ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Client,
        ViewGateway_ViewGateway1_Dispatcher, ViewGatewayIntroOutcome, ViewGatewayPayload,
    };
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
        /// Refuse views stated before this timepoint, as hogd refuses one
        /// before its snapshot's tip, and return the timepoint a view ran at.
        RefuseBefore(u64),
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
            if let FakeViewGatewayMode::BatchOutcome(outcome) = self.mode {
                return Ok(ViewGatewayBatchResult::Default {
                    outcome: thruput_view::ViewGatewayBatchOutcome(outcome),
                });
            }
            if let FakeViewGatewayMode::RefuseBefore(tip) = self.mode {
                if query.calls.0.iter().any(|call| *call.block_timepoint < tip) {
                    return Ok(ViewGatewayBatchResult::Default {
                        outcome: ViewGatewayBatchOutcome::ViewGatewayBatchInvalidRequest,
                    });
                }
                let results =
                    query
                        .calls
                        .0
                        .iter()
                        .map(|call| thruput_xdr::view::TransactionViewResult {
                            output: InvocationFrameOutput::Return {
                                return_buffer: BytesM(call.block_timepoint.to_be_bytes().to_vec()),
                            },
                            gas_consumed: call.invocation.gas_limit,
                            events: VecM(vec![]),
                        });
                return Ok(ViewGatewayBatchResult::ViewGatewayBatchOk {
                    payload: ViewGatewayBatchPayload {
                        height: 42.into(),
                        results: VecM(results.collect()),
                    },
                });
            }
            if self.mode == FakeViewGatewayMode::Slow {
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Ok(ViewGatewayBatchResult::Default {
                    outcome: thruput_view::ViewGatewayBatchOutcome(6),
                });
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
            let mut return_buffer = vec![1, 2, 3];
            if let FakeViewGatewayMode::RefuseBefore(tip) = self.mode {
                if *query.block_timepoint < tip {
                    return Ok(ViewGatewayResult::ViewGatewayInvalidInput {
                        error_code: thruput_xdr::syscall_error::SyscallErrorCode(7),
                    });
                }
                return_buffer = query.block_timepoint.to_be_bytes().to_vec();
            }

            Ok(ViewGatewayResult::ViewGatewayOk {
                payload: ViewGatewayPayload {
                    gas_consumed: query.invocation.gas_limit,
                    height: 42.into(),
                    output: InvocationFrameOutput::Return {
                        return_buffer: BytesM(return_buffer),
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

    #[tokio::test]
    async fn batch_metrics_separate_expected_refusals_from_backend_failures() {
        use crate::{config::GatewayConfig, context::BackendClient};
        use std::sync::Arc;
        use thruput_json_rpc_server::ViewBatchConfig;
        use thruput_json_rpc_types::{error::codes, requests as input};
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
            let error = native_simulate_batch(
                &context,
                RpcRequestContext::unbounded(),
                input::SimulateInvocationsRequest {
                    invocations: vec![input::Invocation {
                        address: request(None).contract,
                        entrypoint: HexBytes(vec![8; 8]),
                        calldata: HexBytes(vec![]),
                        caller: None,
                        assumed_auth: None,
                        gas_limit: Some(123),
                    }],
                    min_state_block_height: Some(43.into()),
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

    #[tokio::test]
    async fn view_gateway_rpc_timeout_maps_to_backend_timeout() {
        let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::Slow).await;
        let socket = FixedSerialPool::connect(&addr, 1)
            .await
            .expect("bug: fake ViewGateway should connect");
        let client = ViewGateway_ViewGateway1_Client::new(std::sync::Arc::new(socket));
        let query =
            shared_view::view_query(request(None), 123, TimePoint::new(7)).expect("query maps");
        let metrics = GatewayMetrics::for_tests();

        let result = view_result_from_client(
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
        crate::backend::GatewayBackend::new(std::sync::Arc::new(native_view_context(addr).await))
    }

    async fn native_view_context(addr: SocketAddr) -> GatewayContext {
        use crate::{config::GatewayConfig, context::BackendClient};
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let socket = FixedSerialPool::connect(&addr, 1).await.unwrap();
        let client = ViewGateway_ViewGateway1_Client::new(Arc::new(socket));
        GatewayContext::assemble(
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
        )
    }

    /// A tip source whose every reading has the tip at `timepoint`.
    struct TipAt(u64);

    #[async_trait::async_trait]
    impl crate::tip_coalescer::TipSink for TipAt {
        async fn get_tip(
            &self,
            _deadline: Instant,
        ) -> Result<thruput_json_rpc_server::TipReading, RpcServerError> {
            let tip = thruput_json_rpc_types::block::ChainTip {
                height: thruput_json_rpc_types::BlockHeight::new(42),
                block_hash: thruput_json_rpc_types::base58::Base58([0; 32].into()),
            };
            let config = thruput_xdr::network_config::NetworkConfig::_0 {
                v0: thruput_xdr::network_config::NetworkConfigV0 {
                    params: VecM(vec![]),
                },
            };
            Ok(thruput_json_rpc_server::TipReading::new(tip, config)
                .with_block_timepoint(TimePoint::new(self.0)))
        }
    }

    /// A gateway backend whose view gateway refuses views stated before
    /// `refuse_before`, with the chain tip at `tip`.
    async fn refusing_backend(
        refuse_before: u64,
        tip: u64,
    ) -> (
        crate::backend::GatewayBackend,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let (addr, fake) =
            start_fake_view_gateway(FakeViewGatewayMode::RefuseBefore(refuse_before)).await;
        let mut context = native_view_context(addr).await;
        let sink: std::sync::Arc<dyn crate::tip_coalescer::TipSink> =
            std::sync::Arc::new(TipAt(tip));
        context.tip_coalescer = crate::tip_coalescer::TipCoalescer::spawn(
            &crate::config::TipConfig::default(),
            &sink,
            GatewayMetrics::for_tests(),
        );
        let backend = crate::backend::GatewayBackend::new(std::sync::Arc::new(context));
        (backend, fake)
    }

    fn simulate_request() -> thruput_json_rpc_types::requests::SimulateInvocationRequest {
        thruput_json_rpc_types::requests::SimulateInvocationRequest {
            address: thruput_json_rpc_types::base58::Base58(
                thruput_xdr::types::ContractAddress::from([7; 32]),
            ),
            entrypoint: HexBytes(vec![8; 8]),
            calldata: HexBytes(vec![1, 2, 3]),
            caller: None,
            assumed_auth: None,
            gas_limit: Some(100),
            min_state_block_height: None,
        }
    }

    fn returned_timepoint(output: &thruput_json_rpc_types::responses::InvocationOutput) -> u64 {
        let thruput_json_rpc_types::responses::InvocationOutput::Return { return_data } = output
        else {
            panic!("bug: the fake returns its timepoint, got {output:?}");
        };
        u64::from_be_bytes(return_data.0.as_slice().try_into().unwrap())
    }

    /// A tip in 2096: later than any wall clock this test runs at.
    const FUTURE_TIP: u64 = 4_000_000_000_000_000_000;

    #[tokio::test]
    async fn a_view_refused_behind_a_newer_tip_is_retried_once_at_the_tip() {
        use thruput_json_rpc_server::native::read::NativeSimulationPlane;
        let (backend, fake) = refusing_backend(FUTURE_TIP, FUTURE_TIP).await;

        let result = backend
            .simulate(RpcRequestContext::unbounded(), simulate_request(), 100)
            .await
            .expect("bug: the retry at the tip is accepted");
        assert_eq!(returned_timepoint(&result.output), FUTURE_TIP);

        let batch = thruput_json_rpc_types::requests::SimulateInvocationsRequest {
            invocations: vec![thruput_json_rpc_types::requests::Invocation {
                address: thruput_json_rpc_types::base58::Base58(
                    thruput_xdr::types::ContractAddress::from([7; 32]),
                ),
                entrypoint: HexBytes(vec![8; 8]),
                calldata: HexBytes(vec![1]),
                caller: None,
                assumed_auth: None,
                gas_limit: Some(100),
            }],
            min_state_block_height: None,
        };
        let results = backend
            .simulate_batch(
                RpcRequestContext::unbounded(),
                batch,
                100,
                thruput_json_rpc_server::ViewBatchConfig::default(),
            )
            .await
            .expect("bug: the batch retry at the tip is accepted");
        assert_eq!(returned_timepoint(&results.results[0].output), FUTURE_TIP);
        fake.abort();
    }

    #[tokio::test]
    async fn a_refusal_the_tip_does_not_explain_is_returned_as_is() {
        use thruput_json_rpc_server::native::read::NativeSimulationPlane;
        // Refuses every view, while the tip is at the epoch: a retry cannot help.
        let (backend, fake) = refusing_backend(u64::MAX, 0).await;
        assert!(matches!(
            backend
                .simulate(RpcRequestContext::unbounded(), simulate_request(), 100)
                .await,
            Err(RpcServerError::InvalidParam(_))
        ));
        fake.abort();
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
            // Only the slow gateway gets a short deadline; a tight one would also
            // cut off a healthy reply on a loaded machine.
            let context = if mode == FakeViewGatewayMode::Slow {
                RpcRequestContext::new(Some(Duration::from_millis(50)))
            } else {
                RpcRequestContext::unbounded()
            };
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
