use thruput_json_rpc_types::events::{EmittedEvent, IdentifiedEvent};
use thruput_json_rpc_types::hex::{HexBytes, Topic};
use thruput_json_rpc_types::transaction::TxHash;
use thruput_json_rpc_types::{BlockHeight, ContractAddress};

use thruput_cli::events_json::{EventDecodeStatus, EventListItemReport};

fn tx_hash(byte: u8) -> TxHash {
    TxHash(thruput_xdr::types::SHA256Hash::from([byte; 32]).into())
}

fn contract_address(byte: u8) -> ContractAddress {
    ContractAddress::from(thruput_xdr::types::ContractAddress::from([byte; 32]))
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
