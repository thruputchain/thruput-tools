//! `groundhog_chainTip`, `groundhog_getBlockByNumber`, and
//! `groundhog_pendingTransactionCount` handlers.

use groundhog_rpc_api_types::block::{BlockInfo, ChainTip};
use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_rpc_api_types::BlockHeight;
use groundhog_scheduler::sched_coord::SchedulerGetTipResult;

use crate::context::GatewayContext;
use crate::error::{self, GatewayError};

/// Current chain tip: height + block hash.
#[tracing::instrument(skip_all)]
pub async fn chain_tip(ctx: &GatewayContext) -> Result<ChainTip, GatewayError> {
    let guard = ctx.scheduler.read().await;
    let client = guard
        .as_ref()
        .ok_or_else(|| GatewayError::BackendUnavailable("scheduler not connected".into()))?;

    let result = client
        .schedcoord_get_tip()
        .await
        .map_err(|e| GatewayError::BackendUnavailable(e.to_string()))?;

    match result {
        SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => Ok(ChainTip {
            height: db_tip.height,
            block_hash: Hex0x(db_tip.block_hash.into_inner()),
        }),
        _ => Err(GatewayError::BackendUnavailable(
            "no committed blocks yet".into(),
        )),
    }
}

/// Block data at a given height.
///
/// **Deferred** — data source is an open question.
/// See `docs/system-design.md` question #1.
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
