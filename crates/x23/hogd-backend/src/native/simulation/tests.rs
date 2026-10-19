use super::*;
use thruput_view::{ViewGatewayBatchPayload, ViewGatewayHeightUnavailable, ViewGatewayPayload};
use thruput_xdr::view::TransactionViewResult;
use xdr_common::BytesM;
fn request() -> input::SimulateInvocationRequest {
    input::SimulateInvocationRequest {
        address: thruput_json_rpc_types::base58::Base58(thruput_xdr::types::ContractAddress::from(
            [3; 32],
        )),
        entrypoint: HexBytes(vec![2; 8]),
        calldata: HexBytes(vec![1, 2]),
        caller: Some(thruput_json_rpc_types::base58::Base58(
            thruput_xdr::types::ContractAddress::from([4; 32]),
        )),
        assumed_auth: Some(input::AssumedAuth::SourceAddress {
            allowed_keys: 5,
            required_weight: 2,
        }),
        gas_limit: Some(123),
        min_state_block_height: Some(42.into()),
    }
}
fn query() -> ViewGatewayBatchQuery {
    crate::view::view_batch_query(
        legacy::ViewBatchRequest {
            calls: vec![legacy::ViewCall {
                contract: thruput_json_rpc_types::base58::Base58(
                    thruput_xdr::types::ContractAddress::from([3; 32]),
                ),
                entrypoint: HexBytes(vec![2; 8]),
                calldata: HexBytes(vec![1]),
                caller: None,
                auth_claim: None,
                gas_limit: None,
            }],
            min_height: Some(42.into()),
        },
        1000,
        std::time::Duration::from_secs(1),
        thruput_json_rpc_server::ViewBatchConfig::default(),
    )
    .unwrap()
}
fn output_value() -> InvocationFrameOutput {
    InvocationFrameOutput::Return {
        return_buffer: BytesM(vec![9]),
    }
}
fn event(malformed: bool) -> TxEmittedEvent {
    TxEmittedEvent {
        contract_address: thruput_xdr::types::ContractAddress::from([7; 32]),
        num_topics: 1,
        serialized_event: BytesM(vec![8; if malformed { 3 } else { 33 }]),
    }
}
fn single(height: i64, malformed: bool) -> ViewGatewayResult {
    ViewGatewayResult::ViewGatewayOk {
        payload: ViewGatewayPayload {
            height: height.into(),
            gas_consumed: 50,
            output: output_value(),
            events: VecM(vec![event(false), event(malformed)]),
        },
    }
}
fn batch(height: i64, count: usize, malformed: bool) -> ViewGatewayBatchResult {
    ViewGatewayBatchResult::ViewGatewayBatchOk {
        payload: ViewGatewayBatchPayload {
            height: height.into(),
            results: VecM(vec![
                TransactionViewResult {
                    gas_consumed: 50,
                    output: output_value(),
                    events: VecM(vec![event(malformed)])
                };
                count
            ]),
        },
    }
}
fn invalid_reason<T: std::fmt::Debug>(result: Result<T, RpcServerError>, expected: Reason) {
    assert!(
        matches!(result.expect_err("invalid backend data must fail"), RpcServerError::BackendDataInvalid { reason, .. } if reason == expected)
    );
}
#[test]
fn native_query_preserves_address_caller_auth_and_budget() {
    let legacy = simulation_request(request()).unwrap();
    assert_eq!(legacy.min_height, Some(42.into()));
    assert_eq!(legacy.gas_limit, Some(123));
    let query = crate::view::view_query(legacy, 123).unwrap();
    assert_eq!(query.invocation.invoked_address.as_ref(), &[3; 32]);
    assert_eq!(query.parent_address.unwrap().as_ref(), &[4; 32]);
    assert_eq!(query.invocation.entrypoint.as_ref(), &[2; 8]);
    assert_eq!(query.invocation.calldata.0, vec![1, 2]);
    assert_ne!(query.auth_claim, crate::view::public_view_auth_claim());
}
#[test]
fn malformed_events_fail_atomically_while_wire13_keeps_its_behavior() {
    invalid_reason(simulation(single(42, true), None), Reason::InvalidEncoding);
    invalid_reason(
        simulations(&query(), batch(42, 1, true)),
        Reason::InvalidEncoding,
    );
    let legacy = crate::view::view_outcome("view", single(42, true)).unwrap();
    assert_eq!(legacy.dropped_events, 1);
    assert_eq!(legacy.response.events.len(), 1);
    let result = simulation(single(42, false), Some(42.into())).unwrap();
    assert_eq!(result.events.len(), 2);
    assert_eq!(result.gas_used, 50);
    assert_eq!(result.events[0].address.0.as_ref(), &[7; 32]);
}
#[test]
fn snapshot_and_cardinality_are_validated_before_returning_results() {
    invalid_reason(
        simulation(single(-2, false), None),
        Reason::InconsistentResult,
    );
    for count in [0, 2] {
        invalid_reason(
            simulations(&query(), batch(42, count, false)),
            Reason::InvalidCardinality,
        );
    }
    invalid_reason(
        simulations(&query(), batch(41, 1, false)),
        Reason::InconsistentResult,
    );
    assert!(matches!(
        simulation(single(41, false), Some(42.into())),
        Err(RpcServerError::ViewHeightUnavailable(_))
    ));
    for (requested, observed) in [(41, 40), (42, 42), (42, -2)] {
        invalid_reason(
            simulations(
                &query(),
                ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
                    height_unavailable: ViewGatewayHeightUnavailable {
                        requested: requested.into(),
                        observed: observed.into(),
                    },
                },
            ),
            Reason::InconsistentResult,
        );
    }
    assert!(matches!(
        simulations(
            &query(),
            ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
                height_unavailable: ViewGatewayHeightUnavailable {
                    requested: 42.into(),
                    observed: 41.into()
                }
            }
        ),
        Err(RpcServerError::ViewHeightUnavailable(_))
    ));
}
#[test]
fn unknown_outcomes_are_invalid_data_and_execution_failures_are_results() {
    invalid_reason(
        simulation(ViewGatewayResult::Default { outcome: 999 }, None),
        Reason::UnsupportedValue,
    );
    invalid_reason(
        simulations(&query(), ViewGatewayBatchResult::Default { outcome: 999 }),
        Reason::UnsupportedValue,
    );
    for output in [
        InvocationFrameOutput::Fault,
        InvocationFrameOutput::Abort {
            abort_buffer: BytesM(vec![4]),
        },
    ] {
        let response = simulation(
            ViewGatewayResult::ViewGatewayOk {
                payload: ViewGatewayPayload {
                    height: 42.into(),
                    gas_consumed: 50,
                    output,
                    events: VecM(vec![]),
                },
            },
            None,
        )
        .unwrap();
        assert_eq!(response.gas_used, 50);
        assert!(!matches!(
            response.output,
            out::InvocationOutput::Return { .. }
        ));
    }
}

