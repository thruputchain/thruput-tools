use super::*;
use serde_json::Value;
use thruput_json_rpc_client::HttpClientBuilder;
use thruput_json_rpc_types::{BlockHeight, base58::Base58};
use thruput_xdr::types::SHA256Hash;

fn hash() -> TxHash {
    TxHash(Base58(SHA256Hash::from([7; 32])))
}
fn receipt_value(execution: &Value) -> Value {
    json!({"transactionHash":hash(),"blockHeight":"9","execution":execution,"events":[{"address":Base58(thruput_xdr::types::ContractAddress::from([3;32])),"topics":[format!("0x{}", "01".repeat(32))],"data":"0x0203"}],"gasUsed":"123"})
}
async fn server(
    method: &'static str,
    result: Value,
) -> (HttpClient, jsonrpsee::server::ServerHandle) {
    let server = jsonrpsee::server::ServerBuilder::default()
        .build("127.0.0.1:0")
        .await
        .unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", server.local_addr().unwrap()))
        .unwrap();
    let mut module = jsonrpsee::RpcModule::new(());
    module
        .register_method(method, move |params, (), _| {
            let input: Value = params.parse().unwrap();
            assert!(input.is_object(), "native params must be named");
            if method == "simulateInvocation" {
                assert!(input.get("address").is_some());
                assert!(input.get("contract").is_none());
                assert_eq!(input["minStateBlockHeight"], "5");
            }
            if method == "getEvents" {
                assert_eq!(
                    input["filters"][0]["addresses"],
                    json!([Base58(thruput_xdr::types::ContractAddress::from([3; 32]))])
                );
                assert_eq!(
                    input["filters"][0]["topics"],
                    json!({"topic0":[],"topic1":[],"topic2":[]})
                );
                assert_eq!(
                    input["start"],
                    json!({"order":"ascending","anchor":{"kind":"atBeginning"}})
                );
                assert!(input.get("cursor").is_none());
            }
            if method == "getTransactionReceipt" {
                assert_eq!(input, json!({"transactionHash":hash()}));
            }
            result.clone()
        })
        .unwrap();
    (client, server.start(module))
}

