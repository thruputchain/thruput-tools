use serde_json::Value;

use groundhog_json_rpc_types::airdrop::{AirdropRequest, ClientNonce};
use groundhog_json_rpc_types::block::{
    BlockInfo, BlockProjectionError, ChainTip, consensus_value_hash,
};
use groundhog_json_rpc_types::events::{
    BlockAnchor, ContractAddressList, EmittedEvent, EventFilter, EventFilterList, EventQuery,
    EventTopicFilter, IdentifiedEvent, IterationOrder, PaginationCursor, TopicList,
};
use groundhog_json_rpc_types::hex::{Hex0x, HexBytes, Topic};
use groundhog_json_rpc_types::network_descriptor::{
    AirdropDescriptor, FaucetDescriptor, NETWORK_DESCRIPTOR_SCHEMA_VERSION, NativeHogDescriptor,
    NetworkDescriptor, NetworkDescriptorEndpoints,
};
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::server_limits::ServerLimits;
use groundhog_json_rpc_types::state::StorageItemInfo;
use groundhog_json_rpc_types::transaction::{
    AddressTransactionQuery, TransactionRef, TransactionRefPage, TransactionStatus, TxHash,
};
use groundhog_json_rpc_types::view::{RootInvocationOutput, ViewRequest, ViewResponse};
use groundhog_scheduler::{ConsensusValue, ProposalTxsHash};
use groundhog_xdr::{
    header::ProposalHeader,
    types::{
        ConfigHash, ConsensusValueHash, EventsHash, ExtensionPoint, NetworkHash, Protocol,
        SHA256Hash as XdrHash, StorageShortHash, TimePoint, TxsHash,
    },
    validator::ValidatorID,
};

use groundhog_json_rpc_types::{BlockHeight, SHA256Hash};
use xdr_common::{BytesM, VecM};

fn zero_hash() -> SHA256Hash {
    Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32]))
}

fn zero_address() -> groundhog_json_rpc_types::ContractAddress {
    Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32]))
}

fn test_consensus_value() -> ConsensusValue {
    ConsensusValue {
        protocol: Protocol::new(1),
        ext: ExtensionPoint::_0,
        prev_hash: ConsensusValueHash::new(XdrHash::from([7; 32])),
        proposal_header: ProposalHeader {
            ext: ExtensionPoint::_0,
            network_hash: NetworkHash::new(XdrHash::from([1; 32])),
            config_hash: ConfigHash::new(XdrHash::from([8; 32])),
            proposer: ValidatorID::new([2; 32]),
            block_timepoint: TimePoint::new(123),
            block_height: BlockHeight::from(99i64),
            txs_hash: TxsHash::new(XdrHash::from([3; 32])),
            storage_hash: StorageShortHash::new(XdrHash::from([4; 32])),
            events_hash: EventsHash::new(XdrHash::from([5; 32])),
        },
        proposal_txs_hash: ProposalTxsHash::new(XdrHash::from([6; 32])),
        upgrades: VecM(vec![BytesM(vec![0xaa, 0xbb])]),
    }
}

fn test_network_descriptor() -> NetworkDescriptor {
    NetworkDescriptor::new(
        "devnet-preview".into(),
        groundhog_json_rpc_types::NetworkHash::from(XdrHash::from([0x12; 32])),
        "x23".into(),
        zero_hash(),
        NetworkDescriptorEndpoints::new("https://rpc.devnet.example".into()),
        NativeHogDescriptor::new(
            zero_address(),
            7,
            HexBytes(vec![0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f]),
        ),
        FaucetDescriptor::new(zero_address()),
    )
    .with_airdrop(AirdropDescriptor::new(
        "100000000".into(),
        43_200,
        10,
        3_600,
        1,
    ))
}

#[test]
fn chain_tip_wire_format() {
    let tip = ChainTip {
        height: BlockHeight::from(42i64),
        block_hash: zero_hash(),
    };
    let v: Value = serde_json::to_value(&tip).unwrap();
    assert_eq!(v["height"], "42");
    assert!(v["blockHash"].as_str().unwrap().starts_with("0x"));
    assert_eq!(v["blockHash"].as_str().unwrap().len(), 66); // 0x + 64
    // Verify camelCase
    assert!(v.get("block_hash").is_none());
}

