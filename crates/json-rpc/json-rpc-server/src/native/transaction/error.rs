use super::{NativeReadError, SubmissionAttempt};
use crate::RpcServerError;
use jsonrpsee::types::ErrorObjectOwned;
use thruput_json_rpc_types::{
    error::{
        TxInvalidDetail,
        native::{
            AttemptAdmission, BackendDataInvalidReason, BoundDetail, ErrorData,
            ExecutorBoundDetail, Reason, TransactionInvalidReason, TransactionRejectedReason,
            codes,
        },
    },
    transaction::TxHash,
};

pub(crate) fn read_error(error: NativeReadError) -> ErrorObjectOwned {
    if let RpcServerError::BackendDataInvalid { reason, message } = error.error {
        return ErrorObjectOwned::owned(
            codes::BACKEND_DATA_INVALID,
            format!("backend data invalid: {message}"),
            Some(ErrorData::BackendDataInvalid {
                admission: None,
                transaction_hash: None,
                reason: Some(Reason::Known(reason)),
                observation: error.observation.map(|observation| *observation),
            }),
        );
    }
    super::super::read::rpc_error(&error.error)
}

pub(crate) fn submit_error(
    error: &RpcServerError,
    attempt: &SubmissionAttempt,
    hash: Option<TxHash>,
) -> ErrorObjectOwned {
    let admission = attempt.cancel();
    if matches!(error, RpcServerError::TxRejected(_))
        && !matches!(
            admission,
            AttemptAdmission::NotAttempted | AttemptAdmission::Rejected
        )
    {
        // A rejection is definite only with evidence of the refusal; a sent
        // attempt whose refusal was not recorded may still have been admitted.
        return submit_error(
            &RpcServerError::invalid_data(
                BackendDataInvalidReason::InconsistentResult,
                "rejection without a recorded refusal",
            ),
            attempt,
            hash,
        );
    }
    let (code, data) = match error {
        RpcServerError::TxInvalid(_) | RpcServerError::TxInvalidDetailed(_) => (
            codes::TRANSACTION_INVALID,
            ErrorData::TransactionInvalid {
                admission,
                transaction_hash: hash,
                reason: matches!(error, RpcServerError::TxInvalidDetailed(_)).then_some(
                    Reason::Known(TransactionInvalidReason::ExecutorBoundViolation),
                ),
                detail: match error {
                    RpcServerError::TxInvalidDetailed(detail) => {
                        bound(detail).map(BoundDetail::Known)
                    }
                    _ => None,
                },
            },
        ),
        RpcServerError::TxRejected(_) => (
            codes::TRANSACTION_REJECTED,
            ErrorData::TransactionRejected {
                admission,
                transaction_hash: hash,
                reason: match admission {
                    AttemptAdmission::NotAttempted => {
                        Some(Reason::Known(TransactionRejectedReason::Expired))
                    }
                    AttemptAdmission::Rejected => Some(Reason::Known(
                        TransactionRejectedReason::AuthenticationRejected,
                    )),
                    _ => None,
                },
                error: None,
            },
        ),
        RpcServerError::BackendTimeout => (
            codes::BACKEND_TIMEOUT,
            ErrorData::BackendTimeout {
                admission: Some(admission),
                transaction_hash: hash,
                retry_after_ms: None,
            },
        ),
        RpcServerError::Overloaded(_) => (
            codes::OVERLOADED,
            ErrorData::Overloaded {
                admission: Some(admission),
                transaction_hash: hash,
                retry_after_ms: None,
                error: None,
            },
        ),
        RpcServerError::BackendDataInvalid { reason, .. } => (
            codes::BACKEND_DATA_INVALID,
            ErrorData::BackendDataInvalid {
                admission: Some(admission),
                transaction_hash: hash,
                reason: Some(Reason::Known(*reason)),
                observation: None,
            },
        ),
        RpcServerError::BackendUnavailable(_) => (
            codes::BACKEND_UNAVAILABLE,
            ErrorData::BackendUnavailable {
                admission: Some(admission),
                transaction_hash: hash,
                retry_after_ms: None,
                service_code: None,
                error: None,
            },
        ),
        _ => return super::super::read::rpc_error(error),
    };
    let message = match error {
        RpcServerError::BackendDataInvalid { message, .. } => {
            format!("backend data invalid: {message}")
        }
        _ => error.to_string(),
    };
    ErrorObjectOwned::owned(code, message, Some(data))
}

