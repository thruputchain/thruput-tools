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
    /// The scheduler's pending or retained outcome for `tx_hash`, or `None` for a miss.
    ///
    /// An answer that does not account for the transaction (no per-hash
    /// slot, a transport failure, a timeout) is an error, never a miss:
    /// see [`crate::scheduler::transaction_outcome`].
    fn scheduler_status(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionStatus>, RpcServerError>> + Send;

    /// The status the indexer records for `tx_hash`, or `None` for a miss.
    ///
    /// `Some` is `Committed` or `RecentFailure`, as
    /// [`crate::indexer::committed_status`] decodes them.
    fn indexed_status(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionStatus>, RpcServerError>> + Send;
}

/// Use any indexer record without consulting the scheduler. On an indexer miss
/// or failure, consult the scheduler; a known scheduler outcome can still answer
/// during indexer unavailability. `Unknown` requires successful misses from both.
#[tracing::instrument(skip_all, fields(?tx_hash))]
pub async fn transaction_status<P: StatusProbe + ?Sized>(
    probe: &P,
    tx_hash: TxHash,
) -> Result<TransactionStatus, RpcServerError> {
    let indexed = probe.indexed_status(tx_hash).await;
    if let Ok(Some(status)) = indexed {
        return Ok(status);
    }
    match probe.scheduler_status(tx_hash).await {
        Ok(Some(status)) => Ok(status),
        Ok(None) => {
            indexed?;
            Ok(TransactionStatus::Unknown)
        }
        Err(error) => {
            indexed?;
            Err(error)
        }
    }
}
