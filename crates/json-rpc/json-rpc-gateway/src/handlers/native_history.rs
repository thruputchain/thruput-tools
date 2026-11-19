//! Native history calls retain typed outcomes and reject malformed pages in the caller.

use super::{INDEXER, backend_deadline, timed_backend_call};
use crate::{config::HealthBackend, context::GatewayContext};
use thruput_hogd_backend::native::history as decode;
use thruput_json_rpc_server::{
    RpcRequestContext, RpcServerError,
    native::read::{
        validate_block_transaction_page, validate_event_page, validate_root_transaction_page,
    },
};
use thruput_json_rpc_types::{requests as input, responses as out};

#[tracing::instrument(skip_all, fields(filters_len = request.filters.len(), limit = ?request.limit, has_cursor = request.cursor.is_some()))]
pub async fn events(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: input::EventQuery,
) -> Result<out::EventPage, RpcServerError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("indexer not connected".into()))?;
    let query = decode::event_query(&request)?;
    let result = timed_backend_call(
        &ctx.metrics,
        &ctx.lanes.indexer_scan,
        1,
        HealthBackend::Indexer,
        "get_events",
        INDEXER,
        backend_deadline(context, ctx.config.backend_request_timeout())?,
        async move { client.get_events(&query).await },
        |_| true,
    )
    .await?;
    let page = decode::event_page(result)?;
    validate_event_page(&request, &page, ctx.config.max_cursor_bytes)?;
    Ok(page)
}

#[tracing::instrument(skip_all, fields(address = ?request.address, limit = ?request.limit, has_cursor = request.cursor.is_some()))]
pub async fn root_transactions(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: input::RootAddressQuery,
) -> Result<out::TransactionRefPage, RpcServerError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("indexer not connected".into()))?;
    let query = decode::root_query(&request)?;
    let result = timed_backend_call(
        &ctx.metrics,
        &ctx.lanes.indexer_scan,
        1,
        HealthBackend::Indexer,
        "get_transactions_by_address",
        INDEXER,
        backend_deadline(context, ctx.config.backend_request_timeout())?,
        async move { client.get_transactions_by_address(&query).await },
        |_| true,
    )
    .await?;
    let page = decode::transaction_page(result)?;
    validate_root_transaction_page(&request, &page, ctx.config.max_cursor_bytes)?;
    Ok(page)
}

#[tracing::instrument(skip_all, fields(block_height = ?request.block_height, limit = ?request.limit, has_cursor = request.cursor.is_some()))]
pub async fn block_transactions(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    request: input::BlockPageRequest,
) -> Result<out::BlockTransactionPage, RpcServerError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("indexer not connected".into()))?;
    let query = decode::block_query(&request);
    let result = timed_backend_call(
        &ctx.metrics,
        &ctx.lanes.indexer_scan,
        1,
        HealthBackend::Indexer,
        "get_transactions_by_block",
        INDEXER,
        backend_deadline(context, ctx.config.backend_request_timeout())?,
        async move { client.get_transactions_by_block(&query).await },
        |_| true,
    )
    .await?;
    let page = decode::transaction_page(result)?;
    validate_block_transaction_page(&request, &page, ctx.config.max_cursor_bytes)?;
    Ok(page)
}
