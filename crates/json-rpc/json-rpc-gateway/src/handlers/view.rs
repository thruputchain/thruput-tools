//! `groundhog_view` handler backed by hogd `ViewGateway`.

use std::time::{Duration, Instant};

use groundhog_hogd_backend::view as shared_view;
use groundhog_json_rpc_server::{RpcRequestContext, RpcServerError};
use groundhog_json_rpc_types::view::{ViewRequest, ViewResponse};
use groundhog_view::ViewGatewayResult;
use groundhog_xdr::view::TransactionViewQuery;

use crate::{
    config::HealthBackend,
    context::GatewayContext,
    metrics::{GatewayMetrics, REASON_PARSE_FAILED},
};

use super::{VIEW_GATEWAY, backend_deadline, timed_backend_call};

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
        |result| matches!(result, ViewGatewayResult::ViewGatewayOk { .. }),
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
            Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
        }

        async fn viewgateway_view(
            &self,
            _arg0: TransactionViewQuery,
        ) -> Result<ViewGatewayResult, ProgramError> {
            if self.mode == FakeViewGatewayMode::Slow {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }

            Ok(ViewGatewayResult::ViewGatewayOk {
                payload: ViewGatewayPayload {
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
            contract: ContractAddress::from(XdrContractAddress::from([7; 32])),
            entrypoint: HexBytes(vec![8; 8]),
            calldata: HexBytes(vec![1, 2, 3]),
            caller,
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

    #[test]
    fn malformed_view_events_are_dropped_and_metered() {
        let metrics = GatewayMetrics::for_tests();
        let result = ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
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
