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

/// Build a batch with the remaining caller budget, rounded down to milliseconds.
pub fn view_batch_query(
    request: groundhog_json_rpc_types::view::ViewBatchRequest,
    gas_limit: u64,
    remaining: std::time::Duration,
    limits: groundhog_json_rpc_server::ViewBatchConfig,
) -> Result<groundhog_view::ViewGatewayBatchQuery, RpcServerError> {
    let millis = remaining.as_millis().min(u128::from(u32::MAX));
    let budget_millis = u32::try_from(millis).map_err(|_| RpcServerError::BackendTimeout)?;
    if budget_millis == 0 {
        return Err(RpcServerError::BackendTimeout);
    }
    Ok(groundhog_view::ViewGatewayBatchQuery {
        calls: VecM(
            request
                .calls
                .into_iter()
                .map(|call| {
                    let call_gas = call.gas_limit.unwrap_or(gas_limit);
                    view_query(call.into(), call_gas)
                })
                .collect::<Result<_, _>>()?,
        ),
        min_height: request.min_height,
        budget_millis,
        max_response_bytes: limits.max_response_bytes,
    })
}

/// Decode one complete batch. Malformed, incomplete, or stale results fail the group.
pub fn view_batch_response(
    service: &str,
    query: &groundhog_view::ViewGatewayBatchQuery,
    result: groundhog_view::ViewGatewayBatchResult,
) -> Result<groundhog_json_rpc_types::view::ViewBatchResponse, RpcServerError> {
    use groundhog_json_rpc_types::view::{
        ViewBatchResponse, ViewCallResult, ViewHeightUnavailable,
    };
    use groundhog_view::{ViewGatewayBatchOutcome as Outcome, ViewGatewayBatchResult};
    match result {
        ViewGatewayBatchResult::ViewGatewayBatchOk { payload } => {
            if payload.results.0.len() != query.calls.0.len()
                || *payload.height < 0
                || query
                    .min_height
                    .as_ref()
                    .is_some_and(|min| &payload.height < min)
            {
                return Err(RpcServerError::BackendUnavailable(format!(
                    "{service} returned an incomplete or stale batch"
                )));
            }
            let results = payload
                .results
                .0
                .into_iter()
                .map(|result| {
                    let parsed = parse_emitted_events(result.events);
                    if parsed.dropped != 0 {
                        return Err(RpcServerError::BackendUnavailable(format!(
                            "{service} returned malformed batch events"
                        )));
                    }
                    Ok(ViewCallResult {
                        gas_used: result.gas_consumed,
                        output: root_output(service, result.output)?,
                        events: parsed.events,
                    })
                })
                .collect::<Result<_, _>>()?;
            Ok(ViewBatchResponse {
                height: payload.height,
                results,
            })
        }
        ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable { height_unavailable } => Err(
            RpcServerError::ViewHeightUnavailable(ViewHeightUnavailable {
                requested: height_unavailable.requested,
                observed: height_unavailable.observed,
            }),
        ),
        ViewGatewayBatchResult::Default { outcome }
            if outcome == Outcome::ViewGatewayBatchDeadlineExceeded.0 =>
        {
            Err(RpcServerError::BackendTimeout)
        }
        ViewGatewayBatchResult::Default { outcome }
            if outcome == Outcome::ViewGatewayBatchInvalidRequest.0 =>
        {
            Err(RpcServerError::InvalidParam(
                "viewBatch exceeds backend request limits".into(),
            ))
        }
        ViewGatewayBatchResult::Default { outcome }
            if outcome == Outcome::ViewGatewayBatchResponseTooLarge.0 =>
        {
            Err(RpcServerError::InvalidParam(
                "viewBatch response exceeds the endpoint limit; request fewer calls or events"
                    .into(),
            ))
        }
        other => Err(RpcServerError::BackendUnavailable(format!(
            "{service} batch failed: {:?}",
            other.outcome()
        ))),
    }
}

/// A decoded view answer and the events it dropped as malformed.
#[derive(Debug)]
pub struct ViewOutcome {
    /// The public response for the caller.
    pub response: ViewResponse,
    /// How many emitted events were dropped as malformed.
    pub dropped_events: usize,
}

