//! `groundhog_getContractEvents` handler.
//!
//! Queries hogd indexer (`get_contract_events`) via x23 RPC.
//! The gateway enforces height range limits since hogd has none.

use groundhog_rpc_api_types::events::IdentifiedEvent;
use groundhog_rpc_api_types::{BlockHeight, ContractAddress};

use crate::context::GatewayContext;
use crate::error::{self, GatewayError};

/// Contract events filtered by address and height range.
///
/// **Indexer query deferred** — not available in Rust yet.
///
/// Implementation plan:
/// 1. Validate `to_height >= from_height` and enforce max range
/// 2. Query hogd indexer `get_contract_events(from, to, address)`
/// 3. Parse each `TxEmittedEvent.serialized_event` into topics + data
/// 4. Return `Vec<IdentifiedEvent>`
#[tracing::instrument(skip_all, fields(?address, from_height, to_height))]
pub async fn get_contract_events(
    ctx: &GatewayContext,
    address: ContractAddress,
    from_height: BlockHeight,
    to_height: BlockHeight,
) -> Result<Vec<IdentifiedEvent>, GatewayError> {
    // Validate range.
    if to_height < from_height {
        return Err(GatewayError::InvalidParam(
            "to_height must be >= from_height".into(),
        ));
    }

    let span = (*to_height).saturating_sub(*from_height);
    if span > ctx.config.max_event_height_range as i64 {
        return Err(GatewayError::RangeTooLarge(format!(
            "height range {} exceeds maximum {}",
            span, ctx.config.max_event_height_range,
        )));
    }

    let _ = address;
    Err(error::not_implemented("getContractEvents"))
}
