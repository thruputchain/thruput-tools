//! Native history query conversion and atomic page projection.
//!
//! Callers validate native request limits before conversion. The shared service
//! checks response bounds, identity, ordering and progress against the request.

use super::{BackendDataInvalidReason as Reason, RpcServerError, invalid};
use thruput_indexer::{self as wire, pagination};
use thruput_json_rpc_types::{
    base58::Base58,
    hex::{HexBytes, Topic},
    requests as input, responses as out,
    transaction::TxHash,
};
use thruput_xdr::event::{TxEmittedEvent, kMaxEventLength};
use xdr_common::{BytesM, VecM};

fn position(
    start: Option<&input::HistoryStart>,
) -> Result<(pagination::IterationOrder, pagination::BlockAnchor), RpcServerError> {
    use input::{AscendingAnchor, DescendingAnchor, HistoryStart};
    use pagination::{BlockAnchor as Anchor, IterationOrder as Order};
    Ok(match start {
        Some(HistoryStart::Ascending { anchor }) => (
            Order::IterationOrderAscending,
            match anchor {
                AscendingAnchor::AtBeginning {} => Anchor::AnchorAtBeginning,
                AscendingAnchor::AtHeight { block_height } => Anchor::AnchorAtHeight {
                    height: block_height.clone(),
                },
                _ => {
                    return Err(RpcServerError::InvalidParam(
                        "unsupported history anchor".into(),
                    ));
                }
            },
        ),
        Some(HistoryStart::Descending { anchor }) => (
            Order::IterationOrderDescending,
            match anchor {
                DescendingAnchor::AtEnd {} => Anchor::AnchorAtEnd,
                DescendingAnchor::AtHeight { block_height } => Anchor::AnchorAtHeight {
                    height: block_height.clone(),
                },
                _ => {
                    return Err(RpcServerError::InvalidParam(
                        "unsupported history anchor".into(),
                    ));
                }
            },
        ),
        // The indexer restores order and anchor from a continuation cursor.
        None => (Order::IterationOrderAscending, Anchor::AnchorAtBeginning),
        _ => {
            return Err(RpcServerError::InvalidParam(
                "unsupported history start".into(),
            ));
        }
    })
}

fn cursor(value: Option<&HexBytes>) -> pagination::PaginationCursor {
    BytesM(value.map_or_else(Vec::new, |value| value.0.clone()))
}

fn topics(value: Option<&[Topic]>) -> VecM<thruput_xdr::event::Topic> {
    VecM(
        value
            .into_iter()
            .flatten()
            .map(|topic| topic.0.into())
            .collect(),
    )
}

/// Convert a validated event request without interpreting its opaque cursor.
pub fn event_query(value: &input::EventQuery) -> Result<wire::EventQuery, RpcServerError> {
    let (order, anchor) = position(value.start.as_ref())?;
    Ok(wire::EventQuery {
        filters: VecM(
            value
                .filters
                .iter()
                .map(|filter| wire::EventFilter {
                    contract_addresses: VecM(
                        filter.addresses.iter().map(|address| address.0).collect(),
                    ),
                    topics: wire::EventTopicFilter {
                        topic0: topics(
                            filter
                                .topics
                                .as_ref()
                                .and_then(|topics| topics.topic0.as_deref()),
                        ),
                        topic1: topics(
                            filter
                                .topics
                                .as_ref()
                                .and_then(|topics| topics.topic1.as_deref()),
                        ),
                        topic2: topics(
                            filter
                                .topics
                                .as_ref()
                                .and_then(|topics| topics.topic2.as_deref()),
                        ),
                    },
                })
                .collect(),
        ),
        order,
        anchor,
        cursor: cursor(value.cursor.as_ref()),
        limit: value.limit.unwrap_or_default(),
    })
}

/// Convert a validated root-address request. The indexer owns cursor binding.
pub fn root_query(
    value: &input::RootAddressQuery,
) -> Result<wire::AddressTransactionQuery, RpcServerError> {
    let (order, anchor) = position(value.start.as_ref())?;
    Ok(wire::AddressTransactionQuery {
        address: value.address.0,
        order,
        anchor,
        cursor: cursor(value.cursor.as_ref()),
        limit: value.limit.unwrap_or_default(),
    })
}

