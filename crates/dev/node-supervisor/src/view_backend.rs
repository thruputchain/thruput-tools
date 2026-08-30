//! hogd-dev ViewGateway-backed view plane adapter.

use std::{fmt, net::SocketAddr, time::Duration};

use async_trait::async_trait;
use groundhog_json_rpc_server::{RpcRequestContext, RpcServerError, ViewPlane};
use groundhog_json_rpc_types::{
    events::EmittedEvent,
    hex::{Hex0x, HexBytes, Topic},
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};
use groundhog_scp::network::ProtocolCommit as Protocol;
use groundhog_view::{ViewGateway_ViewGateway1_Client, ViewGatewayIntroOutcome, ViewGatewayResult};
use groundhog_x23_transport::connect;
use groundhog_xdr::{
    auth_claim::{AuthClaim, AuthClaim_source_address_claim},
    contract::ContractEntrypointID,
    event::TxEmittedEvent,
    invoke_flag::InvokeFlag,
    transaction::{Invocation, InvocationFrameOutput},
    view::TransactionViewQuery,
};
use xdr_common::{BytesM, StringM, VecM};

use crate::{
    NodeSupervisorError, request_deadline::within_request_deadline, stub_backend::StubBackend,
};

const HOGD_DEV_POOL_SIZE: usize = 2;
// Keep event parsing in sync with read_backend.rs until the shared helper is
// worth extracting across read and view adapters.
const TOPIC_SIZE: usize = 32;
const MAX_TOPICS: u32 = 3;

/// Local-node view plane, backed by hogd-dev's optional `ViewGateway`.
#[derive(Clone)]
pub(crate) enum HogdDevViewBackend {
    Gateway {
        client: ViewGateway_ViewGateway1_Client,
        request_timeout: Duration,
    },
    Stub(StubBackend),
}

impl fmt::Debug for HogdDevViewBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway { .. } => f.debug_tuple("HogdDevViewBackend::Gateway").finish(),
            Self::Stub(_) => f.debug_tuple("HogdDevViewBackend::Stub").finish(),
        }
    }
}

impl HogdDevViewBackend {
    pub(crate) async fn connect_optional(
        endpoint: Option<SocketAddr>,
        protocol_version: &str,
        expected_protocol: Option<&str>,
        request_timeout: Duration,
    ) -> Result<Self, NodeSupervisorError> {
        let Some(endpoint) = endpoint else {
            tracing::info!(
                "hogd-dev did not advertise a ViewGateway endpoint; view is unavailable"
            );
            return Ok(Self::Stub(StubBackend::new()));
        };

        tracing::debug!(%endpoint, "connecting to hogd-dev ViewGateway");
        let client = connect(
            "hogd-dev ViewGateway",
            &endpoint.to_string(),
            HOGD_DEV_POOL_SIZE,
            ViewGateway_ViewGateway1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::ViewGatewayConnect { endpoint, source })?;

        let protocol: Protocol = StringM(protocol_version.as_bytes().to_vec());
        let intro_outcome = client.viewgateway_intro(&protocol).await.map_err(|e| {
            NodeSupervisorError::ViewGatewayRpc {
                op: "viewgateway_intro",
                error: e.to_string(),
            }
        })?;
        if intro_outcome != ViewGatewayIntroOutcome::ViewGatewayIntroSuccess {
            return Err(NodeSupervisorError::ProtocolIntroRejected {
                service: "ViewGateway",
                outcome: intro_outcome.0,
                outcome_name: view_gateway_intro_outcome_name(intro_outcome),
                is_protocol_mismatch: intro_outcome
                    == ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol,
                protocol: protocol_version.to_string(),
                expected_protocol: expected_protocol.map(str::to_string),
            });
        }

        Ok(Self::Gateway {
            client,
            request_timeout,
        })
    }

    pub(crate) const fn plane_status(&self) -> crate::server::PlaneStatus {
        match self {
            Self::Gateway { .. } => crate::server::PlaneStatus::ViewGateway,
            Self::Stub(_) => crate::server::PlaneStatus::Stub,
        }
    }
}

fn view_gateway_intro_outcome_name(outcome: ViewGatewayIntroOutcome) -> &'static str {
    match outcome.0 {
        value if value == ViewGatewayIntroOutcome::ViewGatewayIntroSuccess.0 => {
            "ViewGatewayIntroSuccess"
        }
        value if value == ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol.0 => {
            "ViewGatewayIntroMismatchedProtocol"
        }
        _ => "UnknownViewGatewayIntroOutcome",
    }
}

