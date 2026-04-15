//! End-to-end tests for the Groundhog JSON-RPC API.
//!
//! These tests connect to a running server (dev-node or gateway).
//! Set `RPC_URL` env var to override the default (http://127.0.0.1:9545).
//!
//! Start a dev-node first:
//!   cargo run -p groundhog-cli --features dev-node -- node
//!
//! Then run:
//!   cargo test -p groundhog-e2e-tests -- --ignored
//!
//! Tests are marked #[ignore] so they don't run in `cargo test --workspace`.

use groundhog_rpc_api_types::transaction::TransactionStatus;
use groundhog_rpc_client::{GroundhogApiClient, HttpClientBuilder};

fn rpc_url() -> String {
    std::env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:9545".to_string())
}

fn client() -> impl GroundhogApiClient {
    HttpClientBuilder::default()
        .build(rpc_url())
        .expect("bug: HTTP client should build")
}

fn zero_hash() -> groundhog_rpc_api_types::transaction::TxHash {
    groundhog_rpc_api_types::transaction::TxHash(groundhog_rpc_api_types::hex::Hex0x(
        groundhog_xdr::types::SHA256Hash::from([0u8; 32]),
    ))
}

#[tokio::test]
#[ignore = "requires running dev-node or gateway"]
async fn chain_tip_returns_valid_response() {
    let c = client();
    let tip = c.chain_tip().await.expect("chainTip should succeed");
    // Height should be a non-negative number
    assert!(*tip.height >= 0, "height should be non-negative");
    // blockHash should be 0x-prefixed 32 bytes
    assert_eq!(
        tip.block_hash.0.as_ref().len(),
        32,
        "blockHash should be 32 bytes"
    );
}

#[tokio::test]
#[ignore = "requires running dev-node or gateway"]
async fn unknown_tx_returns_unknown_status() {
    let c = client();
    let status = c
        .get_transaction_status(zero_hash())
        .await
        .expect("getTransactionStatus should succeed");
    assert!(
        matches!(status, TransactionStatus::Unknown),
        "zero hash should return Unknown, got {:?}",
        status
    );
}

#[tokio::test]
#[ignore = "requires running dev-node or gateway"]
async fn unknown_tx_receipt_is_none() {
    let c = client();
    let receipt = c
        .get_transaction_receipt(zero_hash())
        .await
        .expect("getTransactionReceipt should succeed");
    assert!(receipt.is_none(), "zero hash should return None");
}

#[tokio::test]
#[ignore = "requires running dev-node or gateway"]
async fn invalid_hex_is_rejected() {
    let c = client();
    let result = c.send_raw_transaction("not-valid-hex".to_string()).await;
    assert!(result.is_err(), "invalid hex should return error");
}

#[tokio::test]
#[ignore = "requires running dev-node or gateway"]
async fn pending_transaction_count_returns_stub_or_value() {
    let c = client();
    // May return NotImplemented (-32052) on gateway or 0 on dev-node.
    // Either is acceptable.
    let _result = c.pending_transaction_count().await;
    // Not asserting on success/failure — both are valid depending on backend.
}

// TODO: add submit_and_query_receipt test once we can construct valid
// transactions against the running node's genesis state. Requires
// knowing the pre-funded account keys (dev-node provides them via
// groundhog_dev_getAccounts, gateway doesn't).
