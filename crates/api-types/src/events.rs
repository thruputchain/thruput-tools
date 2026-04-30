//! Event-query types for the JSON-RPC `getEvents` method.
//!
//! Request shape (`EventQuery`, `EventFilter`, `EventTopicFilter`,
//! `BlockAnchor`, `IterationOrder`, `PaginationCursor`) is the indexer's
//! XDR shape, re-exported verbatim per the design note in
//! `rpc-server/src/backend.rs::ReadPlane` doc comment ("re-export the
//! XDR `EventQuery`/`EventPage` through `api-types` rather than
//! inventing parallel JSON-shaped variants — duplicated query schemas
//! drift").
//!
//! Response shape stays api-types-flavored: `EventPage::events` holds
//! parsed `IdentifiedEvent` entries (topics + data split out from
//! `TxEmittedEvent.serialized_event`) so JSON consumers don't have to
//! reverse the wire layout themselves.

use serde::{Deserialize, Serialize};

use crate::hex::{HexBytes, Topic};
use crate::transaction::TxHash;
use crate::{BlockHeight, ContractAddress};

// --- Request types: re-export from groundhog_indexer XDR ---
pub use groundhog_indexer::indexer::{EventFilter, EventFilterList, EventQuery, EventTopicFilter};
pub use groundhog_indexer::pagination::{BlockAnchor, IterationOrder, PaginationCursor};

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
/// Returned by `getEvents`.
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

/// Paginated `getEvents` response.
///
/// Mirrors `groundhog_indexer::indexer::EventPage` but uses api-types'
/// parsed `IdentifiedEvent` so JSON consumers get topics/data split,
/// not raw `serialized_event` bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPage {
    /// Matched events in the requested order.  May be empty even when
    /// `cursor` is non-empty: the filter was selective and the current
    /// scan window produced no matches; client should keep paginating.
    /// Empty events + empty cursor = stream exhausted.
    pub events: Vec<IdentifiedEvent>,

    /// Empty when the stream has been exhausted in the requested
    /// direction.  Non-empty: pass back in the next call's `cursor`
    /// argument.
    pub cursor: PaginationCursor,

    /// Highest block height the indexer has processed at response time.
    /// Compare to last-seen heights to decide caught-up state.
    pub latest_block: BlockHeight,

    /// Oldest block height the indexer still retains data for at
    /// response time.  Today always 0; meaningful once retention/
    /// pruning lands.
    pub oldest_block: BlockHeight,
}
