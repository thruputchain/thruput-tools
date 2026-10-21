//! External callers can resume read-only observation from a retained transaction hash.
use jsonrpsee::{RpcModule, http_client::HttpClientBuilder, server::ServerBuilder};
use serde_json::json;
use thruput_client::{NativeClient, ReceiptPollConfig, SubmittedCall};
use thruput_json_rpc_types::{NetworkHash, transaction::TxHash};

#[tokio::test]
async fn retained_hash_resumes_receipt_wait_without_submission() {
    let hash: TxHash = serde_json::from_value(json!(thruput_encoding::encode(&[7; 32]))).unwrap();
    let network: NetworkHash =
        serde_json::from_value(json!(thruput_encoding::encode(&[8; 32]))).unwrap();
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut methods = RpcModule::new(());
    methods.register_method("getTransactionReceipt", move |params, (), _| {
        assert_eq!(params.parse::<serde_json::Value>().unwrap(), json!({"transactionHash":hash}));
        json!({"transactionHash":hash,"blockHeight":"12","execution":{"result":"return","returnData":"0x"},"events":[],"gasUsed":"3"})
    }).unwrap();
    // No submission method exists: this flow can only read the retained hash.
    let handle = server.start(methods);
    let client = NativeClient::new(HttpClientBuilder::default().build(url).unwrap());
    let receipt = SubmittedCall::new(hash)
        .wait(
            &client,
            network,
            ReceiptPollConfig::default(),
            std::future::pending(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.transaction_hash, hash);
    assert_eq!(*receipt.block_height, 12);
    handle.stop().unwrap();
}