fn bound(detail: &TxInvalidDetail) -> Option<ExecutorBoundDetail> {
    Some(match *detail {
        TxInvalidDetail::TooManyContracts { attached, max } => {
            ExecutorBoundDetail::TooManyContracts { attached, max }
        }
        TxInvalidDetail::ContractTooLarge {
            index,
            size_bytes,
            max_bytes,
        } => ExecutorBoundDetail::ContractTooLarge {
            index,
            size_bytes,
            max_bytes,
        },
        TxInvalidDetail::DuplicateContract { index, first_index } => {
            ExecutorBoundDetail::DuplicateContract { index, first_index }
        }
        TxInvalidDetail::Rv32MemoryTooLarge {
            index,
            memory_bytes,
            max_bytes,
        } => ExecutorBoundDetail::Rv32MemoryTooLarge {
            index,
            memory_bytes,
            max_bytes,
        },
        TxInvalidDetail::Rv32CodeTooLarge {
            index,
            code_bytes,
            max_bytes,
        } => ExecutorBoundDetail::Rv32CodeTooLarge {
            index,
            code_bytes,
            max_bytes,
        },
        TxInvalidDetail::GasLimitTooHigh { gas_limit, max_gas } => {
            ExecutorBoundDetail::GasLimitTooHigh { gas_limit, max_gas }
        }
        _ => return None,
    })
}