#[test]
fn network_descriptor_wire_format_is_public_and_camel_case() {
    let descriptor = test_network_descriptor();

    let value: Value = serde_json::to_value(&descriptor).unwrap();

    assert_eq!(value["schemaVersion"], NETWORK_DESCRIPTOR_SCHEMA_VERSION);
    assert_eq!(value["kind"], "groundhog-network");
    assert_eq!(value["networkId"], "devnet-preview");
    assert_eq!(value["networkHash"], format!("0x{}", "12".repeat(32)));
    assert_eq!(value["endpoints"]["rpcUrl"], "https://rpc.devnet.example");
    assert!(value["endpoints"].get("faucetUrl").is_none());
    assert_eq!(value["nativeHog"]["decimals"], 7);
    assert_eq!(
        value["nativeHog"]["transferEntrypoint"],
        "0x63285b3523f9dc3f"
    );
    assert_eq!(value["airdrop"]["rawAmount"], "100000000");
    assert!(value["airdrop"].get("amountHog").is_none());

    assert!(value.get("schema_version").is_none());
    assert!(value.get("bootstrapAccountDerivation").is_none());
    assert!(value.get("bootstrapAccounts").is_none());
    assert!(value.get("seed").is_none());
    assert!(value.get("privateKey").is_none());
    assert!(value.get("bootstrap").is_none());

    let encoded = serde_json::to_string(&descriptor).unwrap();
    assert!(!encoded.contains("bootstrapAccountDerivation"));
    assert!(!encoded.contains("bootstrapAccounts"));
    assert!(!encoded.contains("privateKey"));
    assert!(!encoded.contains("seed"));
    assert!(!encoded.contains("createWalletEntrypoint"));
    assert!(!encoded.contains("doActionEntrypoint"));
    assert!(!encoded.contains("deployEntrypoint"));

    let roundtrip: NetworkDescriptor = serde_json::from_value(value).unwrap();
    assert_eq!(roundtrip, descriptor);
}

#[test]
fn network_descriptor_accepts_missing_airdrop_policy() {
    let mut value = serde_json::to_value(test_network_descriptor()).unwrap();
    value.as_object_mut().unwrap().remove("airdrop");

    let descriptor: NetworkDescriptor = serde_json::from_value(value).unwrap();

    assert!(descriptor.airdrop.is_none());
}

#[test]
fn transaction_status_pending() {
    let s = TransactionStatus::Pending;
    let v: Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["status"], "pending");
}

#[test]
fn transaction_status_committed() {
    let s = TransactionStatus::Committed {
        height: BlockHeight::from(100i64),
    };
    let v: Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["status"], "committed");
    assert_eq!(v["height"], "100");
    // No block_hash field
    assert!(v.get("blockHash").is_none());
    assert!(v.get("block_hash").is_none());
}

#[test]
fn transaction_status_unknown() {
    let s = TransactionStatus::Unknown;
    let v: Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["status"], "unknown");
}

#[test]
fn transaction_receipt_wire_format() {
    let receipt = TransactionReceipt {
        tx_hash: TxHash(zero_hash()),
        height: BlockHeight::from(50i64),
        status: ExecStatus::Ok,
        events: vec![],
        return_data: None,
        abort_data: None,
        gas_used: 123_456,
    };
    let v: Value = serde_json::to_value(&receipt).unwrap();
    assert!(v["txHash"].as_str().unwrap().starts_with("0x"));
    assert_eq!(v["gasUsed"], "123456");
    assert_eq!(v["height"], "50");
    assert_eq!(v["status"]["result"], "ok");
    assert!(v["events"].as_array().unwrap().is_empty());
    // camelCase
    assert!(v.get("tx_hash").is_none());

    let decoded: TransactionReceipt = serde_json::from_value(v).unwrap();
    assert_eq!(decoded.gas_used, 123_456);
}

#[test]
fn view_request_wire_format() {
    let request = ViewRequest {
        contract: zero_address(),
        entrypoint: HexBytes(vec![1, 2, 3, 4, 5, 6, 7, 8]),
        calldata: HexBytes(vec![0xca, 0xfe]),
        caller: None,
        gas_limit: None,
    };
    let v: Value = serde_json::to_value(&request).unwrap();
    assert!(v["contract"].as_str().unwrap().starts_with("0x"));
    assert_eq!(v["entrypoint"], "0x0102030405060708");
    assert_eq!(v["calldata"], "0xcafe");
    assert!(v.get("caller").is_none());
    assert!(v.get("gasLimit").is_none());

    let with_gas = ViewRequest {
        gas_limit: Some(250_000),
        ..request
    };
    let v: Value = serde_json::to_value(&with_gas).unwrap();
    assert_eq!(v["gasLimit"], "250000");
    let decoded: ViewRequest = serde_json::from_value(v).unwrap();
    assert_eq!(decoded.gas_limit, Some(250_000));
}

