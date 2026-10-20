use thruput_json_rpc_server::native::transaction::NativeReadError;
use thruput_json_rpc_types::{
    base58::Base58, error::native::BackendDataInvalidReason as Reason, hex::HexBytes,
    responses as out,
};
use thruput_trace::{ContractRejectCode as Reject, DbConflictCode as Conflict, TxFailureCause};

use super::invalid;

pub(super) fn cause(value: TxFailureCause) -> Result<out::FailureCause, NativeReadError> {
    Ok(match value {
        TxFailureCause::TxFailureDbConflict { conflict } => out::FailureCause::DbConflict {
            conflict: out::ConflictDiagnostic {
                address: Base58(conflict.addrkey.address),
                key: HexBytes(xdrs::txs::encode(&conflict.addrkey.key).map_err(|_| {
                    invalid(Reason::InvalidEncoding, "unencodable conflict storage key")
                })?),
                code: match conflict.code {
                    Conflict::ExclusiveLockDenied => out::ConflictCode::ExclusiveLock,
                    Conflict::TypeMismatch => out::ConflictCode::TypeMismatch,
                    Conflict::ValueMismatch => out::ConflictCode::ValueMismatch,
                    Conflict::BoundsExceeded => out::ConflictCode::BoundsExceeded,
                    Conflict::Overflow => out::ConflictCode::Overflow,
                    Conflict::ReplayProtection => out::ConflictCode::ReplayProtection,
                    Conflict::QueueConsumer => out::ConflictCode::QueueConsumer,
                    Conflict::CapacityExceeded => out::ConflictCode::CapacityExceeded,
                    Conflict::InsufficientFunds => out::ConflictCode::InsufficientFunds,
                    _ => {
                        return Err(invalid(
                            Reason::UnsupportedValue,
                            "unsupported storage conflict code",
                        )
                        .into());
                    }
                },
            },
        },
        TxFailureCause::TxFailureContractRejected { contract } => {
            out::FailureCause::ContractRejected {
                contract_index: contract.contract_index,
                code: match contract.code {
                    Reject::ContractRejectTooMany => out::ContractRejectCode::TooMany,
                    Reject::ContractRejectBlobTooLarge => out::ContractRejectCode::BlobTooLarge,
                    Reject::ContractRejectNotDeployable => out::ContractRejectCode::NotDeployable,
                    Reject::ContractRejectDuplicate => out::ContractRejectCode::Duplicate,
                    Reject::ContractRejectMalformedArtifact => {
                        out::ContractRejectCode::MalformedArtifact
                    }
                    Reject::ContractRejectUnsupportedFormat => {
                        out::ContractRejectCode::UnsupportedFormat
                    }
                    Reject::ContractRejectUnsupportedAbi => out::ContractRejectCode::UnsupportedAbi,
                    Reject::ContractRejectGasScheduleMismatch => {
                        out::ContractRejectCode::GasScheduleMismatch
                    }
                    Reject::ContractRejectMemoryTooLarge => out::ContractRejectCode::MemoryTooLarge,
                    Reject::ContractRejectCodeSizeInvalid => {
                        out::ContractRejectCode::CodeSizeInvalid
                    }
                    Reject::ContractRejectInitialMemoryInvalid => {
                        out::ContractRejectCode::InitialMemoryInvalid
                    }
                    Reject::ContractRejectDigestMismatch => out::ContractRejectCode::DigestMismatch,
                    Reject::ContractRejectInvalidProgram => out::ContractRejectCode::InvalidProgram,
                    _ => {
                        return Err(invalid(
                            Reason::UnsupportedValue,
                            "unsupported contract rejection code",
                        )
                        .into());
                    }
                },
            }
        }
        _ => {
            return Err(invalid(
                Reason::UnsupportedValue,
                "unsupported transaction failure cause",
            )
            .into());
        }
    })
}
