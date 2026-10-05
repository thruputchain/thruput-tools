use schemars::JsonSchema;
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
/// - No `gasPrice`, `transactionIndex`, `logsBloom`.
/// - `gasUsed` is committed by consensus as part of the transaction's event
///   log (`TxEventLog.gas_consumed`).
///
/// `storage_delta` (net HOG locked/unlocked) is deferred — hogd's
/// `TxInfo` doesn't include it. Will be added when indexer support lands.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    /// Canonical transaction hash.
    pub tx_hash: TxHash,

    /// Block height at which this transaction was committed.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub height: BlockHeight,

    /// Execution result.
    pub status: ExecStatus,

    /// Events emitted during execution.
    pub events: Vec<EmittedEvent>,

    /// Bytes returned by the contract's root call on a successful
    /// transaction.  Present when `status` is `ok` and the chain
    /// version that executed the transaction recorded a return
    /// value; absent otherwise.  Returned raw — decode against the
    /// contract's spec to recover typed values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_data: Option<HexBytes>,

    /// Bytes the contract passed to its `abort` / `revert` syscall
    /// when the transaction reverted deterministically.  Present
    /// when `status` is `reverted` and the chain version that
    /// executed the transaction recorded the revert payload; absent
    /// otherwise.  Returned raw — decode against the contract's
    /// spec (typically the error variant) to recover a human-
    /// readable reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_data: Option<HexBytes>,

    /// Gas the transaction consumed: the root frame, every frame it invoked,
    /// and the pre-execution validation of attached contracts. On an
    /// out-of-gas fault it equals the transaction's gas limit.
    ///
    /// Committed by consensus as part of the event log, so every node
    /// reports the same figure.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub gas_used: u64,
}

/// `kind` reported for a deterministic execution fault when the node
/// does not surface a finer-grained category.
///
/// hogd's `ExecResult` currently bundles every deterministic failure —
/// faults such as out-of-bounds memory access, plus out of gas — under
/// a single variant, so backends report this kind for all of them.
/// Finer-grained kinds (for example out of gas) may be reported
/// separately in the future.
pub const DETERMINISTIC_ERROR_KIND_FAULT: &str = "fault";

/// Execution result. Maps to hogd's `ExecResult` variants.
/// Richer than EVM's binary status (0 = fail, 1 = success).
///
/// New variants may be added as hogd's `ExecResult` evolves.
/// Clients should handle unknown variants gracefully.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

    /// Deterministic execution error: the transaction faulted during
    /// execution (out-of-bounds access, out of gas, stack overflow, etc.).
    DeterministicError {
        /// Error kind from the execution engine. `"fault"` when the
        /// node does not report a finer-grained category (currently
        /// always — see `DETERMINISTIC_ERROR_KIND_FAULT`).
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
