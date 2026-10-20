use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::{
    BlockHeight, error::native::BackendDataInvalidReason as Reason, hex::HexBytes,
    requests as input, responses as out, view as legacy,
};
use thruput_view::{ViewGatewayBatchQuery, ViewGatewayBatchResult, ViewGatewayResult};
use thruput_xdr::{event::TxEmittedEvent, transaction::InvocationFrameOutput};
use xdr_common::VecM;

use super::{height, invalid};

fn auth(value: &input::AssumedAuth) -> Result<legacy::ViewAuthClaim, RpcServerError> {
    Ok(match value {
        input::AssumedAuth::PubkeySignature {
            pubkey_hash,
            account_nonce,
        } => legacy::ViewAuthClaim::PubkeySignature {
            pubkey_hash: *pubkey_hash,
            nonce: *account_nonce,
        },
        input::AssumedAuth::SourceAddress {
            allowed_keys,
            required_weight,
        } => legacy::ViewAuthClaim::SourceAddress {
            allowed_keys: *allowed_keys,
            required_weight: *required_weight,
        },
        _ => {
            return Err(RpcServerError::InvalidParam(
                "unsupported assumed authorization".into(),
            ));
        }
    })
}

/// Reuse the internal query builder without a JSON conversion or lost fields.
pub fn simulation_request(
    value: input::SimulateInvocationRequest,
) -> Result<legacy::ViewRequest, RpcServerError> {
    let input::SimulateInvocationRequest {
        address,
        entrypoint,
        calldata,
        caller,
        assumed_auth,
        gas_limit,
        min_state_block_height,
    } = value;
    Ok(legacy::ViewRequest {
        contract: address,
        entrypoint,
        calldata,
        caller,
        auth_claim: assumed_auth.as_ref().map(auth).transpose()?,
        gas_limit,
        min_height: min_state_block_height,
    })
}

/// Preserve input order and the group freshness floor for the existing builder.
pub fn simulations_request(
    value: input::SimulateInvocationsRequest,
) -> Result<legacy::ViewBatchRequest, RpcServerError> {
    let input::SimulateInvocationsRequest {
        invocations,
        min_state_block_height,
    } = value;
    Ok(legacy::ViewBatchRequest {
        min_height: min_state_block_height,
        calls: invocations
            .into_iter()
            .map(|value| {
                let input::Invocation {
                    address,
                    entrypoint,
                    calldata,
                    caller,
                    assumed_auth,
                    gas_limit,
                } = value;
                Ok(legacy::ViewCall {
                    contract: address,
                    entrypoint,
                    calldata,
                    caller,
                    auth_claim: assumed_auth.as_ref().map(auth).transpose()?,
                    gas_limit,
                })
            })
            .collect::<Result<_, RpcServerError>>()?,
    })
}

fn output(value: InvocationFrameOutput) -> Result<out::InvocationOutput, RpcServerError> {
    Ok(match value {
        InvocationFrameOutput::Return { return_buffer } => out::InvocationOutput::Return {
            return_data: HexBytes(return_buffer.0),
        },
        InvocationFrameOutput::Abort { abort_buffer } => out::InvocationOutput::Abort {
            abort_data: HexBytes(abort_buffer.0),
        },
        InvocationFrameOutput::Fault => out::InvocationOutput::Fault {},
        _ => {
            return Err(invalid(
                Reason::UnsupportedValue,
                "unsupported invocation output",
            ));
        }
    })
}

fn events<const MAX: u32>(
    value: VecM<TxEmittedEvent, MAX>,
) -> Result<Vec<out::EmittedEvent>, RpcServerError> {
    let parsed = crate::event::parse_emitted_events(value);
    if parsed.dropped != 0 {
        return Err(invalid(
            Reason::InvalidEncoding,
            "malformed simulation event",
        ));
    }
    Ok(parsed
        .events
        .into_iter()
        .map(|event| {
            let thruput_json_rpc_types::events::EmittedEvent {
                contract_address,
                topics,
                data,
            } = event;
            out::EmittedEvent {
                address: contract_address,
                topics,
                data,
            }
        })
        .collect())
}

