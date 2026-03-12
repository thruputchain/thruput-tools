use serde::{Deserialize, Serialize};

use crate::events::EmittedEvent;
use crate::transaction::TxHash;
use crate::BlockHeight;

/// Full transaction receipt. Returned by getTransactionReceipt.
///
/// Diverges from EVM receipts intentionally:
/// - No gasUsed/gasPrice (gas bounds computation but costs zero)
/// - No transactionIndex (not stored in indexer)
/// - No logsBloom (events are directly indexed)
/// - ExecStatus is richer than EVM's binary 0/1
/// - storage_delta replaces gas economics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    pub tx_hash: TxHash,
    pub height: BlockHeight,
    pub status: ExecStatus,
    pub events: Vec<EmittedEvent>,

    /// Net change in locked HOG from storage writes/deletes.
    /// Positive = more HOG locked (storage grew).
    /// Negative = HOG unlocked (storage shrank).
    /// None if not available from the indexer.
    /// TODO: confirm with Geoff whether this is in TxEventLog today
    pub storage_delta: Option<i128>,
}

/// Execution result. Maps to hogd's ExecResult variants.
/// Richer than EVM's binary status (0 = fail, 1 = success).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    DeterministicError { kind: String },

    /// Gas sponsorship failed.
    FailedGasSponsorship,

    // Future: additional variants as hogd ExecResult evolves.
    // Keep this enum non_exhaustive so clients handle new variants.
    #[serde(other)]
    Other,
}
