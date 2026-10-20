use serde_json::json;
use thruput_json_rpc_types::BlockHeight;
use thruput_json_rpc_types::transaction::{ConflictCode, TransactionStatus};
use thruput_trace::{DbConflictCode, DbConflictDiagnostic, TxFailureDiagnostic};
use thruput_xdr::types::{AddressAndKey, ContractAddress, StorageKey};

fn diagnostic(code: DbConflictCode) -> TxFailureDiagnostic {
    TxFailureDiagnostic {
        attempt_height: BlockHeight::from(1234),
        cause: thruput_trace::TxFailureCause::TxFailureDbConflict {
            conflict: DbConflictDiagnostic {
                addrkey: AddressAndKey {
                    address: ContractAddress::from([0x11; 32]),
                    key: StorageKey::FreeNativeTokenBalance,
                },
                code,
            },
        },
    }
}

#[test]
fn recent_failure_serializes_camel_case() {
    let status = TransactionStatus::from(&diagnostic(DbConflictCode::ExclusiveLockDenied));

    let value = serde_json::to_value(&status).expect("recent failure should serialize");

    assert_eq!(value["status"], "recentFailure");
    assert_eq!(value["attemptHeight"], "1234");
    assert_eq!(
        value["cause"]["conflict"]["address"],
        thruput_encoding::encode(&[0x11; 32])
    );
    let expected_key = format!(
        "0x{}",
        hex::encode(
            xdrs::txs::encode(&StorageKey::FreeNativeTokenBalance)
                .expect("storage key should encode")
        )
    );
    assert_eq!(value["cause"]["conflict"]["key"], expected_key);
    assert_eq!(value["cause"]["conflict"]["code"], "exclusiveLock");
    assert!(value.get("attempt_height").is_none());
}

#[test]
fn conflict_code_maps_all_known_codes() {
    let cases = [
        (DbConflictCode::ExclusiveLockDenied, "exclusiveLock"),
        (DbConflictCode::TypeMismatch, "typeMismatch"),
        (DbConflictCode::ValueMismatch, "valueMismatch"),
        (DbConflictCode::BoundsExceeded, "boundsExceeded"),
        (DbConflictCode::Overflow, "overflow"),
        (DbConflictCode::ReplayProtection, "replayProtection"),
        (DbConflictCode::QueueConsumer, "queueConsumer"),
        (DbConflictCode::CapacityExceeded, "capacityExceeded"),
    ];
    for (code, expected) in cases {
        let json =
            serde_json::to_value(ConflictCode::from(code)).expect("conflict code should serialize");
        assert_eq!(json, *expected, "wrong projection for {code}");
    }
}

#[test]
fn unknown_conflict_code_maps_to_other() {
    let json = serde_json::to_value(ConflictCode::from(DbConflictCode(99)))
        .expect("unknown conflict code should serialize");
    assert_eq!(json, "other");
}

#[test]
fn conflict_code_deserializes_unknown_string_as_other() {
    let code: ConflictCode =
        serde_json::from_value(json!("someFutureCode")).expect("unknown code should decode");
    assert_eq!(code, ConflictCode::Other);
}

#[test]
fn transaction_status_deserializes_unknown_tag_as_other() {
    let status: TransactionStatus = serde_json::from_value(json!({"status": "someFutureStatus"}))
        .expect("unknown status tag should decode");
    assert!(
        matches!(status, TransactionStatus::Other),
        "expected Other, got {status:?}"
    );
}

#[test]
fn contract_rejection_preserves_attachment_index_and_all_codes() {
    use thruput_trace::{ContractRejectCode as XdrCode, ContractRejectDiagnostic, TxFailureCause};
    let codes = [
        (XdrCode::ContractRejectTooMany, "tooMany"),
        (XdrCode::ContractRejectBlobTooLarge, "blobTooLarge"),
        (XdrCode::ContractRejectNotDeployable, "notDeployable"),
        (XdrCode::ContractRejectDuplicate, "duplicate"),
        (
            XdrCode::ContractRejectMalformedArtifact,
            "malformedArtifact",
        ),
        (
            XdrCode::ContractRejectUnsupportedFormat,
            "unsupportedFormat",
        ),
        (XdrCode::ContractRejectUnsupportedAbi, "unsupportedAbi"),
        (
            XdrCode::ContractRejectGasScheduleMismatch,
            "gasScheduleMismatch",
        ),
        (XdrCode::ContractRejectMemoryTooLarge, "memoryTooLarge"),
        (XdrCode::ContractRejectCodeSizeInvalid, "codeSizeInvalid"),
        (
            XdrCode::ContractRejectInitialMemoryInvalid,
            "initialMemoryInvalid",
        ),
        (XdrCode::ContractRejectDigestMismatch, "digestMismatch"),
        (XdrCode::ContractRejectInvalidProgram, "invalidProgram"),
        (XdrCode(99), "other"),
    ];
    for (code, expected) in codes {
        let diagnostic = TxFailureDiagnostic {
            attempt_height: 1234.into(),
            cause: TxFailureCause::TxFailureContractRejected {
                contract: ContractRejectDiagnostic {
                    contract_index: 3,
                    code,
                },
            },
        };
        let status = TransactionStatus::from(&diagnostic);
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(
            value,
            json!({"status":"recentFailure", "attemptHeight":"1234",
            "cause":{"kind":"contractRejected", "contractIndex":3, "code":expected}})
        );
        let roundtrip: TransactionStatus = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), value);
    }
}

#[test]
fn unknown_failure_cause_and_contract_code_decode_as_other() {
    use thruput_json_rpc_types::transaction::{ContractRejectCode, TransactionFailureCause};
    assert!(matches!(
        serde_json::from_value::<TransactionFailureCause>(json!({"kind":"futureCause"})).unwrap(),
        TransactionFailureCause::Other
    ));
    assert_eq!(
        serde_json::from_value::<ContractRejectCode>(json!("futureCode")).unwrap(),
        ContractRejectCode::Other
    );
}

#[test]
fn scheduler_failure_statuses_preserve_source_and_code() {
    use thruput_json_rpc_types::transaction::{
        SchedulerTransactionError, SchedulerTransactionErrorSource,
    };
    for (status, tag) in [
        (
            TransactionStatus::Retry {
                error: SchedulerTransactionError::new(
                    SchedulerTransactionErrorSource::Scheduler,
                    1,
                ),
            },
            "retry",
        ),
        (
            TransactionStatus::Dropped {
                error: SchedulerTransactionError::new(SchedulerTransactionErrorSource::Auth, -2),
            },
            "dropped",
        ),
    ] {
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["status"], tag);
        let decoded: TransactionStatus = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        assert!(value["error"]["code"].is_i64());
        assert!(
            serde_json::from_value::<SchedulerTransactionError>(
                serde_json::json!({"source":"auth", "code":1, "extra":true})
            )
            .is_err()
        );
    }
}
