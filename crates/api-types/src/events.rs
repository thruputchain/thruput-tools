//! Event request and response types for the `getEvents` JSON-RPC
//! method.
//!
//! Request types are RPC-owned JSON projections of the indexer wire
//! types, so the JSON-RPC surface can use the same `0x`-prefixed
//! address/topic conventions as the rest of the API while still
//! converting losslessly to the indexer contract at the backend
//! boundary. Responses use a parsed `IdentifiedEvent` so consumers
//! receive topics and data already split out, not raw on-wire bytes.

use std::borrow::Cow;

use groundhog_indexer::indexer as wire_indexer;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use xdr_common::{BytesM, VecM};

use crate::hex::{HexBytes, Topic};
use crate::transaction::TxHash;
use crate::{BlockHeight, ContractAddress};

pub const MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER: u32 =
    wire_indexer::kMaxContractAddressesPerEventFilter;
pub const MAX_EVENT_FILTERS_PER_QUERY: u32 = wire_indexer::kMaxEventFiltersPerQuery;

pub type ContractAddressList = VecM<ContractAddress, MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER>;
pub type EventFilterList = VecM<EventFilter, MAX_EVENT_FILTERS_PER_QUERY>;

/// Direction to walk a paginated event stream.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum IterationOrder {
    Ascending,
    Descending,
}

/// Where to begin iteration on the initial call.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BlockAnchor {
    AtBeginning,
    AtEnd,
    AtHeight { height: BlockHeight },
}

/// Opaque pagination cursor authored by the server.
///
/// Clients must treat this as an opaque token: send back exactly the
/// value returned in the previous page's `cursor` field to continue
/// pagination. The encoding may change across server versions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct PaginationCursor(pub Vec<u8>);

impl Serialize for PaginationCursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        HexBytes(self.0.clone()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PaginationCursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        HexBytes::deserialize(deserializer).map(|bytes| Self(bytes.0))
    }
}

impl JsonSchema for PaginationCursor {
    fn schema_name() -> Cow<'static, str> {
        "PaginationCursor".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::PaginationCursor").into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^0x[0-9a-f]*$"
        })
    }
}

/// Positional topic filter. For each topic position (0, 1, 2):
///   - An empty list means "match anything at this position."
///   - A non-empty list means "match if the event's topic at this
///     position is in this list." OR within a position, AND across
///     positions.
///
/// Partial match: a filter that specifies only topic0 matches events
/// with more topics. Extra topics on the event past the filter's
/// specified positions are ignored. (This is the ethereum getLogs /
/// stellar-rpc post-#358 semantics.)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct EventTopicFilter {
    pub topic0: VecM<Topic>,
    pub topic1: VecM<Topic>,
    pub topic2: VecM<Topic>,
}

/// A single event-filter predicate: events must be emitted by one of
/// `contractAddresses` AND match the topic constraints. Required
/// non-empty `contractAddresses` — the indexer has no topic-indexed
/// column family and can't efficiently serve "any contract" scans.
/// (When such an index is added, the gateway relaxes the non-empty
/// check; the wire contract stays the same.)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct EventFilter {
    pub contract_addresses: ContractAddressList,
    pub topics: EventTopicFilter,
}

/// `getEvents` request. A candidate event matches the query iff it
/// matches at least one `EventFilter` in `filters` (OR across filters,
/// AND within each filter).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct EventQuery {
    pub filters: EventFilterList,
    pub order: IterationOrder,
    pub anchor: BlockAnchor,
    /// `0x` on the initial call. Non-empty to continue where a previous
    /// response left off; `order` and `anchor` in the request are
    /// ignored when a cursor is supplied — the cursor carries them.
    pub cursor: PaginationCursor,
    /// Clamped server-side to [1, 200]; `0` means server default (50).
    pub limit: u32,
}

/// A single emitted event, as stored by the hogd indexer.
///
/// The gateway parses hogd's `TxEmittedEvent.serialized_event` into
/// separate topics and data. The serialized format is:
/// `topic0 (32B) || topic1 (32B) || ... || data (remaining bytes)`.
/// `num_topics` (0–3) determines the split point. Max total 1024 bytes.
///
/// See `docs/gateway/system-design.md` "Event Parsing" for details.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// One page of events returned by `getEvents`.
///
/// To fetch the next page, send the same query back with `cursor`
/// set to the value returned here.  An empty `cursor` means the
/// stream is exhausted in the requested direction.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

impl From<EventTopicFilter> for wire_indexer::EventTopicFilter {
    fn from(value: EventTopicFilter) -> Self {
        Self {
            topic0: map_topic_list(value.topic0),
            topic1: map_topic_list(value.topic1),
            topic2: map_topic_list(value.topic2),
        }
    }
}

impl From<EventFilter> for wire_indexer::EventFilter {
    fn from(value: EventFilter) -> Self {
        Self {
            contract_addresses: VecM(
                value
                    .contract_addresses
                    .0
                    .into_iter()
                    .map(|addr| addr.0)
                    .collect(),
            ),
            topics: value.topics.into(),
        }
    }
}

impl From<EventQuery> for wire_indexer::EventQuery {
    fn from(value: EventQuery) -> Self {
        Self {
            filters: VecM(value.filters.0.into_iter().map(Into::into).collect()),
            order: value.order.into(),
            anchor: value.anchor.into(),
            cursor: value.cursor.into(),
            limit: value.limit,
        }
    }
}

impl From<IterationOrder> for groundhog_indexer::pagination::IterationOrder {
    fn from(value: IterationOrder) -> Self {
        match value {
            IterationOrder::Ascending => Self::IterationOrderAscending,
            IterationOrder::Descending => Self::IterationOrderDescending,
        }
    }
}

impl From<BlockAnchor> for groundhog_indexer::pagination::BlockAnchor {
    fn from(value: BlockAnchor) -> Self {
        match value {
            BlockAnchor::AtBeginning => Self::AnchorAtBeginning,
            BlockAnchor::AtEnd => Self::AnchorAtEnd,
            BlockAnchor::AtHeight { height } => Self::AnchorAtHeight { height },
        }
    }
}

impl From<PaginationCursor> for groundhog_indexer::pagination::PaginationCursor {
    fn from(value: PaginationCursor) -> Self {
        BytesM(value.0)
    }
}

impl From<groundhog_indexer::pagination::PaginationCursor> for PaginationCursor {
    fn from(value: groundhog_indexer::pagination::PaginationCursor) -> Self {
        Self(value.0)
    }
}

fn map_topic_list(topics: VecM<Topic>) -> VecM<groundhog_xdr::event::Topic> {
    VecM(
        topics
            .0
            .into_iter()
            .map(|topic| groundhog_xdr::event::Topic::from(topic.0))
            .collect(),
    )
}
