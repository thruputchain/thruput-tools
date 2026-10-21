use jsonrpsee::types::ErrorObjectOwned;
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

#[test]
fn unresolved_local_funding_carries_evidence_without_asserting_an_outcome() {
    let evidence = thruput_json_rpc_types::airdrop::LocalFundingErrorData::new(
        thruput_json_rpc_types::transaction::TxHash(
            thruput_xdr::types::SHA256Hash::from([0x42; 32]).into(),
        ),
        thruput_json_rpc_types::hex::HexBytes(vec![1, 2, 3]),
    );
    let object: ErrorObjectOwned = RpcServerError::LocalFundingUnresolved {
        cause: None,
        reason: "receipt unavailable".into(),
        evidence: evidence.clone(),
    }
    .into();
    assert_eq!(object.code(), codes::LOCAL_FUNDING_UNRESOLVED);
    assert_eq!(
        serde_json::from_str::<thruput_json_rpc_types::airdrop::LocalFundingErrorData>(
            object.data().unwrap().get()
        )
        .unwrap(),
        evidence
    );
    let mut extended = serde_json::to_value(&evidence).unwrap();
    extended["futureDiagnostic"] = serde_json::json!("additional output metadata");
    assert_eq!(
        serde_json::from_value::<thruput_json_rpc_types::airdrop::LocalFundingErrorData>(extended)
            .unwrap(),
        evidence
    );
}
