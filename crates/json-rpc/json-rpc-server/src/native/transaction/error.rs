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
            assert!(
                matches!(
                    DecodedError::decode("sendTransaction", raw.clone()),
                    DecodedError::Known { .. }
                ),
                "{raw}"
            );
        }
    }
}
