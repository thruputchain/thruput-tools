//! The `getTransactionStatus` probe: which backends are asked, in what
//! order, and what each answer decides.
//!
//! A binary supplies its two raw lookups through [`StatusProbe`], keeping
//! admission permits, deadlines, metrics, and connection recovery inside
//! them; [`transaction_status`] owns only the order and the precedence.

use std::future::Future;

use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};

/// The two backend reads [`transaction_status`] composes.
///
/// Each read is one backend request, bounded and metered however the
/// implementer bounds and meters its requests. Failures are returned, not
/// swallowed: the sequencer decides which ones a later answer overrides.
pub trait StatusProbe: Sync {
    /// Whether the scheduler mempool holds `tx_hash`.
    ///
    /// An answer that does not account for the transaction (no per-hash
    /// slot, a transport failure, a timeout) is an error, never `false`:
    /// see [`crate::scheduler::mempool_answer`].
    fn mempool_holds(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<bool, RpcServerError>> + Send;

    /// The status the indexer records for `tx_hash`, or `None` for a miss.
    ///
    /// `Some` is `Committed` or `RecentFailure`, as
    /// [`crate::indexer::committed_status`] decodes them.
    fn indexed_status(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionStatus>, RpcServerError>> + Send;
}

/// Decide a transaction's status: indexer, then mempool, then the indexer
/// again to cover a commit during the mempool read. A committed record or a
/// mempool hit answers at once; `Unknown` only when both backends answered
/// and missed, and a failed mempool read is returned as its error instead.
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
