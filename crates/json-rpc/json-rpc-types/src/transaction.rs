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
/// Pending reports mempool presence; Committed reports scheduler success or an indexed record.
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
    Committed {
        /// Committed block height supplied by the scheduler or indexer.
        #[serde(with = "crate::decimal")]
        #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
        height: BlockHeight,
    },

    /// Scheduler rejected the transaction.
    Invalid { reason: String },

    /// No committed record, but a recent proposal attempt failed and the
    /// indexer still holds its diagnostic. This includes storage conflicts
    /// during two-phase commit and contract attachments refused before execution.
    ///
    /// Best-effort and advisory: reports are dropped freely under load,
    /// only the proposing node's indexers hear them, and a later retry
    /// of the same transaction may still commit (at which point
    /// `Committed` wins). Absence of this status proves nothing.
    #[serde(rename_all = "camelCase")]
    RecentFailure {
        /// Executing block height of the failed attempt.
        #[serde(with = "crate::decimal")]
        #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
        attempt_height: BlockHeight,
        /// First storage conflict or refused contract attachment.
        cause: TransactionFailureCause,
    },

    /// Scheduler processing failed; resubmitting the transaction may succeed.
    Retry { error: SchedulerTransactionError },

    /// Scheduler processing failed permanently; modify the transaction before resubmitting.
    Dropped { error: SchedulerTransactionError },

    /// Not found in the scheduler's retained outcomes or indexer.
    Unknown,

    /// Unrecognized status from a newer server. Returned when the
    /// client encounters a `status` tag it doesn't know about. Check
    /// for client updates.
    #[serde(other)]
    Other,
}

/// A scheduler processing failure. The source identifies the protocol enum of `code`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[non_exhaustive]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchedulerTransactionError {
    /// Subsystem that reported the failure.
    pub source: SchedulerTransactionErrorSource,
    /// `AuthErrorCode`, `SchedulerErrorCode`, or `ExecResult` for the indicated source.
    pub code: i32,
}

impl SchedulerTransactionError {
    /// Preserve the subsystem and its protocol error code.
    pub fn new(source: SchedulerTransactionErrorSource, code: i32) -> Self {
        Self { source, code }
    }
}

/// Subsystem whose protocol error enum supplies a scheduler failure code.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub enum SchedulerTransactionErrorSource {
    /// Authentication subsystem (`AuthErrorCode`).
    Auth,
    /// Mempool scheduler (`SchedulerErrorCode`).
    Scheduler,
    /// Executor (`ExecResult`).
    Executor,
    /// A source added by a newer server.
    #[serde(other)]
    Other,
}

/// Why a recent proposal attempt failed. This diagnostic is advisory, not final.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TransactionFailureCause {
    /// Two-phase commit could not apply a storage delta.
    DbConflict { conflict: ConflictDiagnostic },
    /// The executor refused a contract attachment before execution.
    #[serde(rename_all = "camelCase")]
    ContractRejected {
        /// Position in the transaction's contracts-to-deploy list.
        contract_index: u32,
        code: ContractRejectCode,
    },
    /// A cause added by a newer server.
    #[serde(other)]
    Other,
}

impl std::fmt::Display for TransactionFailureCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DbConflict { conflict } => {
                let code = serde_json::to_string(&conflict.code).map_err(|_| std::fmt::Error)?;
                let address =
                    serde_json::to_string(&conflict.address).map_err(|_| std::fmt::Error)?;
                write!(
                    f,
                    "{} conflict at {} (key 0x{})",
                    code.trim_matches('"'),
                    address.trim_matches('"'),
                    hex::encode(&conflict.key.0)
                )
            }
            Self::ContractRejected {
                contract_index,
                code,
            } => {
                let code = serde_json::to_string(code).map_err(|_| std::fmt::Error)?;
                write!(
                    f,
                    "contract attachment {contract_index} rejected: {}",
                    code.trim_matches('"')
                )
            }
            Self::Other => f.write_str("unrecognized failure cause"),
        }
    }
}

impl From<&thruput_trace::TxFailureCause> for TransactionFailureCause {
    fn from(cause: &thruput_trace::TxFailureCause) -> Self {
        use thruput_trace::TxFailureCause as Xdr;
        match cause {
            Xdr::TxFailureDbConflict { conflict } => Self::DbConflict {
                conflict: conflict.into(),
            },
            Xdr::TxFailureContractRejected { contract } => Self::ContractRejected {
                contract_index: contract.contract_index,
                code: contract.code.into(),
            },
            _ => Self::Other,
        }
    }
}

/// Reason the executor refused a contract attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub enum ContractRejectCode {
    TooMany,
    BlobTooLarge,
    NotDeployable,
    Duplicate,
    MalformedArtifact,
    UnsupportedFormat,
    UnsupportedAbi,
    GasScheduleMismatch,
    MemoryTooLarge,
    CodeSizeInvalid,
    InitialMemoryInvalid,
    DigestMismatch,
    InvalidProgram,
    /// Unrecognized rejection code from a newer node.
    #[serde(other)]
    Other,
}

