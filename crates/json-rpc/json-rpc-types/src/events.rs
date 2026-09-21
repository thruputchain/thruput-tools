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
/// RPC-layer cap for topic values per event-filter position.
///
/// Unlike the adjacent constants, this does not mirror a hogd-indexer
/// protocol constant.
pub const MAX_TOPIC_VALUES_PER_POSITION: u32 = 16;

/// Contract addresses accepted by a single event filter.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct ContractAddressList(pub VecM<ContractAddress, MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER>);

/// Topic values accepted for a single topic position.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct TopicList(pub VecM<Topic, MAX_TOPIC_VALUES_PER_POSITION>);

/// Event filters accepted by a single event query.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct EventFilterList(pub VecM<EventFilter, MAX_EVENT_FILTERS_PER_QUERY>);

impl JsonSchema for ContractAddressList {
    fn schema_name() -> Cow<'static, str> {
        "ContractAddressList".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::ContractAddressList").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let items = generator.subschema_for::<ContractAddress>();
        json_schema!({
            "type": "array",
            "items": items,
            "maxItems": MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER,
            "description": "Contract addresses accepted by a single event filter. Mirrors the hogd-indexer per-filter contract-address bound."
        })
    }
}

impl JsonSchema for TopicList {
    fn schema_name() -> Cow<'static, str> {
        "TopicList".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::TopicList").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let items = generator.subschema_for::<Topic>();
        json_schema!({
            "type": "array",
            "items": items,
            "maxItems": MAX_TOPIC_VALUES_PER_POSITION,
            "description": "Topic values accepted for a single topic position. This is an RPC-layer cap, not a hogd-indexer protocol constant."
        })
    }
}

impl JsonSchema for EventFilterList {
    fn schema_name() -> Cow<'static, str> {
        "EventFilterList".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::EventFilterList").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let items = generator.subschema_for::<EventFilter>();
        json_schema!({
            "type": "array",
            "items": items,
            "maxItems": MAX_EVENT_FILTERS_PER_QUERY,
            "description": "Event filters accepted by a single event query. Mirrors the hogd-indexer per-query filter bound."
        })
    }
}

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
    pub topic0: TopicList,
    pub topic1: TopicList,
    pub topic2: TopicList,
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
    /// Values above the server's per-page cap (default 200) are
    /// rejected as invalid params; `0` means server default (50).
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

impl From<VecM<ContractAddress, MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER>> for ContractAddressList {
    fn from(value: VecM<ContractAddress, MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER>) -> Self {
        Self(value)
    }
}

impl From<ContractAddressList> for VecM<ContractAddress, MAX_CONTRACT_ADDRESSES_PER_EVENT_FILTER> {
    fn from(value: ContractAddressList) -> Self {
        value.0
    }
}

impl ContractAddressList {
    /// Iterate over the contained contract addresses.
    pub fn iter(&self) -> std::slice::Iter<'_, ContractAddress> {
        self.as_ref().iter()
    }
}

impl AsRef<[ContractAddress]> for ContractAddressList {
    fn as_ref(&self) -> &[ContractAddress] {
        self.0.as_ref()
    }
}

impl IntoIterator for ContractAddressList {
    type Item = ContractAddress;
    type IntoIter = std::vec::IntoIter<ContractAddress>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a ContractAddressList {
    type Item = &'a ContractAddress;
    type IntoIter = std::slice::Iter<'a, ContractAddress>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl From<VecM<Topic, MAX_TOPIC_VALUES_PER_POSITION>> for TopicList {
    fn from(value: VecM<Topic, MAX_TOPIC_VALUES_PER_POSITION>) -> Self {
        Self(value)
    }
}

impl From<TopicList> for VecM<Topic, MAX_TOPIC_VALUES_PER_POSITION> {
    fn from(value: TopicList) -> Self {
        value.0
    }
}

impl TopicList {
    /// Iterate over the contained topic values.
    pub fn iter(&self) -> std::slice::Iter<'_, Topic> {
        self.as_ref().iter()
    }
}

impl AsRef<[Topic]> for TopicList {
    fn as_ref(&self) -> &[Topic] {
        self.0.as_ref()
    }
}

impl IntoIterator for TopicList {
    type Item = Topic;
    type IntoIter = std::vec::IntoIter<Topic>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a TopicList {
    type Item = &'a Topic;
    type IntoIter = std::slice::Iter<'a, Topic>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl From<VecM<EventFilter, MAX_EVENT_FILTERS_PER_QUERY>> for EventFilterList {
    fn from(value: VecM<EventFilter, MAX_EVENT_FILTERS_PER_QUERY>) -> Self {
        Self(value)
    }
}

impl From<EventFilterList> for VecM<EventFilter, MAX_EVENT_FILTERS_PER_QUERY> {
    fn from(value: EventFilterList) -> Self {
        value.0
    }
}

impl EventFilterList {
    /// Iterate over the contained event filters.
    pub fn iter(&self) -> std::slice::Iter<'_, EventFilter> {
        self.as_ref().iter()
    }
}

impl AsRef<[EventFilter]> for EventFilterList {
    fn as_ref(&self) -> &[EventFilter] {
        self.0.as_ref()
    }
}

impl IntoIterator for EventFilterList {
    type Item = EventFilter;
    type IntoIter = std::vec::IntoIter<EventFilter>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a EventFilterList {
    type Item = &'a EventFilter;
    type IntoIter = std::slice::Iter<'a, EventFilter>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
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
            filters: VecM(value.filters.into_iter().map(Into::into).collect()),
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

fn map_topic_list(topics: TopicList) -> VecM<groundhog_xdr::event::Topic> {
    VecM(
        topics
            .into_iter()
            .map(|topic| groundhog_xdr::event::Topic::from(topic.0))
            .collect(),
    )
}
