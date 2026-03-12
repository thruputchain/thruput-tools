//! `groundhog_getContractEvents` handler.
//!
//! Queries hogd indexer (`get_contract_events`) via x23 RPC.
//! The gateway enforces height range limits since hogd has none.

use groundhog_rpc_api_types::events::IdentifiedEvent;
use groundhog_rpc_api_types::{BlockHeight, ContractAddress};

use crate::context::GatewayContext;
use crate::error::GatewayError;

/// Contract events filtered by address and height range.
///
/// Flow:
/// 1. Validate `to_height >= from_height`
/// 2. Enforce max height range (`config.max_event_height_range`)
/// 3. Query hogd indexer `get_contract_events(from, to, address)`
/// 4. Parse each `TxEmittedEvent.serialized_event` into topics + data
/// 5. Return `Vec<IdentifiedEvent>`
///
/// Returns `RANGE_TOO_LARGE` (-32011) if the height span exceeds
/// the configured maximum.
#[tracing::instrument(skip_all, fields(?address, from_height, to_height))]
pub async fn get_contract_events(
    ctx: &GatewayContext,
    address: ContractAddress,
    from_height: BlockHeight,
    to_height: BlockHeight,
) -> Result<Vec<IdentifiedEvent>, GatewayError> {
    // Validate range
    if to_height < from_height {
        return Err(GatewayError::RangeTooLarge(
            "to_height must be >= from_height".into(),
        ));
    }

    let span = to_height.saturating_sub(from_height);
    if span > ctx.config.max_event_height_range as i64 {
        return Err(GatewayError::RangeTooLarge(format!(
            "height range {} exceeds maximum {}",
            span, ctx.config.max_event_height_range,
        )));
    }

    // TODO: query indexer
    //   let events = ctx.indexer
    //       .get_contract_events(from_height, to_height, &address).await
    //       .map_err(|e| GatewayError::BackendUnavailable(e.to_string()))?;
    //
    // TODO: parse each event's serialized_event into topics + data
    //   let identified = events.iter().map(|e| parse_identified_event(e)).collect();

    let _ = address;
    todo!("blocked on x23 indexer client")
}
