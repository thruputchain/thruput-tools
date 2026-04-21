//! `groundhog_chainTip`, `groundhog_getBlockByNumber`, and
//! `groundhog_pendingTransactionCount` handlers.

use groundhog_rpc_api_types::BlockHeight;
use groundhog_rpc_api_types::block::{BlockInfo, ChainTip};

use crate::context::GatewayContext;
use crate::error::{self, GatewayError};

/// Current chain tip: height + block hash.
///
/// Routes through the per-gateway `TipCoalescer` so that concurrent
/// client requests share a single in-flight `schedcoord_get_tip` RPC.
/// See `crate::tip_coalescer` for the linearizable coalescing design.
#[tracing::instrument(skip_all)]
pub async fn chain_tip(ctx: &GatewayContext) -> Result<ChainTip, GatewayError> {
    ctx.tip_coalescer.chain_tip().await
}

/// Block data at a given height.
///
/// **Deferred** — data source is an open question.
/// See `docs/gateway/system-design.md` question #1.
#[tracing::instrument(skip_all, fields(height))]
pub async fn get_block_by_number(
    _ctx: &GatewayContext,
    _height: BlockHeight,
) -> Result<Option<BlockInfo>, GatewayError> {
    Err(error::not_implemented("getBlockByNumber"))
}

/// Number of transactions in the mempool.
///
/// **Deferred** — no `size()` RPC on the scheduler interface yet.
#[tracing::instrument(skip_all)]
pub async fn pending_transaction_count(_ctx: &GatewayContext) -> Result<u64, GatewayError> {
    Err(error::not_implemented("pendingTransactionCount"))
}
