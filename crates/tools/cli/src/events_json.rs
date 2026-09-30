use std::collections::BTreeMap;

use groundhog_json_rpc_types::events::{EmittedEvent, IdentifiedEvent, PaginationCursor};
use groundhog_json_rpc_types::hex::{HexBytes, Topic};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_json_rpc_types::{BlockHeight, ContractAddress};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const EVENTS_LIST_JSON_SCHEMA_VERSION: u32 = 2;

/// Effective positional topic filters for one event query.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventFiltersReport {
    pub topic0: Vec<Topic>,
    pub topic1: Vec<Topic>,
    pub topic2: Vec<Topic>,
}

/// Origin of a contract spec supplied for event decoding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EventSpecSource {
    UserSupplied,
}

/// User-supplied contract spec metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventSpecReport {
    pub source: EventSpecSource,
    pub path: String,
    pub selected_event: Option<String>,
}

/// Decoding state for one returned event.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EventDecodeStatus {
    NotRequested,
    UnknownSignature,
    Decoded,
    Malformed,
}

/// Typed custom-event annotation populated when a supplied spec matches.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedEventReport {
    pub name: String,
    /// Indexed topics by name: address topics as C `StrKeys`, others as `0x` hex.
    pub topics: BTreeMap<String, String>,
    pub fields: BTreeMap<String, serde_json::Value>,
}

/// Command-owned flattened projection of one shared RPC event.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RawEventReport {
    pub tx_hash: TxHash,
    pub block_height: BlockHeight,
    pub sequence_number: u32,
    pub contract_address: ContractAddress,
    pub topics: Vec<Topic>,
    pub data: HexBytes,
}

impl From<IdentifiedEvent> for RawEventReport {
    fn from(value: IdentifiedEvent) -> Self {
        let IdentifiedEvent {
            tx_hash,
            block_height,
            sequence_number,
            event,
        } = value;
        let EmittedEvent {
            contract_address,
            topics,
            data,
        } = event;
        Self {
            tx_hash,
            block_height,
            sequence_number,
            contract_address,
            topics,
            data,
        }
    }
}

/// One raw event plus stable decoding placeholders.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EventListItemReport {
    #[serde(flatten)]
    pub raw: RawEventReport,
    pub decode_status: EventDecodeStatus,
    pub decoded: Option<DecodedEventReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_error: Option<String>,
}

impl From<IdentifiedEvent> for EventListItemReport {
    fn from(value: IdentifiedEvent) -> Self {
        Self {
            raw: value.into(),
            decode_status: EventDecodeStatus::NotRequested,
            decoded: None,
            decode_error: None,
        }
    }
}

/// Stable JSON data for one page of contract events.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EventListReport {
    pub schema_version: u32,
    pub profile: String,
    pub rpc_url: String,
    pub contract_input: String,
    pub contract_address: ContractAddress,
    pub filters: EventFiltersReport,
    pub spec: Option<EventSpecReport>,
    pub events: Vec<EventListItemReport>,
    pub cursor: PaginationCursor,
    pub latest_block: BlockHeight,
    pub oldest_block: BlockHeight,
}
