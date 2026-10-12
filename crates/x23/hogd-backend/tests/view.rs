use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::view::RootInvocationOutput;
use thruput_json_rpc_types::view::ViewRequest;
use thruput_view::ViewGatewayResult;
use thruput_xdr::invoke_flag::InvokeFlag;
use thruput_xdr::syscall_error::SyscallErrorCode;
use thruput_xdr::transaction::InvocationFrameOutput;
use xdr_common::BytesM;
use xdr_common::VecM;

use thruput_json_rpc_types::ContractAddress;
use thruput_view::ViewGatewayPayload;
use thruput_xdr::{event::TxEmittedEvent, types::ContractAddress as XdrContractAddress};

use thruput_hogd_backend::view::*;

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
fn explicit_claim_replaces_only_the_default_auth_context() {
    use thruput_json_rpc_types::view::ViewAuthClaim;
    let mut input = request(None);
    let default = view_query(input.clone(), 123).unwrap();
    input.auth_claim = Some(ViewAuthClaim::SourceAddress {
        allowed_keys: 5,
        required_weight: 3,
    });
    let explicit = view_query(input, 123).unwrap();
    assert_eq!(explicit.invocation, default.invocation);
    assert_eq!(explicit.parent_address, default.parent_address);
    assert_eq!(explicit.invoke_flags, default.invoke_flags);
    assert_ne!(explicit.auth_claim, default.auth_claim);
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
    assert_eq!(query.invoke_flags, InvokeFlag::None.0.cast_unsigned());
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
            gas_consumed: 1234,
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
    assert_eq!(outcome.response.gas_used, 1234);
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
fn invalid_input_is_a_caller_error_naming_the_service_and_code() {
    let result = ViewGatewayResult::ViewGatewayInvalidInput {
        error_code: SyscallErrorCode::kSyscallErrorCodeReentrance,
    };
    let error = view_outcome("hogd-dev ViewGateway", result).unwrap_err();
    assert!(
        matches!(error, RpcServerError::InvalidParam(_)),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("hogd-dev ViewGateway"), "{message}");
    assert!(message.contains("kSyscallErrorCodeReentrance"), "{message}");
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