#[test]
fn server_limits_wire_format() {
    let limits = ServerLimits::new(10_000_000, 65_536, 200, 200, 256);
    let v: Value = serde_json::to_value(limits).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "maxViewGas": "10000000",
            "maxViewCalldataBytes": "65536",
            "maxEventsPerPage": 200,
            "maxTransactionsPerPage": 200,
            "maxCursorBytes": "256",
        })
    );
    let decoded: ServerLimits = serde_json::from_value(v).unwrap();
    assert_eq!(decoded, limits);
}

#[test]
fn view_response_wire_format() {
    let response = ViewResponse {
        output: RootInvocationOutput::Return {
            return_data: HexBytes(vec![0x2a]),
        },
        events: vec![],
        height: BlockHeight::from(7i64),
        gas_used: 1234,
    };
    let v: Value = serde_json::to_value(&response).unwrap();
    assert_eq!(v["output"]["result"], "return");
    assert_eq!(v["output"]["returnData"], "0x2a");
    assert_eq!(v["height"], "7");
    assert_eq!(v["gasUsed"], "1234");
    assert!(v.get("gas_used").is_none());
    assert!(v["events"].as_array().unwrap().is_empty());
}

#[test]
fn view_abort_output_wire_format() {
    let output = RootInvocationOutput::Abort {
        abort_data: HexBytes(vec![0xde, 0xad]),
    };
    let v: Value = serde_json::to_value(&output).unwrap();
    assert_eq!(v["result"], "abort");
    assert_eq!(v["abortData"], "0xdead");
    assert!(v.get("abort_data").is_none());
}

#[test]
fn view_fault_output_wire_format() {
    let v: Value = serde_json::to_value(&RootInvocationOutput::Fault).unwrap();
    assert_eq!(v["result"], "fault");
    assert_eq!(v.as_object().unwrap().len(), 1);
}

#[test]
fn exec_status_reverted() {
    let s = ExecStatus::Reverted {
        message: Some("out of funds".into()),
    };
    let v: Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["result"], "reverted");
    assert_eq!(v["message"], "out of funds");
}

#[test]
fn exec_status_deterministic_error() {
    let s = ExecStatus::DeterministicError {
        kind: "out_of_gas".into(),
    };
    let v: Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["result"], "deterministicError");
    assert_eq!(v["kind"], "out_of_gas");
}

#[test]
fn deterministic_error_fault_kind_wire_value() {
    let s = ExecStatus::DeterministicError {
        kind: groundhog_json_rpc_types::receipt::DETERMINISTIC_ERROR_KIND_FAULT.into(),
    };
    let v: Value = serde_json::to_value(&s).unwrap();
    assert_eq!(v["result"], "deterministicError");
    assert_eq!(v["kind"], "fault");
}

#[test]
fn emitted_event_wire_format() {
    let event = EmittedEvent {
        contract_address: zero_address(),
        topics: vec![Topic([0xab; 32])],
        data: HexBytes(vec![0xca, 0xfe]),
    };
    let v: Value = serde_json::to_value(&event).unwrap();
    assert!(v["contractAddress"].as_str().unwrap().starts_with("0x"));
    assert!(v["topics"][0].as_str().unwrap().starts_with("0x"));
    assert_eq!(v["topics"][0].as_str().unwrap().len(), 66);
    assert!(v["data"].as_str().unwrap().starts_with("0x"));
    // camelCase
    assert!(v.get("contract_address").is_none());
}

#[test]
fn identified_event_wire_format() {
    let event = IdentifiedEvent {
        tx_hash: TxHash(zero_hash()),
        block_height: BlockHeight::from(10i64),
        sequence_number: 0,
        event: EmittedEvent {
            contract_address: zero_address(),
            topics: vec![],
            data: HexBytes(vec![]),
        },
    };
    let v: Value = serde_json::to_value(&event).unwrap();
    assert!(v["txHash"].as_str().unwrap().starts_with("0x"));
    assert_eq!(v["blockHeight"], "10");
    assert_eq!(v["sequenceNumber"], 0);
    assert!(v.get("tx_hash").is_none());
    assert!(v.get("block_hash").is_none());
    assert!(v.get("sequence_number").is_none());
}

