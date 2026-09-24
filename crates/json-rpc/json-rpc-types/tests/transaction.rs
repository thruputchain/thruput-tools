use groundhog_json_rpc_types::BlockHeight;
use groundhog_json_rpc_types::transaction::{ConflictCode, TransactionStatus};
use groundhog_trace::{DbConflictCode, DbConflictDiagnostic, TxFailureDiagnostic};
use groundhog_xdr::types::{AddressAndKey, ContractAddress, StorageKey};
use serde_json::json;

fn diagnostic(code: DbConflictCode) -> TxFailureDiagnostic {
    TxFailureDiagnostic {
        attempt_height: BlockHeight::from(1234),
        conflict: DbConflictDiagnostic {
            addrkey: AddressAndKey {
                address: ContractAddress::from([0x11; 32]),
                key: StorageKey::FreeNativeTokenBalance,
            },
            code,
        },
    }
}

#[test]
fn recent_failure_serializes_camel_case() {
    let status = TransactionStatus::from(&diagnostic(DbConflictCode::ExclusiveLockDenied));

    let value = serde_json::to_value(&status).expect("recent failure should serialize");

    assert_eq!(value["status"], "recentFailure");
    assert_eq!(value["attemptHeight"], 1234);
    assert_eq!(
        value["conflict"]["address"],
        format!("0x{}", "11".repeat(32))
    );
    let expected_key = format!(
        "0x{}",
        hex::encode(
            xdrs::txs::encode(&StorageKey::FreeNativeTokenBalance)
                .expect("storage key should encode")
        )
    );
    assert_eq!(value["conflict"]["key"], expected_key);
    assert_eq!(value["conflict"]["code"], "exclusiveLock");
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
        (
            DbConflictCode::ReplayProtection,
            "replayProtection",
        ),
        (DbConflictCode::QueueConsumer, "queueConsumer"),
        (
            DbConflictCode::CapacityExceeded,
            "capacityExceeded",
        ),
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
