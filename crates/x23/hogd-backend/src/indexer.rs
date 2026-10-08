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
use groundhog_json_rpc_types::{
    BlockHeight,
    events::{EventPage as PublicEventPage, IdentifiedEvent},
    hex::Hex0x,
    transaction::{TransactionStatus, TxHash},
};
use groundhog_trace::TxFailureDiagnostic;

use crate::event::parse_emitted_event;

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

/// Where a malformed event sat in a page, for the caller's diagnostics.
#[derive(Debug, Clone)]
pub struct DroppedEvent {
    /// Transaction that emitted the event.
    pub tx_hash: TxHash,
    /// Block the event was emitted in.
    pub block_height: BlockHeight,
    /// Position within the transaction's event log.
    pub sequence_number: u32,
}

/// A public event page with the malformed events it dropped.
#[derive(Debug)]
pub struct DecodedEventPage {
    /// The page to return to the client.
    pub page: PublicEventPage,
    /// Events dropped as malformed, in wire order.
    pub dropped: Vec<DroppedEvent>,
}

/// Convert an indexer page into its public shape.
///
/// Malformed events are dropped rather than failing the page. The
/// pagination metadata still reflects the indexer's view, so a cursor walk
/// continues past the gap.
pub fn public_event_page(page: EventPage) -> DecodedEventPage {
    let mut events = Vec::with_capacity(page.events.0.len());
    let mut dropped = Vec::new();
    for identified in page.events.0 {
        let tx_hash = TxHash(Hex0x(identified.tx_hash));
        match parse_emitted_event(identified.emitted_event) {
            Some(event) => events.push(IdentifiedEvent {
                tx_hash,
                block_height: identified.block_height,
                sequence_number: identified.sequence_number,
                event,
            }),
            None => dropped.push(DroppedEvent {
                tx_hash,
                block_height: identified.block_height,
                sequence_number: identified.sequence_number,
            }),
        }
    }
    DecodedEventPage {
        page: PublicEventPage {
            events,
            cursor: page.cursor.into(),
            latest_block: page.latest_block,
            oldest_block: page.oldest_block,
        },
        dropped,
    }
}
