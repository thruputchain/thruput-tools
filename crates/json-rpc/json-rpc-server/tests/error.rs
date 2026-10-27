use thruput_json_rpc_types::error::TxInvalidDetail;
use thruput_json_rpc_types::error::codes;

use thruput_json_rpc_server::error::*;

#[test]
fn every_variant_maps_to_its_documented_code() {
    let cases = [
        (RpcServerError::TxInvalid("x".into()), codes::TX_INVALID),
        (
            RpcServerError::TxInvalidDetailed(TxInvalidDetail::TooManyContracts {
                attached: 5,
                max: 4,
            }),
            codes::TX_INVALID,
        ),
        (RpcServerError::TxRejected("x".into()), codes::TX_REJECTED),
        (
            RpcServerError::InvalidParam("x".into()),
            codes::INVALID_PARAMS,
        ),
        (
            RpcServerError::BackendUnavailable("x".into()),
            codes::BACKEND_UNAVAILABLE,
        ),
        (RpcServerError::BackendTimeout, codes::BACKEND_TIMEOUT),
        (RpcServerError::Overloaded("x".into()), codes::OVERLOADED),
        (
            RpcServerError::NotImplemented("x".into()),
            codes::NOT_IMPLEMENTED,
        ),
    ];

    for (err, expected_code) in cases {
        let label = err.to_string();
        assert_eq!(err.code(), expected_code, "wrong code for `{label}`");
    }
}
