//! Decode `IndexerBlockExplorer` results into pages, lookups, and errors.
//!
//! Every fallible indexer result carries its failure as an outcome code
//! rather than a transport error, so an undecoded `Default { outcome }` is
//! easy to mistake for an empty answer. These helpers turn each one into
//! either a value or an [`RpcServerError`], so a backend failure can never
//! reach a client as an empty page or a missing receipt.

use groundhog_indexer::{
    EventPage, EventPageResult, IndexerQueryOutcome, TransactionRefPage, TransactionRefPageResult,
    indexer::{TxInfo, TxInfoOutcome, TxInfoResult},
};
use groundhog_json_rpc_server::RpcServerError;
use groundhog_trace::TxFailureDiagnostic;

/// What the indexer knows about one transaction.
///
/// `RecentFailure` is an advisory diagnostic about a nondeterministic
/// failure, not a committed record: a receipt exists only for `Found`.
#[derive(Debug)]
pub enum TxLookup {
    /// The transaction is committed and the indexer holds its record.
    ///
    /// Boxed because a committed record is far larger than a diagnostic,
    /// and most lookups answer `NotFound`.
    Found(Box<TxInfo>),
    /// The indexer holds a recent nondeterministic-failure diagnostic.
    RecentFailure(TxFailureDiagnostic),
    /// The indexer has no record of the transaction.
    NotFound,
}

/// Map an indexer query outcome code onto a backend error.
pub fn query_error(service: &str, outcome: i32) -> RpcServerError {
    let message = if outcome == IndexerQueryOutcome::IndexerQueryNotReady.0 {
        format!("{service} query backend is not ready")
    } else if outcome == IndexerQueryOutcome::IndexerQueryInternalFailure.0 {
        format!("{service} query backend failed internally")
    } else {
        format!("{service} query returned unexpected outcome: {outcome}")
    };
    RpcServerError::BackendUnavailable(message)
}

/// Unwrap an event page, turning a query failure into a backend error.
pub fn event_page(service: &str, result: EventPageResult) -> Result<EventPage, RpcServerError> {
    match result {
        EventPageResult::IndexerQueryOk { page } => Ok(page),
        EventPageResult::Default { outcome } => Err(query_error(service, outcome)),
        _ => Err(RpcServerError::BackendUnavailable(format!(
            "{service} returned an unknown event-page result"
        ))),
    }
}

/// Unwrap a transaction-ref page, turning a query failure into a backend
/// error.
pub fn transaction_ref_page(
    service: &str,
    result: TransactionRefPageResult,
) -> Result<TransactionRefPage, RpcServerError> {
    match result {
        TransactionRefPageResult::IndexerQueryOk { page } => Ok(page),
        TransactionRefPageResult::Default { outcome } => Err(query_error(service, outcome)),
        _ => Err(RpcServerError::BackendUnavailable(format!(
            "{service} returned an unknown transaction-page result"
        ))),
    }
}

/// Decode a transaction lookup.
///
/// Only `TxInfoNotFound` counts as a miss. Any other bare outcome, and any
/// result variant this build does not recognize, is a protocol disagreement
/// with the indexer and becomes an error — reporting it as a miss would tell
/// the caller a transaction is uncommitted on the strength of an answer we
/// could not read.
pub fn tx_lookup(service: &str, result: TxInfoResult) -> Result<TxLookup, RpcServerError> {
    match result {
        TxInfoResult::TxInfoFound { info } => Ok(TxLookup::Found(Box::new(info))),
        TxInfoResult::TxInfoRecentFailure { recent_failure } => {
            Ok(TxLookup::RecentFailure(recent_failure))
        }
        TxInfoResult::Default { outcome } if outcome == TxInfoOutcome::TxInfoNotFound.0 => {
            Ok(TxLookup::NotFound)
        }
        TxInfoResult::Default { outcome } => {
            tracing::warn!(%outcome, %service, "unexpected TxInfoResult outcome");
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned unexpected outcome: {outcome}"
            )))
        }
        _ => {
            tracing::warn!(%service, "unknown TxInfoResult variant");
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned unknown TxInfoResult variant"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_failures_are_named_and_never_read_as_an_empty_page() {
        let error = event_page(
            "indexer",
            EventPageResult::Default {
                outcome: IndexerQueryOutcome::IndexerQueryNotReady.0,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("not ready"), "{error}");

        let error = transaction_ref_page(
            "hogd-dev indexer",
            TransactionRefPageResult::Default {
                outcome: IndexerQueryOutcome::IndexerQueryInternalFailure.0,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("failed internally"), "{error}");
        assert!(error.to_string().contains("hogd-dev indexer"), "{error}");
    }

    #[test]
    fn only_the_not_found_outcome_reads_as_a_miss() {
        assert!(matches!(
            tx_lookup(
                "indexer",
                TxInfoResult::Default {
                    outcome: TxInfoOutcome::TxInfoNotFound.0,
                },
            ),
            Ok(TxLookup::NotFound)
        ));

        let error = tx_lookup("indexer", TxInfoResult::Default { outcome: 4242 }).unwrap_err();
        assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
        assert!(error.to_string().contains("4242"), "{error}");
    }
}