#[async_trait]
impl ViewPlane for HogdDevViewBackend {
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
    async fn view(
        &self,
        context: RpcRequestContext,
        request: ViewRequest,
        gas_limit: u64,
    ) -> Result<ViewResponse, RpcServerError> {
        let Self::Gateway {
            client,
            request_timeout,
        } = self
        else {
            return Err(RpcServerError::NotImplemented("view".into()));
        };

        let query = view_query_from_request(request, gas_limit)?;
        let result = within_request_deadline(context.capped(*request_timeout), async {
            client.viewgateway_view(&query).await.map_err(|source| {
                RpcServerError::BackendUnavailable(format!("hogd-dev ViewGateway: {source}"))
            })
        })
        .await?;

        view_response_from_gateway_result(result)
    }
}

fn view_query_from_request(
    request: ViewRequest,
    gas_limit: u64,
) -> Result<TransactionViewQuery, RpcServerError> {
    let entrypoint: [u8; 8] = request.entrypoint.0.as_slice().try_into().map_err(|_| {
        RpcServerError::InvalidParam("view entrypoint must be exactly 8 bytes".into())
    })?;

    Ok(TransactionViewQuery {
        invocation: Invocation {
            invoked_address: request.contract.0,
            entrypoint: ContractEntrypointID::from(entrypoint),
            gas_limit,
            calldata: BytesM(request.calldata.0),
        },
        parent_address: request.caller.map(|caller| caller.0),
        auth_claim: public_view_auth_claim(),
        forwarded_tokens: VecM(vec![]),
        // Root invocations carry no flags (`invoke_flag.x:9-10`). Hogd's
        // `ViewingModeData` is what sandboxes the run — writes don't commit,
        // and `log_event` is permitted so wallet-preview callers can see what
        // events a real submit would produce.
        invoke_flags: InvokeFlag::None.0,
    })
}

fn public_view_auth_claim() -> AuthClaim {
    AuthClaim::kSourceAddressAuthData {
        source_address_claim: AuthClaim_source_address_claim {
            allowed_keys: 0,
            required_weight: 0,
        },
    }
}

fn view_response_from_gateway_result(
    result: ViewGatewayResult,
) -> Result<ViewResponse, RpcServerError> {
    match result {
        ViewGatewayResult::ViewGatewayOk { payload } => Ok(ViewResponse {
            output: root_output_from_invocation_output(payload.output)?,
            events: payload
                .events
                .iter()
                .filter_map(parse_emitted_event)
                .collect(),
            height: payload.height,
        }),
        ViewGatewayResult::ViewGatewayAbandoned => Err(RpcServerError::BackendUnavailable(
            "hogd-dev ViewGateway abandoned the view after retrying; retry later".into(),
        )),
        ViewGatewayResult::ViewGatewayInternalError => Err(RpcServerError::BackendUnavailable(
            "hogd-dev ViewGateway reported an internal error".into(),
        )),
        ViewGatewayResult::Default { outcome } => {
            tracing::warn!(outcome, "unexpected ViewGatewayResult outcome");
            Err(RpcServerError::BackendUnavailable(format!(
                "hogd-dev ViewGateway returned unexpected outcome: {outcome}"
            )))
        }
        // `Default` handles unknown wire discriminants. This arm is still kept
        // for source-level forward compatibility because the generated enum is
        // non-exhaustive and a future protocol crate may add a named variant.
        other => {
            tracing::warn!(result = ?other, "unknown ViewGatewayResult variant");
            Err(RpcServerError::BackendUnavailable(
                "hogd-dev ViewGateway returned an unsupported result variant; regenerate the \
                 ViewGateway client for this protocol"
                    .into(),
            ))
        }
    }
}

fn root_output_from_invocation_output(
    output: InvocationFrameOutput,
) -> Result<RootInvocationOutput, RpcServerError> {
    match output {
        InvocationFrameOutput::Return { return_buffer } => Ok(RootInvocationOutput::Return {
            return_data: HexBytes(return_buffer.0),
        }),
        InvocationFrameOutput::Abort { abort_buffer } => Ok(RootInvocationOutput::Abort {
            abort_data: HexBytes(abort_buffer.0),
        }),
        InvocationFrameOutput::Fault => Ok(RootInvocationOutput::Fault),
        _ => Err(RpcServerError::BackendUnavailable(
            "hogd-dev ViewGateway returned an unsupported output variant; regenerate the \
             ViewGateway client for this protocol"
                .into(),
        )),
    }
}