#[tokio::test]
async fn receipt_projection_preserves_execution_events_gas_and_height() {
    for (execution, status, returned, aborted) in [
        (
            json!({"result":"return","returnData":"0x0102"}),
            "ok",
            Some(vec![1, 2]),
            None,
        ),
        (
            json!({"result":"abort","abortData":"0x0304"}),
            "reverted",
            None,
            Some(vec![3, 4]),
        ),
        (json!({"result":"fault"}), "deterministicError", None, None),
        (
            json!({"result":"nonDeterministicError"}),
            "nonDeterministicError",
            None,
            None,
        ),
        (
            json!({"result":"failedGasSponsorship"}),
            "failedGasSponsorship",
            None,
            None,
        ),
    ] {
        let (client, handle) = server("getTransactionReceipt", receipt_value(&execution)).await;
        let value = receipt(&client, hash()).await.unwrap().unwrap();
        assert_eq!(value.tx_hash, hash());
        assert_eq!(*value.height, 9);
        assert_eq!(json!(value.status)["result"], status);
        assert_eq!(value.return_data.map(|data| data.0), returned);
        assert_eq!(value.abort_data.map(|data| data.0), aborted);
        assert_eq!(value.gas_used, 123);
        assert_eq!(
            value.events[0].contract_address,
            Base58(thruput_xdr::types::ContractAddress::from([3; 32]))
        );
        assert_eq!(value.events[0].data.0, vec![2, 3]);
        assert_eq!(value.events[0].topics[0].0, [1; 32]);
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn invalid_receipts_keep_raw_evidence() {
    let mut mismatch = receipt_value(&json!({"result":"return","returnData":"0x"}));
    mismatch["transactionHash"] = json!(TxHash(Base58(SHA256Hash::from([8; 32]))));
    let mut negative = receipt_value(&json!({"result":"return","returnData":"0x"}));
    negative["blockHeight"] = json!("-1");
    for raw in [
        mismatch,
        negative,
        receipt_value(&json!({"result":"future","proof":{"retained":true}})),
        receipt_value(&json!({"result":"return"})),
    ] {
        let (client, handle) = server("getTransactionReceipt", raw.clone()).await;
        let error = receipt(&client, hash()).await.unwrap_err();
        assert_eq!(error.evidence()["result"], raw);
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn simulation_uses_address_and_rejects_stale_state() {
    let (client,handle) = server("simulateInvocation",json!({"stateBlockHeight":"4","output":{"result":"return","returnData":"0x"},"events":[],"gasUsed":"1"})).await;
    let request: ViewRequest = serde_json::from_value(json!({"contract":Base58(thruput_xdr::types::ContractAddress::from([3;32])),"entrypoint":"0x0000000000000000","calldata":"0x","minHeight":"5"})).unwrap();
    let error = simulate(&client, request).await.unwrap_err();
    assert_eq!(error.evidence()["result"]["stateBlockHeight"], "4");
    handle.stop().unwrap();
}

#[tokio::test]
async fn event_cursor_and_watermarks_project_without_reencoding() {
    let (client,handle) = server("getEvents",json!({"events":[],"nextCursor":"0x0012ff","oldestIndexedBlockHeight":"2","latestIndexedBlockHeight":"9"})).await;
    let query = EventQuery {
        filters: serde_json::from_value(json!([{"contractAddresses":[Base58(thruput_xdr::types::ContractAddress::from([3;32]))],"topics":{"topic0":[],"topic1":[],"topic2":[]}}])).unwrap(),
        order: IterationOrder::Ascending,
        anchor: BlockAnchor::AtBeginning,
        cursor: PaginationCursor(vec![]),
        limit: 5,
    };
    let result = events(&client, query).await.unwrap();
    assert_eq!(result.cursor.0, vec![0, 18, 255]);
    assert_eq!(result.oldest_block, BlockHeight::from(2));
    assert_eq!(result.latest_block, BlockHeight::from(9));
    handle.stop().unwrap();
}

#[tokio::test]
async fn storage_projection_preserves_values_and_unknown_data_is_not_absence() {
    let address = Base58(thruput_xdr::types::ContractAddress::from([3; 32]));
    for item in [
        Value::Null,
        json!({"kind":"nonnegativeInt","value":"123"}),
        json!({"kind":"bytestring","bytes":"0x1234"}),
    ] {
        let (client, handle) =
            server("getStorage", json!({"stateBlockHeight":"4","item":item})).await;
        let result = storage(&client, address, HexBytes(vec![])).await.unwrap();
        assert_eq!(*result.block_height, 4);
        assert_eq!(json!(result.item), item);
        handle.stop().unwrap();
    }
    let raw = json!({"stateBlockHeight":"4","item":{"kind":"future","evidence":"retained"}});
    let (client, handle) = server("getStorage", raw.clone()).await;
    let error = storage(&client, address, HexBytes(vec![]))
        .await
        .unwrap_err();
    assert_eq!(error.evidence()["result"], raw);
    handle.stop().unwrap();
}

#[tokio::test]
async fn submission_rejects_mismatched_acknowledgement_hash() {
    use thruput_signing::{
        BuildCallTransaction, PubkeyAccount, SignTransactionParams, build_call_transaction,
        derive_dev_account_signer, sign_and_encode_transaction_hex,
    };
    let signer = derive_dev_account_signer("ack-test", 0);
    let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: account.address(),
        entrypoint: thruput_xdr::contract::ContractEntrypointID::from([0; 8]),
        calldata: vec![],
        gas_limit: 100,
        block_expiry_height: 10,
        nonce: 0,
        auth_claim: account.auth_claim(),
    });
    let encoded = sign_and_encode_transaction_hex(
        tx,
        &signer,
        SignTransactionParams::default(),
        &thruput_xdr::types::NetworkHash::new([0; 32].into()),
    )
    .unwrap();
    let (client, handle) = server(
        "sendTransaction",
        json!({"transactionHash":hash(),"admission":"accepted"}),
    )
    .await;
    let error = send_transaction(&client, encoded).await.unwrap_err();
    assert_eq!(error.evidence()["result"]["transactionHash"], json!(hash()));
    handle.stop().unwrap();
}
