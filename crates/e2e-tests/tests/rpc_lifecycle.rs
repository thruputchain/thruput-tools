//! End-to-end tests for the Groundhog JSON-RPC API.
//!
//! These tests connect to a running RPC server (hogd-dev local node or gateway).
//! Set `RPC_URL` env var to override the default (http://127.0.0.1:9545).
//!
//! Start a local node first:
//!   cargo run -p groundhog-cli --features dev-node -- node --hogd-dev-binary /path/to/hogd-dev
//!
//! Then run:
//!   cargo test -p groundhog-e2e-tests -- --ignored
//!
//! Tests are marked #[ignore] so they don't run in `cargo test --workspace`.

use groundhog_rpc_api_types::events::{
    BlockAnchor, EventFilter, EventQuery, EventTopicFilter, IterationOrder,
};
use groundhog_rpc_api_types::transaction::TransactionStatus;
use groundhog_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use xdr_common::{BytesM, VecM};

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
#[ignore = "requires running hogd-dev local node or gateway"]
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
#[ignore = "requires running hogd-dev local node or gateway"]
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
#[ignore = "requires running hogd-dev local node or gateway"]
async fn unknown_tx_receipt_is_none() {
    let c = client();
    let receipt = c
        .get_transaction_receipt(zero_hash())
        .await
        .expect("getTransactionReceipt should succeed");
    assert!(receipt.is_none(), "zero hash should return None");
}

#[tokio::test]
#[ignore = "requires running hogd-dev local node or gateway"]
async fn invalid_hex_is_rejected() {
    let c = client();
    let result = c.send_raw_transaction("not-valid-hex".to_string()).await;
    assert!(result.is_err(), "invalid hex should return error");
}

#[tokio::test]
#[ignore = "requires running hogd-dev local node or gateway"]
async fn pending_transaction_count_returns_stub_or_value() {
    let c = client();
    // May return NotImplemented (-32052) until a scheduler size RPC exists.
    let _result = c.pending_transaction_count().await;
    // Not asserting on success/failure while backend capabilities are evolving.
}

/// `getEvents` query that fetches every event emitted by `addr`,
/// with no topic filter applied.
fn events_for_address(
    addr: [u8; 32],
    anchor: BlockAnchor,
    order: IterationOrder,
    limit: u32,
) -> EventQuery {
    EventQuery {
        filters: VecM(vec![EventFilter {
            contract_addresses: VecM(vec![groundhog_xdr::types::ContractAddress::from(addr)]),
            topics: EventTopicFilter {
                topic0: VecM(vec![]),
                topic1: VecM(vec![]),
                topic2: VecM(vec![]),
            },
        }]),
        order,
        anchor,
        cursor: BytesM(vec![]),
        limit,
    }
}

#[tokio::test]
#[ignore = "requires running RPC server with populated indexer"]
async fn get_events_returns_well_formed_page() {
    let c = client();
    let query = events_for_address(
        [0u8; 32],
        BlockAnchor::AnchorAtEnd,
        IterationOrder::IterationOrderDescending,
        10,
    );
    let page = c.get_events(query).await.expect("getEvents should succeed");

    assert!(
        page.events.len() <= 10,
        "page must respect requested limit, got {}",
        page.events.len()
    );
    assert!(
        *page.oldest_block <= *page.latest_block,
        "oldestBlock {} must not exceed latestBlock {}",
        *page.oldest_block,
        *page.latest_block,
    );
    for ev in &page.events {
        assert!(
            ev.event.topics.len() <= 3,
            "events expose at most 3 topics, got {}",
            ev.event.topics.len()
        );
        assert!(
            *ev.block_height >= *page.oldest_block && *ev.block_height <= *page.latest_block,
            "event block_height {} outside [oldest={}, latest={}]",
            *ev.block_height,
            *page.oldest_block,
            *page.latest_block
        );
    }
}

/// Walk pagination cursor with `limit=1` and confirm the cursor either
/// advances or terminates (empty).  Pass criterion: at least two
/// non-overlapping pages OR an empty stream — never a stuck cursor that
/// returns the same page repeatedly.
#[tokio::test]
#[ignore = "requires running RPC server with populated indexer"]
async fn get_events_cursor_walks_or_terminates() {
    let c = client();
    let mut query = events_for_address(
        [0u8; 32],
        BlockAnchor::AnchorAtEnd,
        IterationOrder::IterationOrderDescending,
        1,
    );

    let first = c.get_events(query.clone()).await.expect("first page");
    assert!(
        first.events.len() <= 1,
        "limit=1 must cap first page, got {}",
        first.events.len()
    );

    if first.cursor.0.is_empty() {
        // Stream is exhausted in one page — either no events at all, or
        // exactly one event matched.  Nothing to walk; that is itself a
        // valid `getEvents` outcome and the test passes.
        return;
    }

    query.cursor = BytesM(first.cursor.0.clone());
    let second = c.get_events(query).await.expect("second page");

    if let (Some(a), Some(b)) = (first.events.first(), second.events.first()) {
        assert!(
            a.tx_hash != b.tx_hash || a.sequence_number != b.sequence_number,
            "cursor must advance: page1 and page2 returned the same event \
             (tx={:?} seq={})",
            a.tx_hash,
            a.sequence_number,
        );
    }
}

// TODO: add submit_and_query_receipt test once we can construct valid
// transactions against the running node's genesis state. Requires
// the local node to expose pre-funded account keys via
// groundhog_dev_getAccounts.