pub(crate) fn invalid_receipt() -> NativeReadError {
    RpcServerError::invalid_data(
        BackendDataInvalidReason::InconsistentResult,
        "receipt identity or height does not match the request",
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use thruput_json_rpc_types::{base58::Base58, error::native::DecodedError};

    #[test]
    fn submission_errors_have_method_valid_cause_and_evidence() {
        let hash = TxHash(Base58([1; 32].into()));
        for (cause, dispatched, rejected, code, evidence) in [
            (
                RpcServerError::TxInvalid("invalid encoding".into()),
                false,
                false,
                codes::TRANSACTION_INVALID,
                "notAttempted",
            ),
            (
                RpcServerError::TxRejected("expired".into()),
                false,
                false,
                codes::TRANSACTION_REJECTED,
                "notAttempted",
            ),
            (
                RpcServerError::TxRejected("authentication".into()),
                true,
                true,
                codes::TRANSACTION_REJECTED,
                "rejected",
            ),
            (
                RpcServerError::Overloaded("capacity".into()),
                true,
                true,
                codes::OVERLOADED,
                "rejected",
            ),
            (
                RpcServerError::BackendUnavailable("dependency".into()),
                true,
                true,
                codes::BACKEND_UNAVAILABLE,
                "rejected",
            ),
            (
                RpcServerError::BackendTimeout,
                false,
                false,
                codes::BACKEND_TIMEOUT,
                "notAttempted",
            ),
            (
                RpcServerError::BackendTimeout,
                true,
                false,
                codes::BACKEND_TIMEOUT,
                "unknown",
            ),
            (
                RpcServerError::invalid_data(
                    BackendDataInvalidReason::InvalidCardinality,
                    "invalid batch",
                ),
                true,
                false,
                codes::BACKEND_DATA_INVALID,
                "unknown",
            ),
        ] {
            let attempt = SubmissionAttempt::default();
            if dispatched {
                assert!(attempt.begin_dispatch());
            }
            if rejected {
                attempt.rejected();
            }
            let error = submit_error(&cause, &attempt, Some(hash));
            assert_eq!(error.code(), code);
            let raw = serde_json::to_value(error).unwrap();
            assert_eq!(raw["data"]["admission"], evidence);
            assert_eq!(
                raw["data"]["transactionHash"],
                serde_json::to_value(hash).unwrap()
            );
            assert_known(&raw);
        }
    }

    fn assert_known(raw: &serde_json::Value) {
        assert!(
            matches!(
                DecodedError::decode("sendTransaction", raw.clone()),
                DecodedError::Known { .. }
            ),
            "{raw}"
        );
    }

    #[test]
    fn rejection_reason_follows_attempt_evidence() {
        for (dispatched, reason) in [(false, "expired"), (true, "authenticationRejected")] {
            let attempt = SubmissionAttempt::default();
            if dispatched {
                assert!(attempt.begin_dispatch());
                attempt.rejected();
            }
            let error = submit_error(
                &RpcServerError::TxRejected("refused".into()),
                &attempt,
                None,
            );
            assert_eq!(error.code(), codes::TRANSACTION_REJECTED);
            let raw = serde_json::to_value(error).unwrap();
            assert_eq!(raw["data"]["reason"], reason);
            assert_known(&raw);
        }
    }

    #[test]
    fn rejection_without_a_recorded_refusal_is_inconsistent_backend_data() {
        let attempt = SubmissionAttempt::default();
        assert!(attempt.begin_dispatch());
        let error = submit_error(
            &RpcServerError::TxRejected("refused".into()),
            &attempt,
            None,
        );
        assert_eq!(error.code(), codes::BACKEND_DATA_INVALID);
        assert_eq!(
            error.message(),
            "backend data invalid: rejection without a recorded refusal"
        );
        let raw = serde_json::to_value(error).unwrap();
        assert_eq!(raw["data"]["admission"], "unknown");
        assert_eq!(raw["data"]["reason"], "inconsistentResult");
        assert_known(&raw);
    }

    #[test]
    fn invalid_backend_data_keeps_its_native_message_and_reason() {
        let attempt = SubmissionAttempt::default();
        assert!(attempt.begin_dispatch());
        let error = submit_error(
            &RpcServerError::invalid_data(
                BackendDataInvalidReason::InvalidCardinality,
                "invalid batch",
            ),
            &attempt,
            None,
        );
        assert_eq!(error.code(), codes::BACKEND_DATA_INVALID);
        assert_eq!(error.message(), "backend data invalid: invalid batch");
        let raw = serde_json::to_value(error).unwrap();
        assert_eq!(raw["data"]["reason"], "invalidCardinality");
    }

    #[test]
    fn executor_bound_violations_carry_their_detail() {
        for (detail, expected) in [
            (
                TxInvalidDetail::TooManyContracts {
                    attached: 3,
                    max: 2,
                },
                json!({"reason": "tooManyContracts", "attached": 3, "max": 2}),
            ),
            (
                TxInvalidDetail::ContractTooLarge {
                    index: 1,
                    size_bytes: 9,
                    max_bytes: 8,
                },
                json!({"reason": "contractTooLarge", "index": 1, "sizeBytes": 9, "maxBytes": 8}),
            ),
            (
                TxInvalidDetail::DuplicateContract {
                    index: 2,
                    first_index: 0,
                },
                json!({"reason": "duplicateContract", "index": 2, "firstIndex": 0}),
            ),
            (
                TxInvalidDetail::Rv32MemoryTooLarge {
                    index: 1,
                    memory_bytes: 9,
                    max_bytes: 8,
                },
                json!({"reason": "rv32MemoryTooLarge", "index": 1, "memoryBytes": 9, "maxBytes": 8}),
            ),
            (
                TxInvalidDetail::Rv32CodeTooLarge {
                    index: 1,
                    code_bytes: 9,
                    max_bytes: 8,
                },
                json!({"reason": "rv32CodeTooLarge", "index": 1, "codeBytes": 9, "maxBytes": 8}),
            ),
            (
                TxInvalidDetail::GasLimitTooHigh {
                    gas_limit: 11,
                    max_gas: 10,
                },
                json!({"reason": "gasLimitTooHigh", "gasLimit": "11", "maxGas": "10"}),
            ),
        ] {
            let error = submit_error(
                &RpcServerError::TxInvalidDetailed(detail),
                &SubmissionAttempt::default(),
                None,
            );
            assert_eq!(error.code(), codes::TRANSACTION_INVALID);
            let raw = serde_json::to_value(error).unwrap();
            assert_eq!(raw["data"]["reason"], "executorBoundViolation");
            assert_eq!(raw["data"]["detail"], expected);
            assert_known(&raw);
        }
    }
}
