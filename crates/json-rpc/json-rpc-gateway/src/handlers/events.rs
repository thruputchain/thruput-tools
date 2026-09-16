//! `groundhog_getEvents` handler.

use groundhog_indexer::EventPageResult;
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::events::{EventPage, EventQuery, IdentifiedEvent};
use groundhog_json_rpc_types::hex::Hex0x;
use groundhog_json_rpc_types::transaction::TxHash;
use std::time::Instant;

use crate::config::HealthBackend;
use crate::context::GatewayContext;
use crate::error::GatewayError;
use crate::metrics::{BackendRequestOutcome, REASON_PARSE_FAILED};

use super::parse_event::parse_emitted_event;
use super::{backend_deadline, event_page_from_result};

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
    context: RpcRequestContext,
    query: EventQuery,
) -> Result<EventPage, GatewayError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| GatewayError::BackendUnavailable("indexer not connected".into()))?;

    let wire_query: groundhog_indexer::indexer::EventQuery = query.into();
    let started_at = Instant::now();
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let rpc_result = tokio::time::timeout_at(deadline.into(), client.get_events(&wire_query)).await;
    ctx.metrics.observe_backend_request(
        HealthBackend::Indexer,
        "get_events",
        match &rpc_result {
            Err(_) => BackendRequestOutcome::Timeout,
            Ok(Ok(EventPageResult::IndexerQueryOk { .. })) => BackendRequestOutcome::Ok,
            Ok(Ok(_) | Err(_)) => BackendRequestOutcome::Error,
        },
        started_at.elapsed(),
    );
    let result = rpc_result
        .map_err(|_| GatewayError::BackendTimeout)?
        .map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;
    let page = event_page_from_result(result)?;

    let mut events: Vec<IdentifiedEvent> = Vec::with_capacity(page.events.0.len());
    for identified in page.events.0 {
        if let Some(event) = parse_emitted_event(identified.emitted_event) {
            events.push(IdentifiedEvent {
                tx_hash: TxHash(Hex0x(identified.tx_hash)),
                block_height: identified.block_height,
                sequence_number: identified.sequence_number,
                event,
            });
        } else {
            ctx.metrics
                .observe_malformed_event_dropped(REASON_PARSE_FAILED);
            tracing::warn!(
                tx_hash = ?identified.tx_hash,
                block_height = ?identified.block_height,
                sequence_number = identified.sequence_number,
                "dropped malformed event from indexer",
            );
        }
    }

    Ok(EventPage {
        events,
        cursor: page.cursor.into(),
        latest_block: page.latest_block,
        oldest_block: page.oldest_block,
    })
}