/// Decode one invocation atomically. No malformed event is silently omitted.
pub fn simulation(
    result: ViewGatewayResult,
    min_height: Option<BlockHeight>,
) -> Result<out::SimulationResult, RpcServerError> {
    match result {
        ViewGatewayResult::ViewGatewayOk { payload } => {
            height(&payload.height)?;
            let output = output(payload.output)?;
            let events = events(payload.events)?;
            if let Some(requested) = min_height.filter(|requested| payload.height < *requested) {
                return Err(RpcServerError::ViewHeightUnavailable(
                    legacy::ViewHeightUnavailable {
                        requested,
                        observed: payload.height,
                    },
                ));
            }
            Ok(out::SimulationResult {
                state_block_height: payload.height,
                output,
                events,
                gas_used: payload.gas_consumed,
            })
        }
        ViewGatewayResult::ViewGatewayInvalidInput { error_code } => Err(
            RpcServerError::InvalidParam(format!("simulation refused: {error_code}")),
        ),
        ViewGatewayResult::ViewGatewayAbandoned | ViewGatewayResult::ViewGatewayInternalError => {
            Err(RpcServerError::BackendUnavailable(
                "simulation could not complete".into(),
            ))
        }
        _ => Err(invalid(
            Reason::UnsupportedValue,
            "unsupported simulation result",
        )),
    }
}

/// Validate the whole shared-snapshot answer before returning any invocation.
pub fn simulations(
    query: &ViewGatewayBatchQuery,
    result: ViewGatewayBatchResult,
) -> Result<out::SimulationResults, RpcServerError> {
    use thruput_view::ViewGatewayBatchOutcome as Outcome;
    match result {
        ViewGatewayBatchResult::ViewGatewayBatchOk { payload } => {
            height(&payload.height)?;
            if payload.results.0.len() != query.calls.0.len() {
                return Err(invalid(
                    Reason::InvalidCardinality,
                    "simulation result count differs from invocation count",
                ));
            }
            if query
                .min_height
                .as_ref()
                .is_some_and(|min| payload.height < *min)
            {
                return Err(invalid(
                    Reason::InconsistentResult,
                    "batch success is below its requested snapshot floor",
                ));
            }
            let results = payload
                .results
                .0
                .into_iter()
                .map(|value| {
                    Ok(out::InvocationResult {
                        output: output(value.output)?,
                        events: events(value.events)?,
                        gas_used: value.gas_consumed,
                    })
                })
                .collect::<Result<_, RpcServerError>>()?;
            Ok(out::SimulationResults {
                state_block_height: payload.height,
                results,
            })
        }
        ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
            height_unavailable: value,
        } => {
            height(&value.observed)?;
            if query.min_height.as_ref() != Some(&value.requested)
                || value.observed >= value.requested
            {
                return Err(invalid(
                    Reason::InconsistentResult,
                    "invalid simulation freshness evidence",
                ));
            }
            Err(RpcServerError::ViewHeightUnavailable(
                legacy::ViewHeightUnavailable {
                    requested: value.requested,
                    observed: value.observed,
                },
            ))
        }
        ViewGatewayBatchResult::Default { outcome }
            if outcome == Outcome::ViewGatewayBatchDeadlineExceeded.0 =>
        {
            Err(RpcServerError::BackendTimeout)
        }
        ViewGatewayBatchResult::Default { outcome }
            if outcome == Outcome::ViewGatewayBatchInvalidRequest.0
                || outcome == Outcome::ViewGatewayBatchResponseTooLarge.0 =>
        {
            Err(RpcServerError::InvalidParam(
                "simulation batch exceeds backend limits".into(),
            ))
        }
        // A backend can explicitly report an internal availability failure.
        ViewGatewayBatchResult::Default { outcome }
            if outcome == Outcome::ViewGatewayBatchInternalError.0
                || outcome == Outcome::ViewGatewayBatchAbandoned.0 =>
        {
            Err(RpcServerError::BackendUnavailable(
                "simulation batch failed".into(),
            ))
        }
        _ => Err(invalid(
            Reason::UnsupportedValue,
            "unsupported simulation batch outcome",
        )),
    }
}

#[cfg(test)]
mod tests;
