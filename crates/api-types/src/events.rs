use serde::{Deserialize, Serialize};

use crate::hex::{HexBytes, Topic};
use crate::transaction::TxHash;
use crate::{BlockHeight, ContractAddress};

/// A single emitted event, as stored by the hogd indexer.
///
/// The gateway parses hogd's `TxEmittedEvent.serialized_event` into
/// separate topics and data. The serialized format is:
/// `topic0 (32B) || topic1 (32B) || ... || data (remaining bytes)`.
/// `num_topics` (0–3) determines the split point. Max total 1024 bytes.
///
/// See `docs/gateway/system-design.md` "Event Parsing" for details.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmittedEvent {
    /// Contract that emitted this event.
    pub contract_address: ContractAddress,

    /// Up to 3 topics per event, each exactly 32 bytes
    /// (indexed, filterable in Phase 4).
    pub topics: Vec<Topic>,

    /// Opaque event data (up to 1KB, contract-defined encoding).
    pub data: HexBytes,
}

/// Event with provenance — includes which tx emitted it.
/// Returned by `getContractEvents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifiedEvent {
    /// Transaction that emitted this event.
    pub tx_hash: TxHash,

    /// Block height at which the event was emitted.
    pub block_height: BlockHeight,

    /// Sequence number within the transaction's event log.
    pub sequence_number: u32,

    /// The event itself.
    pub event: EmittedEvent,
}
