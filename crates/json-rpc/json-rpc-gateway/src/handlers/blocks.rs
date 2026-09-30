//! `groundhog_chainTip`, `groundhog_getBlockByNumber`, and
//! `groundhog_pendingTransactionCount` handlers.

use groundhog_json_rpc_server::{RpcRequestContext, RpcServerError, TipReading};
use groundhog_json_rpc_types::BlockHeight;
use groundhog_json_rpc_types::block::BlockInfo;

use crate::context::GatewayContext;

/// Current chain tip (height + block hash) with the network config it
/// carries.
///
/// Routes through the per-gateway `TipCoalescer` so that concurrent
/// client requests share a single in-flight `schedgateway_get_tip` RPC.
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
/// **Deferred** — data source is an open question.
/// See `docs/gateway/system-design.md`, "Views and reads".
#[tracing::instrument(skip_all, fields(height))]
pub async fn get_block_by_number(
    _ctx: &GatewayContext,
    _height: BlockHeight,
) -> Result<Option<BlockInfo>, RpcServerError> {
    Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
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
