//! The `getTransactionStatus` probe: which backends are asked, in what
//! order, and what each answer decides.
//!
//! A transaction's status lives in two places. The scheduler mempool holds
//! it while it is pending; the indexer holds it once it is committed, or
//! holds a recent-failure diagnostic for it. The gateway and the node
//! supervisor both compose those two reads into one answer, and the
//! composition is where their answers used to drift: one asked the mempool
//! first and the other the indexer, so a transaction in both places, or one
//! that committed between the two reads, read differently depending on
//! which binary served it. [`transaction_status`] is that composition, once.
//! A binary supplies only its two raw lookups through [`StatusProbe`] and
//! keeps what is its own — admission permits, deadlines, metrics, and
//! connection recovery — inside those lookups.

use async_trait::async_trait;
use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};

/// The two backend reads [`transaction_status`] composes.
///
/// Each read is one backend request, bounded and metered however the
/// implementer bounds and meters its requests. Failures are returned, not
/// swallowed: the sequencer decides which ones a later answer overrides.
#[async_trait]
pub trait StatusProbe: Sync {
    /// Whether the scheduler mempool holds `tx_hash`.
    ///
    /// An answer that does not account for the transaction — no per-hash
    /// slot, a transport failure, a timeout — is an error, never `false`:
    /// see [`crate::scheduler::mempool_answer`].
    async fn mempool_holds(&self, tx_hash: TxHash) -> Result<bool, RpcServerError>;

    /// The status the indexer records for `tx_hash`, or `None` for a miss.
    ///
    /// `Some` is `Committed` or `RecentFailure`, as
    /// [`crate::indexer::committed_status`] decodes them.
    async fn indexed_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionStatus>, RpcServerError>;
}

/// Decide a transaction's status from the indexer and the mempool.
///
/// Sequence:
/// 1. Indexer. A committed record answers at once, so committed reads never
///    occupy the mempool lookup lane.
/// 2. Mempool. A hit answers `Pending`, overriding a recent-failure
///    diagnostic (which belongs to an earlier attempt) and an indexer
///    failure alike.
/// 3. The indexer's first answer. A recent-failure diagnostic is returned.
///    An indexer failure is returned too: without the indexer nothing can
///    rule out a commit, so a mempool miss cannot become `Unknown`.
/// 4. Indexer again. The first read can miss a transaction that committed
///    while the mempool was being consulted; a record found now is returned.
/// 5. `Unknown` only when both backends answered and both missed. A mempool
///    read that failed is returned as its error instead, because the
///    transaction may still be pending in the mempool that could not be
///    consulted, and reporting it gone would invite an unsafe re-sign.
///
/// Committed reads cost one backend request, pending reads two, and unknown
/// reads three.
pub async fn transaction_status<P: StatusProbe + ?Sized>(
    probe: &P,
    tx_hash: TxHash,
) -> Result<TransactionStatus, RpcServerError> {
    let indexed = probe.indexed_status(tx_hash).await;
    if let Ok(Some(TransactionStatus::Committed { height })) = indexed {
        return Ok(TransactionStatus::Committed { height });
    }
    let mempool_failure = match probe.mempool_holds(tx_hash).await {
        Ok(true) => return Ok(TransactionStatus::Pending),
        Ok(false) => None,
        Err(error) => {
            tracing::warn!(%error, "mempool lookup did not complete; checking indexer");
            Some(error)
        }
    };
    if let Some(status) = indexed? {
        return Ok(status);
    }
    if let Some(status) = probe.indexed_status(tx_hash).await? {
        return Ok(status);
    }
    match mempool_failure {
        Some(RpcServerError::BackendUnavailable(message)) => {
            Err(RpcServerError::BackendUnavailable(format!(
                "{message}; the transaction is not committed but may still be pending"
            )))
        }
        Some(error) => Err(error),
        None => Ok(TransactionStatus::Unknown),
    }
}
