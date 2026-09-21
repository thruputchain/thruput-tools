//! `groundhog_getEvents` handler.

use groundhog_hogd_backend::indexer as shared_indexer;
use groundhog_indexer::EventPageResult;
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::events::{EventPage, EventQuery};

use crate::config::HealthBackend;
use crate::context::GatewayContext;
use crate::metrics::REASON_PARSE_FAILED;
use groundhog_json_rpc_server::RpcServerError;

use super::{INDEXER, backend_deadline, timed_backend_call};

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
) -> Result<EventPage, RpcServerError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("indexer not connected".into()))?;

    let wire_query: groundhog_indexer::indexer::EventQuery = query.into();
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let result = timed_backend_call(
        &ctx.metrics,
        HealthBackend::Indexer,
        "get_events",
        INDEXER,
        deadline,
        client.get_events(&wire_query),
        |result| matches!(result, EventPageResult::IndexerQueryOk { .. }),
    )
    .await?;
    let page = shared_indexer::event_page(INDEXER, result)?;
    let decoded = shared_indexer::public_event_page(page);
    ctx.metrics
        .observe_malformed_events_dropped(REASON_PARSE_FAILED, decoded.dropped.len());
    for dropped in &decoded.dropped {
        tracing::warn!(
            tx_hash = ?dropped.tx_hash,
            block_height = ?dropped.block_height,
            sequence_number = dropped.sequence_number,
            "dropped malformed event from indexer",
        );
    }
    Ok(decoded.page)
}