#[test]
fn event_query_wire_format_uses_rpc_hex_projections() {
    let query = EventQuery {
        filters: EventFilterList(VecM(vec![EventFilter {
            contract_addresses: ContractAddressList(VecM(vec![zero_address()])),
            topics: EventTopicFilter {
                topic0: TopicList(VecM(vec![Topic([0xab; 32])])),
                topic1: TopicList(VecM(vec![])),
                topic2: TopicList(VecM(vec![])),
            },
        }])),
        order: IterationOrder::Ascending,
        anchor: BlockAnchor::AtBeginning,
        cursor: PaginationCursor(vec![]),
        limit: 50,
    };
    let v: Value = serde_json::to_value(&query).unwrap();
    assert!(
        v["filters"][0]["contractAddresses"][0]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
    assert!(v["filters"][0].get("contract_addresses").is_none());
    assert!(
        v["filters"][0]["topics"]["topic0"][0]
            .as_str()
            .unwrap()
            .starts_with("0x")
    );
    assert_eq!(v["order"], "ascending");
    assert_eq!(v["anchor"]["kind"], "atBeginning");
    assert_eq!(v["cursor"], "0x");

    let roundtrip: EventQuery = serde_json::from_value(v).unwrap();
    assert_eq!(roundtrip, query);
}

/// Request types reject a field they do not define at every nesting level,
/// so a caller who adds one (a block height on a latest-state read) learns
/// the server will not honor it.
#[test]
fn request_types_reject_unknown_fields() {
    fn rejects<T: serde::de::DeserializeOwned + std::fmt::Debug>(valid: &Value, path: &[&str]) {
        serde_json::from_value::<T>(valid.clone()).expect("bug: valid request should parse");
        let mut bad = valid.clone();
        let mut target = &mut bad;
        for key in path {
            target = if let Ok(index) = key.parse::<usize>() {
                &mut target[index]
            } else {
                &mut target[*key]
            };
        }
        target["blockHeight"] = Value::from(62_256);
        let parsed = serde_json::from_value::<T>(bad);
        let err = parsed.expect_err(&format!("{path:?}: unknown field should be rejected"));
        assert!(
            err.to_string().contains("unknown field `blockHeight`"),
            "{path:?}: {err}"
        );
    }

    let view = serde_json::to_value(ViewRequest {
        contract: zero_address(),
        entrypoint: HexBytes(vec![0; 8]),
        calldata: HexBytes(vec![]),
        caller: None,
        gas_limit: None,
    })
    .unwrap();
    rejects::<ViewRequest>(&view, &[]);

    let filter = EventFilter {
        contract_addresses: ContractAddressList(VecM(vec![zero_address()])),
        topics: EventTopicFilter {
            topic0: TopicList(VecM(vec![])),
            topic1: TopicList(VecM(vec![])),
            topic2: TopicList(VecM(vec![])),
        },
    };
    for anchor in [
        BlockAnchor::AtBeginning,
        BlockAnchor::AtEnd,
        BlockAnchor::AtHeight { height: 7.into() },
    ] {
        let events = serde_json::to_value(EventQuery {
            filters: EventFilterList(VecM(vec![filter.clone()])),
            order: IterationOrder::Ascending,
            anchor: anchor.clone(),
            cursor: PaginationCursor(vec![]),
            limit: 50,
        })
        .unwrap();
        rejects::<EventQuery>(&events, &[]);
        rejects::<EventQuery>(&events, &["anchor"]);
        rejects::<EventQuery>(&events, &["filters", "0"]);
        rejects::<EventQuery>(&events, &["filters", "0", "topics"]);

        let by_address = serde_json::to_value(AddressTransactionQuery {
            address: zero_address(),
            order: IterationOrder::Descending,
            anchor,
            cursor: PaginationCursor(vec![]),
            limit: 50,
        })
        .unwrap();
        rejects::<AddressTransactionQuery>(&by_address, &[]);
        rejects::<AddressTransactionQuery>(&by_address, &["anchor"]);
    }

    let airdrop = serde_json::to_value(AirdropRequest {
        recipient: zero_address(),
        client_nonce: ClientNonce::new(vec![1; 16]).unwrap(),
    })
    .unwrap();
    rejects::<AirdropRequest>(&airdrop, &[]);
}

#[test]
fn address_transaction_query_wire_format_uses_rpc_hex_projections() {
    let query = AddressTransactionQuery {
        address: zero_address(),
        order: IterationOrder::Descending,
        anchor: BlockAnchor::AtEnd,
        cursor: PaginationCursor(vec![]),
        limit: 50,
    };
    let v: Value = serde_json::to_value(&query).unwrap();
    assert!(v["address"].as_str().unwrap().starts_with("0x"));
    assert_eq!(v["order"], "descending");
    assert_eq!(v["anchor"]["kind"], "atEnd");
    assert_eq!(v["cursor"], "0x");
    assert_eq!(v["limit"], 50);

    let roundtrip: AddressTransactionQuery = serde_json::from_value(v).unwrap();
    assert_eq!(roundtrip, query);
}

#[test]
fn transaction_ref_page_wire_format() {
    let page = TransactionRefPage {
        refs: vec![TransactionRef {
            tx_hash: TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0xab; 32]))),
            height: BlockHeight::from(7),
        }],
        cursor: PaginationCursor(vec![0x01, 0x02]),
        latest_block: BlockHeight::from(9),
        oldest_block: BlockHeight::from(0),
    };
    let v: Value = serde_json::to_value(&page).unwrap();
    assert!(v["refs"][0]["txHash"].as_str().unwrap().starts_with("0x"));
    assert!(v["refs"][0].get("tx_hash").is_none());
    assert_eq!(v["refs"][0]["height"], "7");
    assert_eq!(v["cursor"], "0x0102");
    assert_eq!(v["latestBlock"], "9");
    assert_eq!(v["oldestBlock"], "0");
    assert!(v.get("latest_block").is_none());
}

