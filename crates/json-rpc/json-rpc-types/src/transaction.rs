use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::events::{BlockAnchor, IterationOrder, PaginationCursor};
use crate::{BlockHeight, ContractAddress, SHA256Hash};

/// Canonical transaction hash: `sha256(xdr_serialize(signed_tx.tx))`.
/// Covers the inner Transaction body (not signatures), matching
/// the scheduler's lookup key and the indexer's primary key.
///
/// Serializes as `"0x..."` hex string (64 hex chars).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TxHash(pub SHA256Hash);

/// Lightweight status — the result of checking mempool then indexer.
///
/// Lookup order: scheduler (Pending) → indexer (Committed) → Unknown.
/// See `docs/gateway/system-design.md` "Transaction Status Lookup Order" for
/// degraded-mode behavior when a backend is unavailable.
///
/// New variants may be added in future phases.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TransactionStatus {
    /// In the mempool, not yet in a committed block.
    Pending,

    /// Included in a committed block. SCP finality = done.
    /// No `block_hash`: SCP instant finality means no forks/reorgs,
    /// so height alone is sufficient for confirmation.
    Committed { height: BlockHeight },

    /// Scheduler rejected the transaction.
    Invalid { reason: String },

    /// Not found in mempool or indexer.
    Unknown,
}

/// `getTransactionsByAddress` request: the transaction history of
/// `address`, walked in `order` from `anchor`.
///
/// A transaction is part of an address's history when its root
/// invocation targets that address; transactions that merely touch the
/// address mid-execution do not match.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct AddressTransactionQuery {
    pub address: ContractAddress,
    pub order: IterationOrder,
    pub anchor: BlockAnchor,
    /// `0x` on the initial call. Non-empty to continue where a previous
    /// response left off; `order` and `anchor` in the request are
    /// ignored when a cursor is supplied — the cursor carries them.
    pub cursor: PaginationCursor,
    /// Values above the server's per-page cap (default 200) are
    /// rejected as invalid params; `0` means server default (50).
    pub limit: u32,
}

/// Lightweight reference to a transaction: its hash plus the block it
/// executed in.  Hydrate the full body via `getTransactionReceipt`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRef {
    pub tx_hash: TxHash,
    pub height: BlockHeight,
}

/// `getTransactionsByAddress` response: transaction refs in the
/// requested order.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRefPage {
    /// Matched transaction refs in the requested order.  Empty refs +
    /// empty cursor = stream exhausted.
    pub refs: Vec<TransactionRef>,

    /// Empty when the stream has been exhausted in the requested
    /// direction.  Non-empty: pass back in the next call's `cursor`
    /// argument.
    pub cursor: PaginationCursor,

    /// Highest block height the indexer has processed at response time.
    pub latest_block: BlockHeight,

    /// Oldest block height the indexer still retains data for at
    /// response time.
    pub oldest_block: BlockHeight,
}

impl From<AddressTransactionQuery> for groundhog_indexer::indexer::AddressTransactionQuery {
    fn from(value: AddressTransactionQuery) -> Self {
        Self {
            address: value.address.0,
            order: value.order.into(),
            anchor: value.anchor.into(),
            cursor: value.cursor.into(),
            limit: value.limit,
        }
    }
}

impl From<groundhog_indexer::indexer::TransactionRef> for TransactionRef {
    fn from(value: groundhog_indexer::indexer::TransactionRef) -> Self {
        Self {
            tx_hash: TxHash(crate::hex::Hex0x(groundhog_xdr::types::SHA256Hash::from(
                value.tx_hash.into_inner(),
            ))),
            height: value.height,
        }
    }
}

impl From<groundhog_indexer::indexer::TransactionRefPage> for TransactionRefPage {
    fn from(value: groundhog_indexer::indexer::TransactionRefPage) -> Self {
        Self {
            refs: value.refs.0.into_iter().map(Into::into).collect(),
            cursor: value.cursor.into(),
            latest_block: value.latest_block,
            oldest_block: value.oldest_block,
        }
    }
}