#[test]
fn known_batch_failures_preserve_their_causes() {
    use thruput_view::ViewGatewayBatchOutcome as O;
    for outcome in [
        O::ViewGatewayBatchAbandoned,
        O::ViewGatewayBatchInternalError,
    ] {
        assert!(matches!(
            simulations(
                &query(),
                ViewGatewayBatchResult::Default { outcome: outcome.0 }
            ),
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }
    assert!(matches!(
        simulations(
            &query(),
            ViewGatewayBatchResult::Default {
                outcome: O::ViewGatewayBatchDeadlineExceeded.0
            }
        ),
        Err(RpcServerError::BackendTimeout)
    ));
    for outcome in [
        O::ViewGatewayBatchInvalidRequest,
        O::ViewGatewayBatchResponseTooLarge,
    ] {
        assert!(matches!(
            simulations(
                &query(),
                ViewGatewayBatchResult::Default { outcome: outcome.0 }
            ),
            Err(RpcServerError::InvalidParam(_))
        ));
    }
}

#[test]
fn pregenesis_snapshots_are_unavailable_not_corrupt() {
    assert!(matches!(
        simulation(single(-1, false), None),
        Err(RpcServerError::BackendUnavailable(_))
    ));
    assert!(matches!(
        simulations(&query(), batch(-1, 1, false)),
        Err(RpcServerError::BackendUnavailable(_))
    ));
    assert!(matches!(
        simulations(
            &query(),
            ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
                height_unavailable: ViewGatewayHeightUnavailable {
                    requested: 42.into(),
                    observed: (-1).into()
                }
            }
        ),
        Err(RpcServerError::BackendUnavailable(_))
    ));
}
