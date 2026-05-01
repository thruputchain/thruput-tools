//! `groundhog_getEvents` handler.

use groundhog_rpc_api_types::events::{EventPage, EventQuery, IdentifiedEvent};
use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_rpc_api_types::transaction::TxHash;

use crate::context::GatewayContext;
use crate::error::GatewayError;

use super::parse_event::parse_emitted_event;

/// Implementation of the `groundhog_getEvents` JSON-RPC method.
/// Forwards the paginated `EventQuery` to the hogd indexer and
/// returns one page of parsed events to the caller.
///
/// Events whose payload disagrees with `num_topics` are dropped
/// (with a warning per drop) — pagination metadata still reflects
/// the indexer view, so cursor walks continue past the gap.
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

    let mut events: Vec<IdentifiedEvent> = Vec::with_capacity(page.events.0.len());
    for identified in &page.events.0 {
        if let Some(event) = parse_emitted_event(&identified.emitted_event) {
            events.push(IdentifiedEvent {
                tx_hash: TxHash(Hex0x(identified.proposed_tx_hash)),
                block_height: identified.block_height.clone(),
                sequence_number: identified.sequence_number,
                event,
            });
        }
    }

    Ok(EventPage {
        events,
        cursor: page.cursor,
        latest_block: page.latest_block,
        oldest_block: page.oldest_block,
    })
}