#[test]
fn block_info_wire_format() {
    let value = test_consensus_value();
    let expected_block_hash =
        consensus_value_hash(&value).expect("bug: valid consensus value should hash");
    let info = BlockInfo::try_from(value).expect("bug: supported consensus value should project");
    let v: Value = serde_json::to_value(&info).unwrap();
    assert_eq!(
        v["blockHash"],
        serde_json::to_value(expected_block_hash).unwrap()
    );
    assert_eq!(v["protocol"], 1);
    assert_eq!(v["networkHash"], format!("0x{}", "01".repeat(32)));
    assert_eq!(v["configHash"], format!("0x{}", "08".repeat(32)));
    assert_eq!(v["proposer"], format!("0x{}", "02".repeat(32)));
    assert_eq!(v["blockTimepoint"], "123");
    assert_eq!(v["blockHeight"], "99");
    assert_eq!(v["txsHash"], format!("0x{}", "03".repeat(32)));
    assert_eq!(v["storageHash"], format!("0x{}", "04".repeat(32)));
    assert_eq!(v["eventsHash"], format!("0x{}", "05".repeat(32)));
    assert_eq!(v["proposalTxsHash"], format!("0x{}", "06".repeat(32)));
    assert_eq!(v["prevBlockHash"], format!("0x{}", "07".repeat(32)));
    assert_eq!(v["upgrades"], serde_json::json!(["0xaabb"]));
    // camelCase
    assert!(v.get("network_hash").is_none());
    assert!(v.get("block_height").is_none());
    assert!(v.get("txs_hash").is_none());
}

#[test]
fn consensus_value_hash_has_stable_golden_vector() {
    let hash = consensus_value_hash(&test_consensus_value())
        .expect("bug: valid consensus value should hash");
    assert_eq!(
        serde_json::to_value(hash).expect("bug: hash should serialize"),
        "0x77027501d110d5b4b92b76f64f3953f3f7a41f1280e31ae7de433e3f9677f9b0"
    );
}

#[test]
fn consensus_value_hash_and_projection_return_encoding_errors() {
    let mut value = test_consensus_value();
    value.upgrades = VecM(vec![BytesM(vec![0; 129])]);

    assert!(matches!(
        consensus_value_hash(&value),
        Err(xdrs::WriteError::InvalidArrayLength)
    ));
    assert!(matches!(
        BlockInfo::try_from(value),
        Err(BlockProjectionError::ConsensusValueEncoding {
            source: xdrs::WriteError::InvalidArrayLength
        })
    ));
}

#[test]
fn storage_item_wire_format_uses_camel_case_variant_fields() {
    let deployed = StorageItemInfo::DeployedContract {
        hash: zero_hash(),
        size: 7,
        contract_type: 3,
    };
    let deployed_json: Value = serde_json::to_value(&deployed).unwrap();
    assert_eq!(deployed_json["kind"], "deployedContract");
    assert_eq!(deployed_json["contractType"], 3);
    assert!(deployed_json.get("contract_type").is_none());

    let unknown = StorageItemInfo::Unknown { type_id: 9 };
    let unknown_json: Value = serde_json::to_value(&unknown).unwrap();
    assert_eq!(unknown_json["kind"], "unknown");
    assert_eq!(unknown_json["typeId"], 9);
    assert!(unknown_json.get("type_id").is_none());
}