fn parse_emitted_event(raw: &TxEmittedEvent) -> Option<EmittedEvent> {
    let num_topics = raw.num_topics;
    if num_topics > MAX_TOPICS {
        tracing::warn!(num_topics, "view event has too many topics, skipping");
        return None;
    }

    let bytes: &[u8] = raw.serialized_event.as_ref();
    let topics_len = num_topics as usize * TOPIC_SIZE;
    if bytes.len() < topics_len {
        tracing::warn!(
            num_topics,
            event_len = bytes.len(),
            "view event too short for declared topics, skipping"
        );
        return None;
    }

    let mut topics = Vec::with_capacity(num_topics as usize);
    for i in 0..num_topics as usize {
        let start = i * TOPIC_SIZE;
        let mut topic = [0u8; TOPIC_SIZE];
        topic.copy_from_slice(&bytes[start..start + TOPIC_SIZE]);
        topics.push(Topic(topic));
    }

    Some(EmittedEvent {
        contract_address: Hex0x(raw.contract_address),
        topics,
        data: HexBytes(bytes[topics_len..].to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use async_trait::async_trait;
    use groundhog_json_rpc_server::ViewPlane;
    use groundhog_json_rpc_types::ContractAddress;
    use groundhog_json_rpc_types::hex::HexBytes;
    use groundhog_json_rpc_types::protocol_token::protocol_text;
    use groundhog_scp::network::ProtocolCommit as Protocol;
    use groundhog_view::{
        ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Dispatcher, ViewGatewayPayload,
    };
    use groundhog_xdr::PROTOCOL_VERSION;
    use groundhog_xdr::transaction::InvocationFrameOutput;
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;

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
        }
    }

    #[test]
    fn view_query_maps_public_request_shape() {
        let caller = ContractAddress::from(groundhog_xdr::types::ContractAddress::from([9; 32]));
        let query = view_query_from_request(request(Some(caller)), 123).expect("query maps");

        assert_eq!(query.invocation.invoked_address.as_ref(), &[7; 32]);
        assert_eq!(query.invocation.entrypoint.as_ref(), &[8; 8]);
        assert_eq!(query.invocation.gas_limit, 123);
        assert_eq!(query.invocation.calldata.0, vec![1, 2, 3]);
        assert_eq!(query.parent_address.as_ref(), Some(&caller.0));
        assert_eq!(query.forwarded_tokens.0.len(), 0);
        assert_eq!(query.invoke_flags, InvokeFlag::None.0);
        assert_eq!(
            query.auth_claim,
            AuthClaim::kSourceAddressAuthData {
                source_address_claim: AuthClaim_source_address_claim {
                    allowed_keys: 0,
                    required_weight: 0,
                },
            }
        );
    }

    #[test]
    fn view_query_preserves_missing_caller() {
        let query = view_query_from_request(request(None), 456).expect("query maps");

        assert_eq!(query.parent_address, None);
        assert_eq!(query.invocation.gas_limit, 456);
    }

    #[test]
    fn gateway_return_maps_to_public_view_response() {
        let result = ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
                height: 42.into(),
                output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(vec![1, 2, 3]),
                },
                events: VecM(vec![]),
            },
        };

        let response = view_response_from_gateway_result(result).expect("ok response");

        assert_eq!(response.height, 42.into());
        match response.output {
            RootInvocationOutput::Return { return_data } => {
                assert_eq!(return_data.0, vec![1, 2, 3]);
            }
            other => panic!("unexpected output: {other:?}"),
        }
        assert!(response.events.is_empty());
    }

    #[test]
    fn gateway_abort_and_fault_map_to_success_outputs() {
        let abort = root_output_from_invocation_output(InvocationFrameOutput::Abort {
            abort_buffer: BytesM(vec![4, 5]),
        })
        .expect("abort maps");
        match abort {
            RootInvocationOutput::Abort { abort_data } => assert_eq!(abort_data.0, vec![4, 5]),
            other => panic!("unexpected output: {other:?}"),
        }

        assert!(matches!(
            root_output_from_invocation_output(InvocationFrameOutput::Fault),
            Ok(RootInvocationOutput::Fault)
        ));
    }

    #[test]
    fn gateway_service_failures_map_to_backend_errors() {
        assert!(matches!(
            view_response_from_gateway_result(ViewGatewayResult::ViewGatewayAbandoned),
            Err(RpcServerError::BackendUnavailable(_))
        ));
        assert!(matches!(
            view_response_from_gateway_result(ViewGatewayResult::ViewGatewayInternalError),
            Err(RpcServerError::BackendUnavailable(_))
        ));
        assert!(matches!(
            view_response_from_gateway_result(ViewGatewayResult::Default { outcome: 99 }),
            Err(RpcServerError::BackendUnavailable(_))
        ));
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
            "hogp/sent",
            Some("hogp/expected"),
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
                assert_eq!(protocol, "hogp/sent");
                assert_eq!(expected_protocol.as_deref(), Some("hogp/expected"));
                assert!(message.contains("ViewGateway intro mismatch"));
                assert!(message.contains("hogp/sent"));
                assert!(message.contains("hogp/expected"));
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
}
