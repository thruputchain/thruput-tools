use serde::{Deserialize, Serialize};

use crate::transaction::TxHash;
use crate::{BlockHeight, ContractAddress};

/// A single emitted event, as stored by the hogd indexer.
/// Maps to hogd's TxEmittedEvent + identification metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedEvent {
    pub contract_address: ContractAddress,

    /// Up to 3 topics per event (indexed, filterable in Phase 4).
    pub topics: Vec<Vec<u8>>,

    /// Opaque event data (up to 1KB, contract-defined encoding).
    pub data: Vec<u8>,
}

/// Event with provenance — includes which tx emitted it.
/// Returned by getContractEvents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifiedEvent {
    pub tx_hash: TxHash,
    pub block_height: BlockHeight,
    pub sequence_number: u32,
    pub event: EmittedEvent,
}
