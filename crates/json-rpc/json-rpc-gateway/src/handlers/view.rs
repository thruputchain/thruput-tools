//! `groundhog_view` handler backed by hogd `ViewGateway`.

use std::time::{Duration, Instant};

use groundhog_hogd_backend::view as shared_view;
use groundhog_json_rpc_server::{RpcRequestContext, RpcServerError};
use groundhog_json_rpc_types::view::{ViewRequest, ViewResponse};
use groundhog_view::{ViewGatewayBatchOutcome, ViewGatewayBatchResult, ViewGatewayResult};
use groundhog_xdr::view::TransactionViewQuery;

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
    request: groundhog_json_rpc_types::view::ViewBatchRequest,
    gas_limit: u64,
    limits: groundhog_json_rpc_server::ViewBatchConfig,
) -> Result<groundhog_json_rpc_types::view::ViewBatchResponse, RpcServerError> {
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
    if request.calls.iter().any(|call| call.auth_claim.is_some()) {
        check_auth_claim_support(&client, &ctx.metrics, deadline).await?;
    }
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
    shared_view::view_batch_response(VIEW_GATEWAY, &query, result?)
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

    let explicit_auth = request.auth_claim.is_some();
    let query = shared_view::view_query(request, gas_limit)?;
    let timeout =
        Duration::from_millis(ctx.config.view_timeout_ms).min(ctx.config.backend_request_timeout());
    let deadline = backend_deadline(context, timeout)?;
    if explicit_auth {
        check_auth_claim_support(&client, &ctx.metrics, deadline).await?;
    }
    view_response_from_client(&client, &ctx.metrics, &query, deadline).await
}

async fn check_auth_claim_support(
    client: &groundhog_view::ViewGateway_ViewGateway1_Client,
    metrics: &GatewayMetrics,
    deadline: Instant,
) -> Result<(), RpcServerError> {
    let outcome = timed_backend_call(
        metrics,
        HealthBackend::ViewGateway,
        "viewgateway_intro",
        VIEW_GATEWAY,
        deadline,
        client.viewgateway_intro(&shared_view::VIEW_AUTH_CLAIMS_PROTOCOL),
        |_| true,
    )
    .await?;
    shared_view::require_auth_claim_support(outcome)
}

async fn view_response_from_client(
    client: &groundhog_view::ViewGateway_ViewGateway1_Client,
    metrics: &GatewayMetrics,
    query: &TransactionViewQuery,
    deadline: Instant,
) -> Result<ViewResponse, RpcServerError> {
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
    view_response_from_gateway_result(result?, metrics)
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use async_trait::async_trait;
    use groundhog_json_rpc_types::ContractAddress;
    use groundhog_json_rpc_types::hex::HexBytes;
    use groundhog_json_rpc_types::view::RootInvocationOutput;
    use groundhog_scp::network::ProtocolCommit as Protocol;
    use groundhog_view::{
        ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Client,
        ViewGateway_ViewGateway1_Dispatcher, ViewGatewayIntroOutcome, ViewGatewayPayload,
    };
    use groundhog_xdr::event::TxEmittedEvent;
    use groundhog_xdr::transaction::InvocationFrameOutput;
    use groundhog_xdr::types::ContractAddress as XdrContractAddress;
    use rpc_driver::{FixedSerialPool, ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;
    use xdr_common::{BytesM, VecM};

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeViewGatewayMode {
        Slow,
        Batch,
        Legacy,
        BatchOutcome(i32),
    }

    struct FakeViewGateway {
        mode: FakeViewGatewayMode,
    }

    #[async_trait]
    impl ViewGateway_ViewGateway1 for FakeViewGateway {
        async fn viewgateway_view_batch(
            &self,
            query: groundhog_view::ViewGatewayBatchQuery,
        ) -> Result<groundhog_view::ViewGatewayBatchResult, ProgramError> {
            use groundhog_view::{
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
                            .map(|call| groundhog_xdr::view::TransactionViewResult {
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
            protocol: Protocol,
        ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
            if self.mode == FakeViewGatewayMode::Legacy
                && protocol == shared_view::VIEW_AUTH_CLAIMS_PROTOCOL
            {
                return Ok(ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol);
            }
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

    #[tokio::test]
    async fn explicit_auth_probe_distinguishes_legacy_and_capable_backends() {
        for mode in [FakeViewGatewayMode::Legacy, FakeViewGatewayMode::Batch] {
            let (addr, server) = start_fake_view_gateway(mode).await;
            let client = groundhog_x23_transport::connect(
                "view",
                &addr.to_string(),
                1,
                ViewGateway_ViewGateway1_Client::new,
            )
            .await
            .unwrap();
            let result = check_auth_claim_support(
                &client,
                &GatewayMetrics::for_tests(),
                Instant::now() + Duration::from_secs(1),
            )
            .await;
            if mode == FakeViewGatewayMode::Legacy {
                assert!(matches!(result, Err(RpcServerError::NotImplemented(_))));
            } else {
                assert!(result.is_ok());
            }
            server.abort();
        }
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
    async fn public_views_preserve_order_gas_height_and_freshness_errors() {
        use crate::{backend::GatewayBackend, config::GatewayConfig, context::BackendClient};
        use groundhog_json_rpc_api::GroundhogApiServer;
        use groundhog_json_rpc_server::{BuildInfo, GroundhogRpcServer, RpcServerConfig};
        use groundhog_json_rpc_types::{NetworkHash, network_info::NetworkInfo};
        use groundhog_xdr::{PROTOCOL_VERSION, types::SHA256Hash};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let (addr, fake) = start_fake_view_gateway(FakeViewGatewayMode::Batch).await;
        let socket = FixedSerialPool::connect(&addr, 1).await.unwrap();
        let client = ViewGateway_ViewGateway1_Client::new(Arc::new(socket));
        let context = Arc::new(GatewayContext::assemble(
            PROTOCOL_VERSION,
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
        ));
        let module = GroundhogRpcServer::unified(
            GatewayBackend::new(context),
            NetworkInfo::new(NetworkHash::from(SHA256Hash::from([0; 32]))),
            RpcServerConfig::default()
                .with_view_timeout_ms(60_000)
                .with_view_batch(groundhog_json_rpc_server::ViewBatchConfig {
                    max_gas: 1_000,
                    max_response_bytes: 256,
                    ..Default::default()
                }),
            BuildInfo::new("test", None),
        )
        .into_rpc();
        for min_height in ["42", "43"] {
            let calls: Vec<_> = [("0x01", "123"), ("0x02", "456")].into_iter().map(|(calldata, gas)| serde_json::json!({
                "contract": format!("0x{}", "07".repeat(32)), "entrypoint": "0x0808080808080808",
                "authClaim": {"kind":"sourceAddress", "allowedKeys":5, "requiredWeight": if calldata == "0x01" { 3 } else { 4 }},
                "calldata": calldata, "gasLimit": gas,
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
    async fn batch_metrics_separate_expected_refusals_from_backend_failures() {
        use crate::{config::GatewayConfig, context::BackendClient};
        use groundhog_json_rpc_server::ViewBatchConfig;
        use groundhog_json_rpc_types::{
            error::codes,
            view::{ViewBatchRequest, ViewCall},
        };
        use groundhog_view::ViewGatewayBatchOutcome as Outcome;
        use std::sync::Arc;
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
                groundhog_xdr::PROTOCOL_VERSION,
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
}
