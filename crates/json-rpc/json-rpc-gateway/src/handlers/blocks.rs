//! `groundhog_chainTip`, `groundhog_getBlockByNumber`, and
//! `groundhog_pendingTransactionCount` handlers.

use thruput_json_rpc_server::{RpcRequestContext, RpcServerError, TipReading};
use thruput_json_rpc_types::BlockHeight;
use thruput_json_rpc_types::block::BlockInfo;

use super::{INDEXER, backend_deadline, timed_backend_call};
use crate::config::HealthBackend;
use crate::context::GatewayContext;

/// Current chain tip (height + block hash) with the network config it
/// carries.
///
/// Routes through the per-gateway `TipCoalescer` so that concurrent
/// client requests share a fresh `dbgateway_get_config` RPC.
/// See `crate::tip_coalescer` for the linearizable coalescing design.
#[tracing::instrument(skip_all)]
pub async fn chain_tip(
    ctx: &GatewayContext,
    context: RpcRequestContext,
) -> Result<TipReading, RpcServerError> {
    ctx.tip_coalescer.tip_reading(context).await
}

/// Block data at a given height.
///
/// Reads the indexer's committed consensus projection.
#[tracing::instrument(skip_all, fields(?height))]
pub async fn get_block_by_number(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    height: BlockHeight,
) -> Result<Option<BlockInfo>, RpcServerError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcServerError::BackendUnavailable("indexer not connected".into()))?;
    timed_backend_call(
        &ctx.metrics,
        HealthBackend::Indexer,
        "get_block_by_height",
        INDEXER,
        backend_deadline(context, ctx.config.backend_request_timeout())?,
        async {
            client
                .get_block_by_height(&height)
                .await
                .map(|result| thruput_hogd_backend::indexer::block_lookup(INDEXER, &height, result))
        },
        Result::is_ok,
    )
    .await?
}

/// Number of transactions in the mempool.
///
/// **Deferred** — the scheduler-gateway RPC exists, but standalone handler
/// plumbing remains deferred. See `docs/gateway/system-design.md`, "Views and reads".
#[tracing::instrument(skip_all)]
pub async fn pending_transaction_count(_ctx: &GatewayContext) -> Result<u64, RpcServerError> {
    Err(RpcServerError::NotImplemented(
        "pendingTransactionCount".into(),
    ))
}

#[cfg(test)]
mod tests;
