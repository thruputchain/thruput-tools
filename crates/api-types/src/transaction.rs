use serde::{Deserialize, Serialize};

use crate::{BlockHeight, SHA256Hash};

/// Canonical transaction hash: sha256(xdr_serialize(signed_tx.tx)).
/// Covers the inner Transaction body (not signatures), matching
/// the scheduler's lookup key and the indexer's primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TxHash(pub SHA256Hash);

/// Lightweight status — the result of checking mempool then indexer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TransactionStatus {
    /// In the mempool, not yet in a committed block.
    Pending,

    /// Included in a committed block. SCP finality = done.
    Committed {
        height: BlockHeight,
        block_hash: SHA256Hash,
    },

    /// Scheduler rejected the transaction.
    Invalid { reason: String },

    /// Not found in mempool or indexer.
    Unknown,
}
