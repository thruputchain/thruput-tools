use groundhog_json_rpc_types::error::TxInvalidDetail;
use groundhog_json_rpc_types::error::codes;
use jsonrpsee::types::ErrorObjectOwned;

use groundhog_json_rpc_server::error::*;

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
        (RpcServerError::TxNotFound, codes::TX_NOT_FOUND),
        (RpcServerError::BlockNotFound, codes::BLOCK_NOT_FOUND),
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
        let obj: ErrorObjectOwned = err.into();
        assert_eq!(obj.code(), expected_code, "wrong code for `{label}`");
    }
}

#[test]
fn detailed_tx_invalid_carries_structured_data() {
    let err = RpcServerError::TxInvalidDetailed(TxInvalidDetail::ContractTooLarge {
        index: 1,
        size_bytes: 300_000,
        max_bytes: 262_144,
    });

    let obj: ErrorObjectOwned = err.into();

    assert_eq!(obj.code(), codes::TX_INVALID);
    assert_eq!(
        obj.message(),
        "invalid transaction: contract 1 in contractsToDeploy is 300000 bytes; \
             limit is 262144"
    );
    let data: serde_json::Value =
        serde_json::from_str(obj.data().expect("detail should be attached").get()).unwrap();
    assert_eq!(
        data,
        serde_json::json!({
            "reason": "contractTooLarge",
            "index": 1,
            "sizeBytes": 300_000,
            "maxBytes": 262_144,
        })
    );
}

#[test]
fn plain_tx_invalid_carries_no_data() {
    let obj: ErrorObjectOwned = RpcServerError::TxInvalid("invalid hex".into()).into();

    assert_eq!(obj.code(), codes::TX_INVALID);
    assert!(obj.data().is_none());
}
