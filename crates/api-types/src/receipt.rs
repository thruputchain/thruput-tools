use serde::{Deserialize, Serialize};

use crate::BlockHeight;
use crate::events::EmittedEvent;
use crate::hex::HexBytes;
use crate::transaction::TxHash;

/// Full transaction receipt. Returned by `getTransactionReceipt`.
///
/// Design notes:
/// - `status` maps from `TxEventLog.exec_result` in the indexer.
///   Committed blocks include both successful and deterministically-failed
///   txs (reverts, OOG). If a receipt exists, the tx was committed.
/// - No `blockHash`: SCP instant finality, height is sufficient.
/// - No `gasUsed`/`gasPrice`, `transactionIndex`, `logsBloom`.
///
/// `storage_delta` (net HOG locked/unlocked) is deferred — hogd's
/// `TxInfo` doesn't include it. Will be added when indexer support lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    /// Canonical transaction hash.
    pub tx_hash: TxHash,

    /// Block height at which this transaction was committed.
    pub height: BlockHeight,

    /// Execution result.
    pub status: ExecStatus,

    /// Events emitted during execution.
    pub events: Vec<EmittedEvent>,

    /// Raw return data from contract execution.
    ///
    /// Populated by local-node backends from `InvocationFrameOutput::Return`.
    /// May be `None` for production receipts if the indexer does not
    /// store return data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_data: Option<HexBytes>,
}

/// Execution result. Maps to hogd's `ExecResult` variants.
/// Richer than EVM's binary status (0 = fail, 1 = success).
///
/// New variants may be added as hogd's `ExecResult` evolves.
/// Clients should handle unknown variants gracefully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum ExecStatus {
    /// Transaction executed successfully.
    Ok,

    /// Root call frame reverted (contract called abort/revert).
    Reverted {
        /// Abort buffer from the contract, if any.
        message: Option<String>,
    },

    /// Deterministic execution error (out of gas, stack overflow, etc.).
    DeterministicError {
        /// Error kind from the execution engine.
        kind: String,
    },

    /// Commit failed due to a race or other proposal-time storage conflict.
    NonDeterministicError,

    /// Gas sponsorship failed.
    FailedGasSponsorship,

    /// Unrecognized execution result from the indexer.
    /// Returned when the gateway encounters an `ExecResult` variant
    /// it doesn't know about. Check for gateway updates.
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::ExecStatus;

    #[test]
    fn serializes_nondeterministic_error_variant() {
        let json = serde_json::to_string(&ExecStatus::NonDeterministicError)
            .expect("bug: nondeterministic status should serialize");
        assert_eq!(json, r#"{"result":"nonDeterministicError"}"#);
    }
}
