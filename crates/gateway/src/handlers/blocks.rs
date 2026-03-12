//! `groundhog_chainTip` and `groundhog_getBlockByNumber` handlers.
//!
//! Chain tip comes from the hogd scheduler (`get_tip`).
//! Block data source is TBD — see `docs/system-design.md` question #1.

use groundhog_rpc_api_types::block::{BlockInfo, ChainTip};
use groundhog_rpc_api_types::BlockHeight;

use crate::context::GatewayContext;
use crate::error::GatewayError;

/// Current chain tip: height + block hash.
///
/// Flow:
/// 1. Query hogd scheduler `get_tip()`
/// 2. Map to `ChainTip { height, block_hash }`
#[tracing::instrument(skip_all)]
pub async fn chain_tip(
    _ctx: &GatewayContext,
) -> Result<ChainTip, GatewayError> {
    // TODO: query scheduler
    //   let tip = ctx.scheduler.get_tip().await
    //       .map_err(|e| GatewayError::BackendUnavailable(e.to_string()))?;
    //   Ok(ChainTip { height: tip.height, block_hash: tip.hash })

    todo!("blocked on x23 scheduler client")
}

/// Block data at a given height.
///
/// **Data source is an open question.** Options:
/// - (A) New x23 query endpoint on chuckd-rs wrapping
///   `DiskStorage::lookup_catchup_block`
/// - (B) Index block headers in hogd and serve from indexer
/// - (C) Defer from Phase 1
///
/// See `docs/system-design.md` question #1.
#[tracing::instrument(skip_all, fields(height))]
pub async fn get_block_by_number(
    _ctx: &GatewayContext,
    height: BlockHeight,
) -> Result<Option<BlockInfo>, GatewayError> {
    let _ = height;
    todo!("blocked on block data source decision — see system-design.md question #1")
}