/// Convert a validated exact-block request, preserving its height and cursor.
pub fn block_query(value: &input::BlockPageRequest) -> wire::BlockTransactionQuery {
    wire::BlockTransactionQuery {
        height: value.block_height.clone(),
        order: value.start.as_ref().map_or(
            pagination::IterationOrder::IterationOrderAscending,
            |start| start.order.into(),
        ),
        cursor: cursor(value.cursor.as_ref()),
        limit: value.limit.unwrap_or_default(),
    }
}

fn query_error(outcome: i32) -> RpcServerError {
    use wire::IndexerQueryOutcome as Outcome;
    if outcome == Outcome::IndexerQueryInvalidCursor.0 {
        RpcServerError::InvalidCursor("indexer rejected history cursor".into())
    } else if outcome == Outcome::IndexerQueryHistoryUnavailable.0 {
        RpcServerError::HistoryUnavailable("indexer cannot serve requested history".into())
    } else if outcome == Outcome::IndexerQueryNotReady.0 {
        RpcServerError::BackendUnavailable("indexer history backend is not ready".into())
    } else if outcome == Outcome::IndexerQueryInternalFailure.0 {
        RpcServerError::BackendUnavailable("indexer history backend failed".into())
    } else {
        invalid(Reason::UnsupportedValue, "unknown indexer history outcome")
    }
}

fn next_cursor(cursor: pagination::PaginationCursor) -> Option<HexBytes> {
    (!cursor.0.is_empty()).then_some(HexBytes(cursor.0))
}

fn event(value: TxEmittedEvent) -> Result<out::EmittedEvent, RpcServerError> {
    // XDR enforces this bound on decoded bytes; direct Rust callers can still
    // construct an oversized bounded vector.
    if value.serialized_event.0.len() > kMaxEventLength as usize {
        return Err(invalid(Reason::InvalidEncoding, "malformed history event"));
    }
    let parsed = crate::event::parse_emitted_event(value)
        .ok_or_else(|| invalid(Reason::InvalidEncoding, "malformed history event"))?;
    Ok(out::EmittedEvent {
        address: parsed.contract_address,
        topics: parsed.topics,
        data: parsed.data,
    })
}

/// Project the complete page or fail it; never omit a malformed event.
pub fn event_page(value: wire::EventPageResult) -> Result<out::EventPage, RpcServerError> {
    let page = match value {
        wire::EventPageResult::IndexerQueryOk { page } => page,
        wire::EventPageResult::Default { outcome } => return Err(query_error(outcome)),
        _ => {
            return Err(invalid(
                Reason::UnsupportedValue,
                "unknown event-page result",
            ));
        }
    };
    let events = page
        .events
        .0
        .into_iter()
        .map(|value| {
            let transaction_hash = TxHash(Base58(value.tx_hash));
            let event = event(value.emitted_event).inspect_err(|_| {
                tracing::warn!(
                    tx_hash = %transaction_hash.0,
                    block_height = *value.block_height,
                    sequence_number = value.sequence_number,
                    "malformed history event; failing page"
                );
            })?;
            Ok(out::IdentifiedEvent {
                transaction_hash,
                block_height: value.block_height,
                sequence_number: value.sequence_number,
                event,
            })
        })
        .collect::<Result<Vec<_>, RpcServerError>>()?;
    Ok(out::EventPage {
        events,
        next_cursor: next_cursor(page.cursor),
        oldest_indexed_block_height: page.oldest_block,
        latest_indexed_block_height: page.latest_block,
    })
}

/// Project root-address or exact-block references without weakening failures.
pub fn transaction_page(
    value: wire::TransactionRefPageResult,
) -> Result<out::TransactionRefPage, RpcServerError> {
    let page = match value {
        wire::TransactionRefPageResult::IndexerQueryOk { page } => page,
        wire::TransactionRefPageResult::Default { outcome } => return Err(query_error(outcome)),
        _ => {
            return Err(invalid(
                Reason::UnsupportedValue,
                "unknown transaction-page result",
            ));
        }
    };
    Ok(out::TransactionRefPage {
        transactions: page
            .refs
            .0
            .into_iter()
            .map(|value| out::TransactionRef {
                transaction_hash: TxHash(Base58(value.tx_hash)),
                block_height: value.height,
            })
            .collect(),
        next_cursor: next_cursor(page.cursor),
        oldest_indexed_block_height: page.oldest_block,
        latest_indexed_block_height: page.latest_block,
    })
}

#[cfg(test)]
mod tests;
