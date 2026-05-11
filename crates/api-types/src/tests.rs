#[cfg(test)]
mod schema_generation {
    use schemars::schema_for;

    use crate::block::{BlockInfo, ChainTip};
    use crate::dev::{
        DevAccount, FrameOutputInfo, InvocationInfo, StorageDeltaInfo, SyscallInfo,
        SyscallStatusInfo, TraceInfo, TransactionDetail,
    };
    use crate::events::{
        EmittedEvent, EventFilter, EventPage, EventQuery, EventTopicFilter, IdentifiedEvent,
    };
    use crate::receipt::{ExecStatus, TransactionReceipt};
    use crate::state::{StorageItemInfo, StorageItemRead};
    use crate::transaction::{TransactionStatus, TxHash};

    #[test]
    fn schema_generation_succeeds() {
        // Core RPC types
        let _ = schema_for!(ChainTip);
        let _ = schema_for!(BlockInfo);
        let _ = schema_for!(TxHash);
        let _ = schema_for!(TransactionStatus);
        let _ = schema_for!(TransactionReceipt);
        let _ = schema_for!(ExecStatus);

        // Event types
        let _ = schema_for!(EmittedEvent);
        let _ = schema_for!(IdentifiedEvent);
        let _ = schema_for!(EventPage);
        let _ = schema_for!(EventTopicFilter);
        let _ = schema_for!(EventFilter);
        let _ = schema_for!(EventQuery);

        // State types
        let _ = schema_for!(StorageItemInfo);
        let _ = schema_for!(StorageItemRead);

        // Dev types
        let _ = schema_for!(DevAccount);
        let _ = schema_for!(TransactionDetail);
        let _ = schema_for!(StorageDeltaInfo);
        let _ = schema_for!(TraceInfo);
        let _ = schema_for!(InvocationInfo);
        let _ = schema_for!(SyscallInfo);
        let _ = schema_for!(SyscallStatusInfo);
        let _ = schema_for!(FrameOutputInfo);
    }
}

#[cfg(test)]
mod wire_format {
    use serde_json::Value;

    use crate::block::{BlockInfo, ChainTip};
    use crate::events::{
        BlockAnchor, EmittedEvent, EventFilter, EventQuery, EventTopicFilter, IdentifiedEvent,
        IterationOrder, PaginationCursor,
    };
    use crate::hex::{Hex0x, HexBytes, Topic};
    use crate::receipt::{ExecStatus, TransactionReceipt};
    use crate::state::StorageItemInfo;
    use crate::transaction::{TransactionStatus, TxHash};
    use crate::{BlockHeader, BlockHeight, SHA256Hash};
    use xdr_common::VecM;

    fn zero_hash() -> SHA256Hash {
        Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32]))
    }

    fn zero_address() -> crate::ContractAddress {
        Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32]))
    }

    #[test]
    fn chain_tip_wire_format() {
        let tip = ChainTip {
            height: BlockHeight::from(42i64),
            block_hash: zero_hash(),
        };
        let v: Value = serde_json::to_value(&tip).unwrap();
        assert_eq!(v["height"], 42);
        assert!(v["blockHash"].as_str().unwrap().starts_with("0x"));
        assert_eq!(v["blockHash"].as_str().unwrap().len(), 66); // 0x + 64
        // Verify camelCase
        assert!(v.get("block_hash").is_none());
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
        assert_eq!(v["height"], 100);
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
        };
        let v: Value = serde_json::to_value(&receipt).unwrap();
        assert!(v["txHash"].as_str().unwrap().starts_with("0x"));
        assert_eq!(v["height"], 50);
        assert_eq!(v["status"]["result"], "ok");
        assert!(v["events"].as_array().unwrap().is_empty());
        // camelCase
        assert!(v.get("tx_hash").is_none());
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
        assert_eq!(v["blockHeight"], 10);
        assert_eq!(v["sequenceNumber"], 0);
        assert!(v.get("tx_hash").is_none());
        assert!(v.get("block_hash").is_none());
        assert!(v.get("sequence_number").is_none());
    }

    #[test]
    fn event_query_wire_format_uses_rpc_hex_projections() {
        let query = EventQuery {
            filters: VecM(vec![EventFilter {
                contract_addresses: VecM(vec![zero_address()]),
                topics: EventTopicFilter {
                    topic0: VecM(vec![Topic([0xab; 32])]),
                    topic1: VecM(vec![]),
                    topic2: VecM(vec![]),
                },
            }]),
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
    }

    #[test]
    fn block_info_wire_format() {
        let info = BlockInfo::from(BlockHeader {
            network_id: 1,
            block_height: BlockHeight::from(99i64),
            txset_hash: groundhog_xdr::types::SHA256Hash::from([0u8; 32]),
            db_hash: groundhog_xdr::types::DbHash::from([0u8; 2048]),
            events_hash: groundhog_xdr::types::SHA256Hash::from([0u8; 32]),
            prev_block_hash: groundhog_xdr::types::BlockHash::from(
                groundhog_xdr::types::SHA256Hash::from([0u8; 32]),
            ),
        });
        let v: Value = serde_json::to_value(&info).unwrap();
        assert_eq!(v["networkId"], 1);
        assert_eq!(v["blockHeight"], 99);
        assert!(v["txsetHash"].as_str().unwrap().starts_with("0x"));
        assert!(v["dbHash"].as_str().unwrap().starts_with("0x"));
        assert!(v["eventsHash"].as_str().unwrap().starts_with("0x"));
        assert!(v["prevBlockHash"].as_str().unwrap().starts_with("0x"));
        // camelCase
        assert!(v.get("network_id").is_none());
        assert!(v.get("block_height").is_none());
        assert!(v.get("txset_hash").is_none());
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
}
