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
    types::TimePoint,
    view::TransactionViewQuery,
};
use xdr_common::{BytesM, VecM};

/// Build a batch with the remaining caller budget, rounded down to milliseconds.
/// Every call runs at `block_timepoint`.
pub fn view_batch_query(
    request: thruput_json_rpc_types::view::ViewBatchRequest,
    gas_limit: u64,
    remaining: std::time::Duration,
    limits: thruput_json_rpc_server::ViewBatchConfig,
    block_timepoint: &TimePoint,
) -> Result<thruput_view::ViewGatewayBatchQuery, RpcServerError> {
    let budget_millis = budget_millis(remaining)?;
    Ok(thruput_view::ViewGatewayBatchQuery {
        calls: VecM(
            request
                .calls
                .into_iter()
                .map(|call| {
                    let call_gas = call.gas_limit.unwrap_or(gas_limit);
                    view_query(call.into(), call_gas, block_timepoint.clone())
                })
                .collect::<Result<_, _>>()?,
        ),
        min_height: request.min_height,
        budget_millis,
        max_response_bytes: limits.max_response_bytes,
    })
}

/// Restate a built batch at `block_timepoint` for a retry, with the caller's
/// remaining budget, so a retry does not rebuild the calls.
pub fn restate_view_batch(
    query: &mut thruput_view::ViewGatewayBatchQuery,
    block_timepoint: &TimePoint,
    remaining: std::time::Duration,
) -> Result<(), RpcServerError> {
    query.budget_millis = budget_millis(remaining)?;
    for call in &mut query.calls.0 {
        call.block_timepoint = block_timepoint.clone();
    }
    Ok(())
}

/// The caller's remaining budget, rounded down to milliseconds. None left is
/// a timeout.
fn budget_millis(remaining: std::time::Duration) -> Result<u32, RpcServerError> {
    let millis = remaining.as_millis().min(u128::from(u32::MAX));
    match u32::try_from(millis) {
        Ok(0) | Err(_) => Err(RpcServerError::BackendTimeout),
        Ok(millis) => Ok(millis),
    }
}

/// Translate a public view request into hogd's query shape.
/// [`crate::native::simulation`] checks minimum height against the response.
///
/// hogd runs the view at exactly `block_timepoint`, the value the contract
/// reads with `get_block_timepoint()`, and refuses one earlier than its
/// snapshot tip's (see [`view_timepoint_refused`]).
pub fn view_query(
    request: ViewRequest,
    gas_limit: u64,
    block_timepoint: TimePoint,
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
        block_timepoint,
    })
}

/// Whether a stated view timepoint may have been refused for preceding the
/// tip of hogd's snapshot: true when `tip` is later than `stated`.
///
/// hogd reports that refusal as invalid input, like other invalid requests,
/// and picks its snapshot after the caller picks the timepoint, so a block
/// committed in between can refuse a timepoint that was valid when stated.
/// Callers retry once at a timepoint at or after `tip` when this holds.
pub fn view_timepoint_refused(stated: &TimePoint, tip: &TimePoint) -> bool {
    **tip > **stated
}

/// The wall clock as a block timepoint (Unix nanoseconds).
pub fn wall_clock_timepoint() -> TimePoint {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    TimePoint::new(u64::try_from(nanos).unwrap_or(u64::MAX))
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
            &thruput_xdr::types::TimePoint::new(1_700_000_000_000_000_000),
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
        assert_eq!(*query.calls.0[0].block_timepoint, 1_700_000_000_000_000_000);
    }

    #[test]
    fn a_restated_batch_keeps_its_calls_at_a_new_time_and_budget() {
        use thruput_xdr::types::TimePoint;
        let mut restated = query();
        restate_view_batch(
            &mut restated,
            &TimePoint::new(5),
            Duration::from_millis(250),
        )
        .unwrap();
        let original = query();
        assert_eq!(restated.budget_millis, 250);
        assert_eq!(*restated.calls.0[0].block_timepoint, 5);
        assert_eq!(
            restated.calls.0[0].invocation,
            original.calls.0[0].invocation
        );
        assert!(matches!(
            restate_view_batch(&mut restated, &TimePoint::new(6), Duration::ZERO),
            Err(RpcServerError::BackendTimeout)
        ));
    }

    #[test]
    fn a_timepoint_is_refused_only_behind_the_tip() {
        use thruput_xdr::types::TimePoint;
        let stated = TimePoint::new(100);
        assert!(view_timepoint_refused(&stated, &TimePoint::new(101)));
        assert!(!view_timepoint_refused(&stated, &TimePoint::new(100)));
        assert!(!view_timepoint_refused(&stated, &TimePoint::new(99)));
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
        let timepoint = thruput_xdr::types::TimePoint::new(7);
        let query = view_batch_query(
            request.clone(),
            123,
            Duration::from_mins(1),
            limits,
            &timepoint,
        )
        .unwrap();
        assert_eq!(query.budget_millis, 60_000);
        assert_eq!(query.max_response_bytes, 321);
        assert!(matches!(
            view_batch_query(request, 123, Duration::ZERO, limits, &timepoint),
            Err(RpcServerError::BackendTimeout)
        ));
    }
}
