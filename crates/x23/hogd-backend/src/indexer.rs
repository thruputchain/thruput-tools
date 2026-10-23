//! Decode `IndexerBlockExplorer` transaction lookups.
//!
//! Transaction lookups carry failure outcome codes as well as transport
//! errors, so an undecoded `Default { outcome }` is easy to mistake for a
//! miss. These helpers turn each one into either a value or an
//! [`RpcServerError`], so a backend failure can never reach a client as a
//! missing receipt.

use thruput_indexer::indexer::{TxInfo, TxInfoOutcome, TxInfoResult};
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::transaction::{TransactionStatus, TxHash};
use thruput_trace::TxFailureDiagnostic;

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

/// Decode a transaction lookup.
///
/// Only `TxInfoNotFound` counts as a miss. Any other bare outcome, and any
/// result variant this build does not recognize, is a protocol disagreement
/// with the indexer and becomes an error — reporting it as a miss would tell
/// the caller a transaction is uncommitted on the strength of an answer we
/// could not read.
pub fn tx_lookup(
    service: &str,
    tx_hash: TxHash,
    result: TxInfoResult,
) -> Result<TxLookup, RpcServerError> {
    match result {
        TxInfoResult::TxInfoFound { info } => Ok(TxLookup::Found(Box::new(info))),
        TxInfoResult::TxInfoRecentFailure { recent_failure } => {
            Ok(TxLookup::RecentFailure(recent_failure))
        }
        TxInfoResult::Default { outcome } if outcome == TxInfoOutcome::TxInfoNotFound.0 => {
            Ok(TxLookup::NotFound)
        }
        TxInfoResult::Default { outcome } => {
            tracing::warn!(?tx_hash, %outcome, %service, "unexpected TxInfoResult outcome");
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned unexpected outcome: {outcome}"
            )))
        }
        _ => {
            tracing::warn!(?tx_hash, %service, "unknown TxInfoResult variant");
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned unknown TxInfoResult variant"
            )))
        }
    }
}

/// The status a lookup answers for a transaction the indexer knows about.
///
/// `None` is a miss: the caller decides whether that means `Unknown` or
/// still-pending, because only it knows whether the mempool answered.
pub fn committed_status(lookup: TxLookup) -> Option<TransactionStatus> {
    match lookup {
        TxLookup::Found(info) => Some(TransactionStatus::Committed {
            height: info.height,
        }),
        TxLookup::RecentFailure(recent_failure) => Some(TransactionStatus::from(&recent_failure)),
        TxLookup::NotFound => None,
    }
}
