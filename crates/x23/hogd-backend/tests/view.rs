use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::ContractAddress;
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::view::ViewRequest;
use thruput_xdr::invoke_flag::InvokeFlag;
use thruput_xdr::types::ContractAddress as XdrContractAddress;

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
