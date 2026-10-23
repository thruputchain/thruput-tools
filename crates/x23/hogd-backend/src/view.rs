//! Build `ViewGateway` queries for native simulation.
//!
//! A view is a sandboxed root invocation: hogd's viewing mode is what keeps
//! writes from committing, so the query carries no special flags and the
//! auth claim is assumed, never signature-verified. Events are still produced,
//! which lets a wallet preview show what a real submit would emit.

use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::view::ViewRequest;
use thruput_xdr::{
    auth_claim::{AuthClaim, AuthClaim_source_address_claim},
    contract::ContractEntrypointID,
    invoke_flag::InvokeFlag,
    transaction::Invocation,
    view::TransactionViewQuery,
};
use xdr_common::{BytesM, VecM};

/// Build a batch with the remaining caller budget, rounded down to milliseconds.
pub fn view_batch_query(
    request: thruput_json_rpc_types::view::ViewBatchRequest,
    gas_limit: u64,
    remaining: std::time::Duration,
    limits: thruput_json_rpc_server::ViewBatchConfig,
) -> Result<thruput_view::ViewGatewayBatchQuery, RpcServerError> {
    let millis = remaining.as_millis().min(u128::from(u32::MAX));
    let budget_millis = u32::try_from(millis).map_err(|_| RpcServerError::BackendTimeout)?;
    if budget_millis == 0 {
        return Err(RpcServerError::BackendTimeout);
    }
    Ok(thruput_view::ViewGatewayBatchQuery {
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
        auth_claim: request
            .auth_claim
            .map_or_else(public_view_auth_claim, Into::into),
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

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::time::Duration;
    use thruput_json_rpc_types::{hex::HexBytes, view::ViewBatchRequest};

    fn query() -> thruput_view::ViewGatewayBatchQuery {
        view_batch_query(
            ViewBatchRequest {
                calls: vec![thruput_json_rpc_types::view::ViewCall {
                    auth_claim: None,
                    gas_limit: Some(123),
                    contract: thruput_xdr::types::ContractAddress::from([7; 32]).into(),
                    entrypoint: HexBytes(vec![0; 8]),
                    calldata: HexBytes(vec![1]),
                    caller: Some(thruput_xdr::types::ContractAddress::from([8; 32]).into()),
                }],
                min_height: Some(42.into()),
            },
            10_000_000,
            Duration::from_millis(999),
            thruput_json_rpc_server::ViewBatchConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn batch_query_preserves_caller_budget_gas_and_parent() {
        let query = query();
        assert_eq!(query.budget_millis, 999);
        assert_eq!(query.max_response_bytes, 1_048_576);
        assert_eq!(query.calls.0[0].invocation.gas_limit, 123);
        assert_eq!(
            query.calls.0[0].parent_address.as_ref().unwrap().as_ref(),
            &[8; 32]
        );
    }

    #[test]
    fn batch_propagates_operator_budgets_without_a_protocol_timeout_cap() {
        let request = ViewBatchRequest {
            calls: vec![],
            min_height: None,
        };
        let limits = thruput_json_rpc_server::ViewBatchConfig {
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
}
