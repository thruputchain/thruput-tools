use std::collections::BTreeMap;

use groundhog_json_rpc_types::events::{EmittedEvent, IdentifiedEvent, PaginationCursor};
use groundhog_json_rpc_types::hex::{HexBytes, Topic};
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_json_rpc_types::{BlockHeight, ContractAddress};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const EVENTS_LIST_JSON_SCHEMA_VERSION: u32 = 1;

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
    pub topics: BTreeMap<String, Topic>,
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

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::events::{EmittedEvent, IdentifiedEvent};
    use groundhog_json_rpc_types::hex::{HexBytes, Topic};
    use groundhog_json_rpc_types::transaction::TxHash;
    use groundhog_json_rpc_types::{BlockHeight, ContractAddress};

    use super::{EventDecodeStatus, EventListItemReport};

    fn tx_hash(byte: u8) -> TxHash {
        serde_json::from_value(serde_json::Value::String(format!(
            "0x{}",
            hex::encode([byte; 32])
        )))
        .expect("bug: transaction hash fixture should parse")
    }

    fn contract_address(byte: u8) -> ContractAddress {
        ContractAddress::from(groundhog_xdr::types::ContractAddress::from([byte; 32]))
    }

    #[test]
    fn raw_event_conversion_preserves_every_shared_field() {
        let event = IdentifiedEvent {
            tx_hash: tx_hash(0x11),
            block_height: BlockHeight::from(42),
            sequence_number: 3,
            event: EmittedEvent {
                contract_address: contract_address(0x22),
                topics: vec![Topic([0x33; 32])],
                data: HexBytes(vec![0x44, 0x55]),
            },
        };

        let item = EventListItemReport::from(event);

        assert_eq!(item.raw.tx_hash, tx_hash(0x11));
        assert_eq!(item.raw.block_height, BlockHeight::from(42));
        assert_eq!(item.raw.sequence_number, 3);
        assert_eq!(item.raw.contract_address, contract_address(0x22));
        assert_eq!(item.raw.topics, vec![Topic([0x33; 32])]);
        assert_eq!(item.raw.data, HexBytes(vec![0x44, 0x55]));
        assert_eq!(item.decode_status, EventDecodeStatus::NotRequested);
        assert!(item.decoded.is_none());
        assert!(item.decode_error.is_none());
    }

    #[test]
    fn raw_event_serialization_includes_final_decoding_placeholders() {
        let item = EventListItemReport::from(IdentifiedEvent {
            tx_hash: tx_hash(0x11),
            block_height: BlockHeight::from(42),
            sequence_number: 3,
            event: EmittedEvent {
                contract_address: contract_address(0x22),
                topics: vec![],
                data: HexBytes(vec![]),
            },
        });

        let value = serde_json::to_value(item).expect("bug: event report should serialize");

        assert_eq!(value["decodeStatus"], "notRequested");
        assert!(value["decoded"].is_null());
        assert!(value.get("decodeError").is_none());
    }
}