/// Translate a public view request into hogd's query shape.
/// The shared RPC server checks minimum height against the execution response.
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
        invoke_flags: InvokeFlag::None.0.cast_unsigned(),
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
                    gas_used: payload.gas_consumed,
                },
                dropped_events: parsed.dropped,
            })
        }
        // Rejected before any frame ran: deterministic for this request
        // against this state, so it is the caller's to fix, not a retry.
        ViewGatewayResult::ViewGatewayInvalidInput { error_code } => Err(
            RpcServerError::InvalidParam(format!("{service} rejected the view: {error_code}")),
        ),
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
mod batch_tests {
    use super::*;
    use groundhog_json_rpc_types::view::ViewBatchRequest;
    use groundhog_view::{ViewGatewayBatchPayload, ViewGatewayBatchResult};
    use groundhog_xdr::view::TransactionViewResult;
    use std::time::Duration;

    fn query() -> groundhog_view::ViewGatewayBatchQuery {
        view_batch_query(
            ViewBatchRequest {
                calls: vec![groundhog_json_rpc_types::view::ViewCall {
                    gas_limit: Some(123),
                    contract: groundhog_xdr::types::ContractAddress::from([7; 32]).into(),
                    entrypoint: HexBytes(vec![0; 8]),
                    calldata: HexBytes(vec![1]),
                    caller: Some(groundhog_xdr::types::ContractAddress::from([8; 32]).into()),
                }],
                min_height: Some(42.into()),
            },
            10_000_000,
            Duration::from_millis(999),
            groundhog_json_rpc_server::ViewBatchConfig::default(),
        )
        .unwrap()
    }

    fn result(height: i64, outputs: Vec<InvocationFrameOutput>) -> ViewGatewayBatchResult {
        ViewGatewayBatchResult::ViewGatewayBatchOk {
            payload: ViewGatewayBatchPayload {
                height: height.into(),
                results: VecM(
                    outputs
                        .into_iter()
                        .map(|output| TransactionViewResult {
                            gas_consumed: 12,
                            output,
                            events: VecM(vec![]),
                        })
                        .collect(),
                ),
            },
        }
    }

    #[test]
    fn batch_preserves_caller_budget_and_fault_as_a_result() {
        let query = query();
        assert_eq!(query.budget_millis, 999);
        assert_eq!(query.max_response_bytes, 1_048_576);
        assert_eq!(query.calls.0[0].invocation.gas_limit, 123);
        assert_eq!(
            query.calls.0[0].parent_address.as_ref().unwrap().as_ref(),
            &[8; 32]
        );
        let response = view_batch_response(
            "test",
            &query,
            result(42, vec![InvocationFrameOutput::Fault]),
        )
        .unwrap();
        assert_eq!(*response.height, 42);
        assert_eq!(response.results[0].gas_used, 12);
        assert!(matches!(
            response.results[0].output,
            RootInvocationOutput::Fault
        ));
    }

    #[test]
    fn batch_rejects_partial_extra_and_stale_results() {
        for result in [
            result(42, vec![]),
            result(41, vec![InvocationFrameOutput::Fault]),
            result(42, vec![InvocationFrameOutput::Fault; 2]),
        ] {
            assert!(matches!(
                view_batch_response("test", &query(), result),
                Err(RpcServerError::BackendUnavailable(_))
            ));
        }
    }

    #[test]
    fn batch_propagates_operator_budgets_without_a_protocol_timeout_cap() {
        let request = ViewBatchRequest {
            calls: vec![],
            min_height: None,
        };
        let limits = groundhog_json_rpc_server::ViewBatchConfig {
            max_response_bytes: 321,
            ..Default::default()
        };
        let query = view_batch_query(request.clone(), 123, Duration::from_mins(1), limits).unwrap();
        assert_eq!(query.budget_millis, 60_000);
        assert_eq!(query.max_response_bytes, 321);
        assert!(matches!(
            view_batch_query(request, 123, Duration::ZERO, limits),
            Err(RpcServerError::BackendTimeout)
        ));
    }

    #[test]
    fn batch_failure_maps_height_details_and_deadline() {
        let result = ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
            height_unavailable: groundhog_view::ViewGatewayHeightUnavailable {
                requested: 42.into(),
                observed: 41.into(),
            },
        };
        let error = view_batch_response("test", &query(), result).unwrap_err();
        assert!(matches!(error, RpcServerError::ViewHeightUnavailable(_)));
        let result = ViewGatewayBatchResult::Default { outcome: 4 };
        assert!(matches!(
            view_batch_response("test", &query(), result),
            Err(RpcServerError::BackendTimeout)
        ));
    }
}
