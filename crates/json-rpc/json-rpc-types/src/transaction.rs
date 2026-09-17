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

    /// Unrecognized conflict code. Check for gateway updates.
    #[serde(other)]
    Other,
}

impl From<groundhog_trace::DbConflictCode> for ConflictCode {
    fn from(code: groundhog_trace::DbConflictCode) -> Self {
        use groundhog_trace::DbConflictCode as Xdr;
        match code {
            Xdr::kDbConflictExclusiveLock => Self::ExclusiveLock,
            Xdr::kDbConflictTypeMismatch => Self::TypeMismatch,
            Xdr::kDbConflictValueMismatch => Self::ValueMismatch,
            Xdr::kDbConflictBoundsExceeded => Self::BoundsExceeded,
            Xdr::kDbConflictOverflow => Self::Overflow,
            Xdr::kDbConflictReplayProtection => Self::ReplayProtection,
            Xdr::kDbConflictQueueConsumer => Self::QueueConsumer,
            Xdr::kDbConflictCapacityExceeded => Self::CapacityExceeded,
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

#[cfg(test)]
mod tests {
    use super::{ConflictCode, TransactionStatus};
    use crate::BlockHeight;
    use groundhog_trace::{DbConflictCode, DbConflictDiagnostic, TxFailureDiagnostic};
    use groundhog_xdr::types::{AddressAndKey, ContractAddress, StorageKey};
    use serde_json::json;

    fn diagnostic(code: DbConflictCode) -> TxFailureDiagnostic {
        TxFailureDiagnostic {
            attempt_height: BlockHeight::from(1234),
            conflict: DbConflictDiagnostic {
                addrkey: AddressAndKey {
                    address: ContractAddress::from([0x11; 32]),
                    key: StorageKey::FreeNativeTokenBalance,
                },
                code,
            },
        }
    }

    #[test]
    fn recent_failure_serializes_camel_case() {
        let status = TransactionStatus::from(&diagnostic(DbConflictCode::kDbConflictExclusiveLock));

        let value = serde_json::to_value(&status).expect("recent failure should serialize");

        assert_eq!(value["status"], "recentFailure");
        assert_eq!(value["attemptHeight"], 1234);
        assert_eq!(
            value["conflict"]["address"],
            format!("0x{}", "11".repeat(32))
        );
        let expected_key = format!(
            "0x{}",
            hex::encode(
                xdrs::txs::encode(&StorageKey::FreeNativeTokenBalance)
                    .expect("storage key should encode")
            )
        );
        assert_eq!(value["conflict"]["key"], expected_key);
        assert_eq!(value["conflict"]["code"], "exclusiveLock");
        assert!(value.get("attempt_height").is_none());
    }

    #[test]
    fn conflict_code_maps_all_known_codes() {
        let cases = [
            (DbConflictCode::kDbConflictExclusiveLock, "exclusiveLock"),
            (DbConflictCode::kDbConflictTypeMismatch, "typeMismatch"),
            (DbConflictCode::kDbConflictValueMismatch, "valueMismatch"),
            (DbConflictCode::kDbConflictBoundsExceeded, "boundsExceeded"),
            (DbConflictCode::kDbConflictOverflow, "overflow"),
            (
                DbConflictCode::kDbConflictReplayProtection,
                "replayProtection",
            ),
            (DbConflictCode::kDbConflictQueueConsumer, "queueConsumer"),
            (
                DbConflictCode::kDbConflictCapacityExceeded,
                "capacityExceeded",
            ),
        ];
        for (code, expected) in cases {
            let json = serde_json::to_value(ConflictCode::from(code))
                .expect("conflict code should serialize");
            assert_eq!(json, *expected, "wrong projection for {code}");
        }
    }

    #[test]
    fn unknown_conflict_code_maps_to_other() {
        let json = serde_json::to_value(ConflictCode::from(DbConflictCode(99)))
            .expect("unknown conflict code should serialize");
        assert_eq!(json, "other");
    }

    #[test]
    fn conflict_code_deserializes_unknown_string_as_other() {
        let code: ConflictCode =
            serde_json::from_value(json!("someFutureCode")).expect("unknown code should decode");
        assert_eq!(code, ConflictCode::Other);
    }

    #[test]
    fn transaction_status_deserializes_unknown_tag_as_other() {
        let status: TransactionStatus =
            serde_json::from_value(json!({"status": "someFutureStatus"}))
                .expect("unknown status tag should decode");
        assert!(
            matches!(status, TransactionStatus::Other),
            "expected Other, got {status:?}"
        );
    }
}
