use groundhog_json_rpc_types::BlockHeight;
use groundhog_json_rpc_types::dev::{
    FrameOutputInfo, InvocationInfo, StorageDeltaInfo, SyscallInfo, TraceInfo, TransactionDetail,
};
use groundhog_json_rpc_types::events::EmittedEvent;
use groundhog_json_rpc_types::hex::{Hex0x, HexBytes, Topic};
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;
use serde_json::Value;

#[test]
fn transaction_detail_serializes_hex_and_camel_case() {
    let address = groundhog_xdr::types::ContractAddress::from([0x11; 32]);
    let detail = TransactionDetail {
        receipt: TransactionReceipt {
            tx_hash: TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x22; 32]))),
            height: BlockHeight::from(7),
            status: ExecStatus::Ok,
            events: vec![EmittedEvent {
                contract_address: Hex0x(address),
                topics: vec![Topic([0x33; 32])],
                data: HexBytes(vec![0x44]),
            }],
            return_data: Some(HexBytes(vec![0x55])),
            abort_data: None,
            gas_used: 12,
        },
        gas_consumed: 12,
        gas_limit: Some(99),
        storage_deltas: vec![StorageDeltaInfo {
            address: Hex0x(address),
            key: HexBytes(vec![0x66]),
            kind: "bytestringSet".into(),
            summary: "BytestringSet -> 0x77".into(),
            flags: 0,
            delta_xdr: HexBytes(vec![0x77]),
        }],
        trace: Some(TraceInfo {
            invocation: InvocationInfo {
                address: Hex0x(address),
                entrypoint: HexBytes(vec![0x88; 8]),
                gas_limit: Some(99),
                calldata: HexBytes(vec![0x99]),
            },
            system_calls: vec![SyscallInfo {
                name: "logEvent".into(),
                number: 23,
                gas_remaining_at_start: 100,
                gas_remaining_at_end: 90,
                status: groundhog_json_rpc_types::dev::SyscallStatusInfo {
                    code: 0,
                    label: "SyscallErrorCode::kSyscallErrorCodeNone".into(),
                },
                body_xdr: HexBytes(vec![0xaa]),
                child_trace: None,
            }],
            storage_writes: Vec::new(),
            status: FrameOutputInfo::Return {
                buffer: HexBytes(vec![0xbb]),
            },
        }),
    };

    let value: Value = serde_json::to_value(&detail).expect("bug: detail should serialize");
    assert_eq!(value["gasConsumed"], 12);
    assert_eq!(value["gasLimit"], 99);
    assert_eq!(value["storageDeltas"][0]["kind"], "bytestringSet");
    assert_eq!(value["storageDeltas"][0]["key"], "0x66");
    assert_eq!(
        value["trace"]["invocation"]["entrypoint"],
        "0x8888888888888888"
    );
    assert_eq!(value["trace"]["systemCalls"][0]["bodyXdr"], "0xaa");
    assert_eq!(value["trace"]["status"]["result"], "return");
    assert_eq!(value["trace"]["status"]["buffer"], "0xbb");
    assert!(value.get("gas_consumed").is_none());
    assert!(value["trace"].get("system_calls").is_none());
}
