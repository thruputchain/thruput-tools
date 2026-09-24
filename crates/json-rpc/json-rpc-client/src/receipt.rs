//! One receipt-polling loop, shared by every consumer that waits on a
//! transaction.
//!
//! [`poll_receipt`] drives the loop over a [`ReceiptSource`] rather than a
//! concrete client, so a caller that meters its own requests, wraps the typed
//! client behind its own error type, or substitutes a test double reuses the
//! loop instead of copying it.

use std::future::Future;
use std::time::{Duration, Instant};

use groundhog_json_rpc_api::GroundhogApiClient;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};

/// Default number of receipt lookups [`poll_receipt`] makes before giving up.
//
// Keep the default wait window at roughly 30 seconds, but poll frequently
// enough to observe indexer failure diagnostics that intentionally expire
// after only a few blocks. Fast local and single-validator networks can
// advance several blocks during a one-second polling interval.
pub const DEFAULT_RECEIPT_POLL_ATTEMPTS: usize = 300;

/// Default pause between receipt lookups.
pub const DEFAULT_RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The chain reads a receipt poll makes.
///
/// Implement this to poll through something other than a bare JSON-RPC client:
/// a connection pool that takes a permit per request, a handle that reports its
/// own error type, or a test double. Every [`GroundhogApiClient`] already
/// satisfies it, so an `HttpClient` can be polled directly.
pub trait ReceiptSource {
    /// What this source reports when an RPC round trip fails.
    type Error;

    /// The committed receipt for `tx_hash`, or `None` while it is not indexed.
    fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, Self::Error>> + Send;

    /// Status for `tx_hash`, or `None` from a source that does not serve it.
    ///
    /// Polling a source without status rests on receipts alone: it neither
    /// stops early on a rejection nor reports a last status when it times out.
    /// A failed status lookup is treated the same way and never fails the poll,
    /// because the receipt is the authoritative answer.
    fn transaction_status(
        &self,
        _tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionStatus>, Self::Error>> + Send {
        async { Ok(None) }
    }
}

impl<C> ReceiptSource for C
where
    C: GroundhogApiClient + Sync,
{
    type Error = jsonrpsee::core::client::Error;

    #[allow(clippy::manual_async_fn)]
    fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, Self::Error>> + Send {
        async move { GroundhogApiClient::get_transaction_receipt(self, tx_hash).await }
    }

    #[allow(clippy::manual_async_fn)]
    fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionStatus>, Self::Error>> + Send {
        async move {
            GroundhogApiClient::get_transaction_status(self, tx_hash)
                .await
                .map(Some)
        }
    }
}

/// How long [`poll_receipt`] keeps looking and how often it retries.
///
/// A config carries both an attempt ceiling and an optional wall-clock budget;
/// polling stops at whichever it reaches first.
#[derive(Debug, Clone, Copy)]
pub struct ReceiptPollConfig {
    max_attempts: usize,
    interval: Duration,
    budget: Option<Duration>,
}

impl ReceiptPollConfig {
    /// Look up the receipt at most `max_attempts` times, pausing `interval`
    /// between lookups. Slow round trips extend the total wait.
    pub const fn attempts(max_attempts: usize, interval: Duration) -> Self {
        Self {
            max_attempts,
            interval,
            budget: None,
        }
    }

    /// Look for the receipt for at most `budget`, pausing `interval` between
    /// lookups. Unlike [`ReceiptPollConfig::attempts`], the wait covers the
    /// round trips as well, so a slow backend shortens the retry count rather
    /// than overrunning the budget.
    pub fn within(budget: Duration, interval: Duration) -> Self {
        Self {
            // A deadline alone cannot bound the loop if the clock refuses to
            // advance that far, so derive an equivalent ceiling as well.
            max_attempts: attempts_spanning(budget, interval),
            interval,
            budget: Some(budget),
        }
    }

    /// Most lookups this config permits.
    pub const fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// Pause between lookups.
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// Wall-clock budget, when one bounds this config.
    pub const fn budget(&self) -> Option<Duration> {
        self.budget
    }
}

impl Default for ReceiptPollConfig {
    fn default() -> Self {
        Self::attempts(DEFAULT_RECEIPT_POLL_ATTEMPTS, DEFAULT_RECEIPT_POLL_INTERVAL)
    }
}

/// Lookups that fit in `budget` when each is followed by `interval`.
fn attempts_spanning(budget: Duration, interval: Duration) -> usize {
    let interval = interval.as_nanos();
    if interval == 0 {
        return usize::MAX;
    }
    let spans = budget.as_nanos() / interval;
    usize::try_from(spans)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
}

/// Why [`poll_receipt`] stopped without a receipt.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReceiptPollError<E> {
    /// The receipt lookup itself failed.
    #[error("receipt lookup failed for {tx_hash:?}: {source}")]
    Lookup {
        tx_hash: TxHash,
        #[source]
        source: E,
    },
    /// The transaction reported a status that rules out a receipt.
    #[error("transaction {tx_hash:?} reported failure status {status:?}")]
    Status {
        tx_hash: TxHash,
        status: TransactionStatus,
    },
    /// No receipt appeared before the config ran out.
    #[error("timed out waiting for receipt for {tx_hash:?} after {attempts} attempts")]
    Timeout {
        tx_hash: TxHash,
        attempts: usize,
        /// Last successfully observed status; absent if status was unavailable.
        last_status: Option<TransactionStatus>,
    },
}

/// Wait for a transaction's receipt, retrying on the schedule `config` sets.
///
/// Receipts are authoritative. While one is missing, an `Invalid` status ends
/// polling early. A best-effort `RecentFailure` diagnostic is retained while
/// polling continues, because the same transaction may still commit; if no
/// receipt appears, that diagnostic is returned instead of a bare timeout.
pub async fn poll_receipt<S>(
    source: &S,
    tx_hash: TxHash,
    config: ReceiptPollConfig,
) -> Result<TransactionReceipt, ReceiptPollError<S::Error>>
where
    S: ReceiptSource + Sync + ?Sized,
{
    let deadline = config
        .budget
        .and_then(|budget| Instant::now().checked_add(budget));
    let mut recent_failure = None;
    let mut last_status = None;
    let mut attempts = 0;

    while attempts < config.max_attempts {
        attempts += 1;
        match source.transaction_receipt(tx_hash).await {
            Ok(Some(receipt)) => return Ok(receipt),
            Ok(None) => match source.transaction_status(tx_hash).await {
                Ok(Some(status @ TransactionStatus::Invalid { .. })) => {
                    return Err(ReceiptPollError::Status { tx_hash, status });
                }
                Ok(Some(status @ TransactionStatus::RecentFailure { .. })) => {
                    recent_failure = Some(status);
                }
                // Status failures do not erase the last successful observation.
                // Unknown remains advisory; a later receipt is authoritative.
                observed => {
                    if let Ok(Some(status)) = observed {
                        last_status = Some(status);
                    }
                }
            },
            Err(source) => return Err(ReceiptPollError::Lookup { tx_hash, source }),
        }

        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if remaining.is_some_and(|remaining| remaining.is_zero()) {
            break;
        }
        if attempts < config.max_attempts {
            let pause =
                remaining.map_or(config.interval, |remaining| config.interval.min(remaining));
            tokio::time::sleep(pause).await;
        }
    }

    match recent_failure {
        Some(status) => Err(ReceiptPollError::Status { tx_hash, status }),
        None => Err(ReceiptPollError::Timeout {
            tx_hash,
            attempts,
            last_status,
        }),
    }
}
