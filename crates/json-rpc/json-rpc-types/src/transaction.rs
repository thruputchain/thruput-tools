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

/// Lightweight transaction status from the mempool and indexer.
///
/// Pending reports mempool presence; Committed reports an indexed record.
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

    /// No committed record, but the transaction failed
    /// nondeterministically during two-phase commit within the last few
    /// blocks and the indexer still holds the diagnostic.
    ///
    /// Best-effort and advisory: reports are dropped freely under load,
    /// only the proposing node's indexers hear them, and a later retry
    /// of the same transaction may still commit (at which point
    /// `Committed` wins). Absence of this status proves nothing.
    #[serde(rename_all = "camelCase")]
    RecentFailure {
        /// Executing block height of the failed attempt.
        attempt_height: BlockHeight,
        /// First conflicting storage op reported for the failed attempt.
        conflict: ConflictDiagnostic,
    },

    /// Not found in mempool or indexer.
    Unknown,

    /// Unrecognized status from a newer server. Returned when the
    /// client encounters a `status` tag it doesn't know about. Check
    /// for client updates.
    #[serde(other)]
    Other,
}

/// One conflicting storage operation reported by two-phase commit.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDiagnostic {
    /// Contract whose storage the conflicting operation targeted.
    pub address: ContractAddress,

    /// XDR-encoded `StorageKey` bytes.
    pub key: crate::hex::HexBytes,

    /// Coarse conflict reason.
    pub code: ConflictCode,
}

/// Reason a storage delta could not be applied during two-phase commit.
/// Projects hogd's `DbConflictCode`.
///
/// New codes may be added as hogd evolves; unrecognized codes surface
/// as `Other` on both serialize and deserialize so mixed-version
/// deployments degrade instead of failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub enum ConflictCode {
    /// The key is exclusively locked by another transaction, an
    /// exclusive lock could not be acquired, or an exclusive-lock flag
    /// was used on a key type that does not support it.
    ExclusiveLock,

    /// Two transactions created different item types at the same key.
    TypeMismatch,

    /// Two transactions wrote irreconcilable values of the same type at
    /// the same key.
    ValueMismatch,

    /// A nonnegative int (or balance) would go below zero given the
    /// deltas in this block, or a newly created balance item cannot
    /// cover its own storage cost.
    BoundsExceeded,

    /// Integer overflow combining deltas on a nonnegative int or balance.
    Overflow,

    /// Replay-protection conflict: uniquifier expired, already consumed,
    /// or consumed twice by this transaction.
    ReplayProtection,

    /// A retrocausal queue already has a consumer in this block.
    QueueConsumer,

    /// A per-item capacity limit was hit.
    CapacityExceeded,

    /// The native HOG balance at the diagnostic's address cannot cover the
    /// debits in this block, or a newly created balance item cannot cover
    /// its own storage cost. Storage written under a contract's address is
    /// charged to that contract's balance, so a deployment reports this at
    /// the new contract's address when it holds no HOG.
    InsufficientFunds,

    /// Unrecognized conflict code. Check for gateway updates.
    #[serde(other)]
    Other,
}

impl From<groundhog_trace::DbConflictCode> for ConflictCode {
    fn from(code: groundhog_trace::DbConflictCode) -> Self {
        use groundhog_trace::DbConflictCode as Xdr;
        match code {
            Xdr::ExclusiveLockDenied => Self::ExclusiveLock,
            Xdr::TypeMismatch => Self::TypeMismatch,
            Xdr::ValueMismatch => Self::ValueMismatch,
            Xdr::BoundsExceeded => Self::BoundsExceeded,
            Xdr::Overflow => Self::Overflow,
            Xdr::ReplayProtection => Self::ReplayProtection,
            Xdr::QueueConsumer => Self::QueueConsumer,
            Xdr::CapacityExceeded => Self::CapacityExceeded,
            Xdr::InsufficientFunds => Self::InsufficientFunds,
            _ => Self::Other,
        }
    }
}

impl From<&groundhog_trace::DbConflictDiagnostic> for ConflictDiagnostic {
    fn from(diagnostic: &groundhog_trace::DbConflictDiagnostic) -> Self {
        let key_bytes = xdrs::txs::encode(&diagnostic.addrkey.key)
            .expect("bug: generated StorageKey should encode");
        Self {
            address: crate::hex::Hex0x(diagnostic.addrkey.address),
            key: crate::hex::HexBytes(key_bytes),
            code: diagnostic.code.into(),
        }
    }
}

impl From<&groundhog_trace::TxFailureDiagnostic> for TransactionStatus {
    fn from(diagnostic: &groundhog_trace::TxFailureDiagnostic) -> Self {
        Self::RecentFailure {
            attempt_height: diagnostic.attempt_height.clone(),
            conflict: (&diagnostic.conflict).into(),
        }
    }
}

/// `getTransactionsByAddress` request: the transaction history of
/// `address`, walked in `order` from `anchor`.
///
/// A transaction is part of an address's history when its root
/// invocation targets that address; transactions that merely touch the
/// address mid-execution do not match.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddressTransactionQuery {
    pub address: ContractAddress,
    pub order: IterationOrder,
    pub anchor: BlockAnchor,
    /// `0x` on the initial call. Non-empty to continue where a previous
    /// response left off. `order` and `anchor` remain required request fields,
    /// but the cursor supplies continuation state. Keep the same address.
    /// The historical walk does not follow blocks added after it began.
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
/// Continue through empty pages with non-empty cursors. To scan later history,
/// reread the last stored row's height and deduplicate by transaction hash.
/// Persist rows and progress together; an opaque cursor is not a durable
/// coverage checkpoint or a promise that pruned history remains available.
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

    /// Highest block height the indexer has processed at response time,
    /// not the height through which the application has stored every result.
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
