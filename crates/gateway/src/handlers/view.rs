//! `groundhog_view` handler backed by hogd `ViewGateway`.

use std::time::Duration;

use groundhog_rpc_api_types::{
    hex::HexBytes,
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};
use groundhog_view::{
    AuthClaim, AuthClaim_source_address_claim, ContractEntrypointID, Invocation,
    InvocationFrameOutput, InvokeFlag, TransactionViewQuery, ViewGatewayResult,
};
use xdr_common::{BytesM, VecM};

use crate::{context::GatewayContext, error::GatewayError};

use super::parse_event::parse_emitted_event;

/// Execute a read-only contract invocation through hogd `ViewGateway`.
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
    request: ViewRequest,
    gas_limit: u64,
) -> Result<ViewResponse, GatewayError> {
    let client = ctx.view_gateway.read().await.clone().ok_or_else(|| {
        if ctx.config.view_gateway_address.is_some() {
            GatewayError::BackendUnavailable("view_gateway not connected".into())
        } else {
            GatewayError::NotImplemented("view".into())
        }
    })?;

    let query = view_query_from_request(request, gas_limit)?;
    let timeout = Duration::from_millis(ctx.config.view_timeout_ms);
    view_response_from_client(&client, &query, timeout).await
}

async fn view_response_from_client(
    client: &groundhog_view::ViewGateway_ViewGateway1_Client,
    query: &TransactionViewQuery,
    timeout: Duration,
) -> Result<ViewResponse, GatewayError> {
    let result = tokio::time::timeout(timeout, client.viewgateway_view(query))
        .await
        .map_err(|_| GatewayError::BackendTimeout)?
        .map_err(|source| GatewayError::BackendUnavailable(format!("view_gateway: {source}")))?;

    view_response_from_gateway_result(result)
}

fn view_query_from_request(
    request: ViewRequest,
    gas_limit: u64,
) -> Result<TransactionViewQuery, GatewayError> {
    let entrypoint: [u8; 8] = request.entrypoint.0.as_slice().try_into().map_err(|_| {
        GatewayError::InvalidParam("view entrypoint must be exactly 8 bytes".into())
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
) -> Result<ViewResponse, GatewayError> {
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
        ViewGatewayResult::ViewGatewayAbandoned => Err(GatewayError::BackendUnavailable(
            "view_gateway abandoned the view after retrying; retry later".into(),
        )),
        ViewGatewayResult::ViewGatewayInternalError => Err(GatewayError::BackendUnavailable(
            "view_gateway reported an internal error".into(),
        )),
        ViewGatewayResult::Default { outcome } => {
            tracing::warn!(outcome, "unexpected ViewGatewayResult outcome");
            Err(GatewayError::BackendUnavailable(format!(
                "view_gateway returned unexpected outcome: {outcome}"
            )))
        }
        // `Default` handles unknown wire discriminants. This arm is still kept
        // for source-level forward compatibility because the generated enum is
        // non-exhaustive and a future protocol crate may add a named variant.
        other => {
            tracing::warn!(result = ?other, "unknown ViewGatewayResult variant");
            Err(GatewayError::BackendUnavailable(
                "view_gateway returned an unsupported result variant; regenerate the \
                 ViewGateway client for this protocol"
                    .into(),
            ))
        }
    }
}

fn root_output_from_invocation_output(
    output: InvocationFrameOutput,
) -> Result<RootInvocationOutput, GatewayError> {
    match output {
        InvocationFrameOutput::Return { return_buffer } => Ok(RootInvocationOutput::Return {
            return_data: HexBytes(return_buffer.0),
        }),
        InvocationFrameOutput::Abort { abort_buffer } => Ok(RootInvocationOutput::Abort {
            abort_data: HexBytes(abort_buffer.0),
        }),
        InvocationFrameOutput::Fault => Ok(RootInvocationOutput::Fault),
        // The generated XDR enum is non-exhaustive across protocol updates.
        _ => Err(GatewayError::BackendUnavailable(
            "view_gateway returned an unsupported output variant; regenerate the ViewGateway \
             client for this protocol"
                .into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use async_trait::async_trait;
    use groundhog_rpc_api_types::ContractAddress;
    use groundhog_view::{
        Protocol, ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Client,
        ViewGateway_ViewGateway1_Dispatcher, ViewGatewayIntroOutcome, ViewGatewayPayload,
    };
    use groundhog_xdr::types::ContractAddress as XdrContractAddress;
    use rpc_driver::{FixedSerialPool, ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;

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
    fn view_query_maps_public_request_shape() {
        let caller = ContractAddress::from(XdrContractAddress::from([9; 32]));
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
            Err(GatewayError::BackendUnavailable(_))
        ));
        assert!(matches!(
            view_response_from_gateway_result(ViewGatewayResult::ViewGatewayInternalError),
            Err(GatewayError::BackendUnavailable(_))
        ));
        assert!(matches!(
            view_response_from_gateway_result(ViewGatewayResult::Default { outcome: 99 }),
            Err(GatewayError::BackendUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn view_gateway_rpc_timeout_maps_to_backend_timeout() {
        let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::Slow).await;
        let socket = FixedSerialPool::connect(&addr, 1)
            .await
            .expect("bug: fake ViewGateway should connect");
        let client = ViewGateway_ViewGateway1_Client::new(std::sync::Arc::new(socket));
        let query = view_query_from_request(request(None), 123).expect("query maps");

        let result = view_response_from_client(&client, &query, Duration::from_millis(10)).await;

        assert!(
            matches!(result, Err(GatewayError::BackendTimeout)),
            "ViewGateway timeout should surface BackendTimeout, got {result:?}"
        );
    }
}
