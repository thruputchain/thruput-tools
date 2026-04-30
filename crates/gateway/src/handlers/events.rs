//! `groundhog_getEvents` handler.
//!
//! Forwards a paginated `EventQuery` to hogd's
//! `IndexerBlockExplorer1::get_events` over x23 and re-parses the
//! returned events into the api-types-flavored `EventPage`.

use groundhog_rpc_api_types::events::{EventPage, EventQuery, IdentifiedEvent};
use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_rpc_api_types::transaction::TxHash;

use crate::context::GatewayContext;
use crate::error::GatewayError;

use super::parse_event::parse_emitted_event;

/// Query paginated, filterable contract events through the gateway.
///
/// Flow:
/// 1. The shared rpc-server has already enforced limit/cursor `DoS`
///    guards before dispatching here.
/// 2. Hand the `EventQuery` to the indexer; it owns filtering, anchor
///    seek, ordering, and per-page assembly.
/// 3. Parse each `TxEmittedEvent.serialized_event` into the
///    api-types `IdentifiedEvent` shape (topics + data split out)
///    so JSON consumers don't see raw wire bytes.
/// 4. Surface `cursor`, `latest_block`, `oldest_block` verbatim.
#[tracing::instrument(skip_all, fields(filters_len = query.filters.0.len(), limit = query.limit))]
pub async fn get_events(
    ctx: &GatewayContext,
    query: EventQuery,
) -> Result<EventPage, GatewayError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| GatewayError::BackendUnavailable("indexer not connected".into()))?;

    let page = client
        .get_events(&query)
        .await
        .map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;

    let events: Vec<IdentifiedEvent> = page
        .events
        .0
        .iter()
        .filter_map(|identified| {
            let event = parse_emitted_event(&identified.emitted_event)?;
            Some(IdentifiedEvent {
                tx_hash: TxHash(Hex0x(identified.proposed_tx_hash)),
                block_height: identified.block_height.clone(),
                sequence_number: identified.sequence_number,
                event,
            })
        })
        .collect();

    Ok(EventPage {
        events,
        cursor: page.cursor,
        latest_block: page.latest_block,
        oldest_block: page.oldest_block,
    })
}
