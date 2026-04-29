//! `groundhog_getContractEvents` handler.
//!
//! Queries hogd indexer (`get_contract_events`) via x23 RPC.
//! Shared RPC-layer code validates height ranges before dispatching here.

use groundhog_rpc_api_types::events::IdentifiedEvent;
use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_rpc_api_types::transaction::TxHash;
use groundhog_rpc_api_types::{BlockHeight, ContractAddress};

use crate::context::GatewayContext;
use crate::error::GatewayError;

use super::parse_event::parse_emitted_event;

/// Contract events filtered by address and height range.
///
/// Flow:
/// 1. Validate `to_height >= from_height` and enforce max range
/// 2. Query hogd indexer `get_contract_events(address, from, to)`
/// 3. Parse each `TxEmittedEvent.serialized_event` into topics + data
/// 4. Return `Vec<IdentifiedEvent>`
#[tracing::instrument(skip_all, fields(?address, from_height, to_height))]
pub async fn get_contract_events(
    ctx: &GatewayContext,
    address: ContractAddress,
    from_height: BlockHeight,
    to_height: BlockHeight,
) -> Result<Vec<IdentifiedEvent>, GatewayError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| GatewayError::BackendUnavailable("indexer not connected".into()))?;

    let result = client
        .get_contract_events(&address.0, &from_height, &to_height)
        .await
        .map_err(|e| GatewayError::BackendUnavailable(format!("indexer: {e}")))?;

    let events = result
        .iter()
        .filter_map(|identified| {
            let event = parse_emitted_event(&identified.emitted_event)?;
            Some(IdentifiedEvent {
                tx_hash: TxHash(Hex0x(identified.proposed_tx_hash)),
                block_height: identified.block_height.clone(),
                sequence_number: identified.sequence_number,
                event,
            })
        })
        .collect();

    Ok(events)
}