impl From<thruput_trace::ContractRejectCode> for ContractRejectCode {
    fn from(code: thruput_trace::ContractRejectCode) -> Self {
        use thruput_trace::ContractRejectCode as Xdr;
        match code {
            Xdr::ContractRejectTooMany => Self::TooMany,
            Xdr::ContractRejectBlobTooLarge => Self::BlobTooLarge,
            Xdr::ContractRejectNotDeployable => Self::NotDeployable,
            Xdr::ContractRejectDuplicate => Self::Duplicate,
            Xdr::ContractRejectMalformedArtifact => Self::MalformedArtifact,
            Xdr::ContractRejectUnsupportedFormat => Self::UnsupportedFormat,
            Xdr::ContractRejectUnsupportedAbi => Self::UnsupportedAbi,
            Xdr::ContractRejectGasScheduleMismatch => Self::GasScheduleMismatch,
            Xdr::ContractRejectMemoryTooLarge => Self::MemoryTooLarge,
            Xdr::ContractRejectCodeSizeInvalid => Self::CodeSizeInvalid,
            Xdr::ContractRejectInitialMemoryInvalid => Self::InitialMemoryInvalid,
            Xdr::ContractRejectDigestMismatch => Self::DigestMismatch,
            Xdr::ContractRejectInvalidProgram => Self::InvalidProgram,
            _ => Self::Other,
        }
    }
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

impl From<thruput_trace::DbConflictCode> for ConflictCode {
    fn from(code: thruput_trace::DbConflictCode) -> Self {
        use thruput_trace::DbConflictCode as Xdr;
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

impl From<&thruput_trace::DbConflictDiagnostic> for ConflictDiagnostic {
    fn from(diagnostic: &thruput_trace::DbConflictDiagnostic) -> Self {
        let key_bytes = xdrs::txs::encode(&diagnostic.addrkey.key)
            .expect("bug: generated StorageKey should encode");
        Self {
            address: crate::hex::Hex0x(diagnostic.addrkey.address),
            key: crate::hex::HexBytes(key_bytes),
            code: diagnostic.code.into(),
        }
    }
}

impl From<&thruput_trace::TxFailureDiagnostic> for TransactionStatus {
    fn from(diagnostic: &thruput_trace::TxFailureDiagnostic) -> Self {
        Self::RecentFailure {
            attempt_height: diagnostic.attempt_height.clone(),
            cause: (&diagnostic.cause).into(),
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
    /// A cursor continues one historical walk, not blocks added after it began.
    pub cursor: PaginationCursor,
    /// Values above the server's per-page cap (default 200) are
    /// rejected as invalid params; `0` means server default (50).
    pub limit: u32,
}

/// Transactions in one committed block, sorted by unsigned transaction-hash
/// bytes, not execution order. Height is a canonical decimal string.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockTransactionQuery {
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub height: BlockHeight,
    pub order: IterationOrder,
    /// `0x` starts a walk. Preserve returned bytes and the same height.
    /// A continuation cursor supplies its original order. Invalid or stale
    /// cursors return an empty terminal page; oversized cursors are rejected.
    pub cursor: PaginationCursor,
    /// Positive page size, at most 200 and subject to the endpoint cap.
    /// Zero is invalid. Send 50 for a normal page.
    #[schemars(range(min = 1, max = 200))]
    pub limit: u32,
}

impl From<BlockTransactionQuery> for thruput_indexer::indexer::BlockTransactionQuery {
    fn from(value: BlockTransactionQuery) -> Self {
        Self {
            height: value.height,
            order: value.order.into(),
            cursor: value.cursor.into(),
            limit: value.limit,
        }
    }
}

#[cfg(test)]
mod block_query_tests;

/// Lightweight reference to a transaction: its hash plus the block it
/// executed in.  Hydrate the full body via `getTransactionReceipt`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRef {
    pub tx_hash: TxHash,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub height: BlockHeight,
}

/// Transaction references from an address-history or exact-block query.
/// Block queries use hash order; address queries use their history order.
///
/// Opaque cursors continue one historical walk; an empty cursor ends it.
/// For restart rules, see the
/// [pagination contract](https://docs.groundhog.dev/reference/json-rpc-api/#pagination-contract).
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

    /// Highest block height indexed at response time, not an application checkpoint.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub latest_block: BlockHeight,

    /// Oldest block height the indexer still retains data for at
    /// response time.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub oldest_block: BlockHeight,
}

impl From<AddressTransactionQuery> for thruput_indexer::indexer::AddressTransactionQuery {
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

impl From<thruput_indexer::indexer::TransactionRef> for TransactionRef {
    fn from(value: thruput_indexer::indexer::TransactionRef) -> Self {
        Self {
            tx_hash: TxHash(crate::hex::Hex0x(thruput_xdr::types::SHA256Hash::from(
                value.tx_hash.into_inner(),
            ))),
            height: value.height,
        }
    }
}

impl From<thruput_indexer::indexer::TransactionRefPage> for TransactionRefPage {
    fn from(value: thruput_indexer::indexer::TransactionRefPage) -> Self {
        Self {
            refs: value.refs.0.into_iter().map(Into::into).collect(),
            cursor: value.cursor.into(),
            latest_block: value.latest_block,
            oldest_block: value.oldest_block,
        }
    }
}
