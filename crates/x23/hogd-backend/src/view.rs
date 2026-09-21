//! Build `ViewGateway` queries and decode their results.
//!
//! A view is a sandboxed root invocation: hogd's viewing mode is what keeps
//! writes from committing, so the query carries no special flags and the
//! auth claim grants nothing. Events are still produced, which is what lets
//! a wallet preview show what a real submit would emit.

use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::{
    hex::HexBytes,
    view::{RootInvocationOutput, ViewRequest, ViewResponse},
};
use groundhog_view::ViewGatewayResult;
use groundhog_xdr::{
    auth_claim::{AuthClaim, AuthClaim_source_address_claim},
    contract::ContractEntrypointID,
    invoke_flag::InvokeFlag,
    transaction::{Invocation, InvocationFrameOutput},
    view::TransactionViewQuery,
};
use xdr_common::{BytesM, VecM};

use crate::event::parse_emitted_events;

/// A decoded view answer and the events it dropped as malformed.
#[derive(Debug)]
pub struct ViewOutcome {
    /// The public response for the caller.
    pub response: ViewResponse,
    /// How many emitted events were dropped as malformed.
    pub dropped_events: usize,
}

/// Translate a public view request into hogd's query shape.
pub fn view_query(
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

/// The auth claim a public, unauthenticated view runs under: no keys
/// allowed, no signature weight required.
pub fn public_view_auth_claim() -> AuthClaim {
    AuthClaim::kSourceAddressAuthData {
        source_address_claim: AuthClaim_source_address_claim {
            allowed_keys: 0,
            required_weight: 0,
        },
    }
}

/// Map a root invocation frame's output onto the public view output.
pub fn root_output(
    service: &str,
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
        // The generated XDR enum is non-exhaustive across protocol updates.
        _ => Err(RpcServerError::BackendUnavailable(format!(
            "{service} returned an unsupported output variant; regenerate the ViewGateway \
             client for this protocol"
        ))),
    }
}

/// Decode a `viewgateway_view` result into the public response.
pub fn view_outcome(
    service: &str,
    result: ViewGatewayResult,
) -> Result<ViewOutcome, RpcServerError> {
    match result {
        ViewGatewayResult::ViewGatewayOk { payload } => {
            let output = root_output(service, payload.output)?;
            let parsed = parse_emitted_events(payload.events);
            Ok(ViewOutcome {
                response: ViewResponse {
                    output,
                    events: parsed.events,
                    height: payload.height,
                },
                dropped_events: parsed.dropped,
            })
        }
        ViewGatewayResult::ViewGatewayAbandoned => Err(RpcServerError::BackendUnavailable(
            format!("{service} abandoned the view after retrying; retry later"),
        )),
        ViewGatewayResult::ViewGatewayInternalError => Err(RpcServerError::BackendUnavailable(
            format!("{service} reported an internal error"),
        )),
        ViewGatewayResult::Default { outcome } => {
            tracing::warn!(outcome, %service, "unexpected ViewGatewayResult outcome");
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned unexpected outcome: {outcome}"
            )))
        }
        // `Default` handles unknown wire discriminants. This arm is still kept
        // for source-level forward compatibility because the generated enum is
        // non-exhaustive and a future protocol crate may add a named variant.
        other => {
            tracing::warn!(result = ?other, %service, "unknown ViewGatewayResult variant");
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned an unsupported result variant; regenerate the ViewGateway \
                 client for this protocol"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::ContractAddress;
    use groundhog_view::ViewGatewayPayload;
    use groundhog_xdr::{event::TxEmittedEvent, types::ContractAddress as XdrContractAddress};

    use super::*;

    fn request(caller: Option<ContractAddress>) -> ViewRequest {
        ViewRequest {
            contract: ContractAddress::from(XdrContractAddress::from([7; 32])),
            entrypoint: HexBytes(vec![8; 8]),
            calldata: HexBytes(vec![1, 2, 3]),
            caller,
        }
    }

    #[test]
    fn view_query_maps_the_public_request_shape() {
        let caller = ContractAddress::from(XdrContractAddress::from([9; 32]));
        let query = view_query(request(Some(caller)), 123).expect("query maps");

        assert_eq!(query.invocation.invoked_address.as_ref(), &[7; 32]);
        assert_eq!(query.invocation.entrypoint.as_ref(), &[8; 8]);
        assert_eq!(query.invocation.gas_limit, 123);
        assert_eq!(query.invocation.calldata.0, vec![1, 2, 3]);
        assert_eq!(query.parent_address.as_ref(), Some(&caller.0));
        assert_eq!(query.forwarded_tokens.0.len(), 0);
        assert_eq!(query.invoke_flags, InvokeFlag::None.0);
        assert_eq!(query.auth_claim, public_view_auth_claim());
    }

    #[test]
    fn view_query_preserves_a_missing_caller() {
        let query = view_query(request(None), 456).expect("query maps");

        assert_eq!(query.parent_address, None);
        assert_eq!(query.invocation.gas_limit, 456);
    }

    #[test]
    fn a_wrong_width_entrypoint_is_a_caller_error() {
        let mut request = request(None);
        request.entrypoint = HexBytes(vec![1, 2, 3]);

        assert!(matches!(
            view_query(request, 1),
            Err(RpcServerError::InvalidParam(_))
        ));
    }

    #[test]
    fn an_ok_result_splits_topics_and_counts_malformed_events() {
        let topic = [10; 32];
        let mut serialized_event = topic.to_vec();
        serialized_event.extend_from_slice(&[12, 13, 14]);
        let result = ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
                height: 42.into(),
                output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(vec![1, 2, 3]),
                },
                events: VecM(vec![
                    TxEmittedEvent {
                        contract_address: XdrContractAddress::from([15; 32]),
                        num_topics: 1,
                        serialized_event: BytesM(serialized_event),
                    },
                    TxEmittedEvent {
                        contract_address: XdrContractAddress::from([16; 32]),
                        num_topics: 3,
                        serialized_event: BytesM(vec![0; 8]),
                    },
                ]),
            },
        };

        let outcome = view_outcome("view_gateway", result).expect("ok response");

        assert_eq!(outcome.response.height, 42.into());
        assert!(matches!(
            outcome.response.output,
            RootInvocationOutput::Return { .. }
        ));
        assert_eq!(outcome.response.events.len(), 1);
        assert_eq!(outcome.response.events[0].topics[0].0, topic);
        assert_eq!(outcome.response.events[0].data.0, vec![12, 13, 14]);
        assert_eq!(outcome.dropped_events, 1);
    }

    #[test]
    fn service_failures_map_to_backend_errors_naming_the_service() {
        for result in [
            ViewGatewayResult::ViewGatewayAbandoned,
            ViewGatewayResult::ViewGatewayInternalError,
            ViewGatewayResult::Default { outcome: 99 },
        ] {
            let error = view_outcome("hogd-dev ViewGateway", result).unwrap_err();
            assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
            assert!(
                error.to_string().contains("hogd-dev ViewGateway"),
                "{error}"
            );
        }
    }

    #[test]
    fn abort_and_fault_are_successful_outputs() {
        let abort = root_output(
            "view_gateway",
            InvocationFrameOutput::Abort {
                abort_buffer: BytesM(vec![4, 5]),
            },
        )
        .expect("abort maps");
        match abort {
            RootInvocationOutput::Abort { abort_data } => assert_eq!(abort_data.0, vec![4, 5]),
            other => panic!("unexpected output: {other:?}"),
        }

        assert!(matches!(
            root_output("view_gateway", InvocationFrameOutput::Fault),
            Ok(RootInvocationOutput::Fault)
        ));
    }
}
