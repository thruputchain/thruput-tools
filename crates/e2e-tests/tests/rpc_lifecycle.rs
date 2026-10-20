//! End-to-end tests for the native Thruput JSON-RPC API.
//!
//! These tests connect to a running RPC server (hog-devnode local node or gateway).
//! Set `RPC_URL` env var to override the default (http://127.0.0.1:9545).
//!
//! Start a local node first:
//!   cargo run -p thruput-cli -- node --hog-devnode-binary /path/to/hog-devnode
//!
//! Then run:
//!   cargo test -p thruput-e2e-tests -- --ignored
//!
//! Tests are marked #[ignore] so they don't run in `cargo test --workspace`.

use jsonrpsee::core::client::ClientT;
use thruput_json_rpc_client::{
    HttpClient, HttpClientBuilder,
    native::{NativeClient, NativeClientError},
};
use thruput_json_rpc_types::{
    ContractAddress,
    base58::Base58,
    hex::HexBytes,
    requests::{
        DescendingAnchor, EmptyRequest, EventFilter, EventQuery, HistoryStart,
        SendTransactionRequest, StorageQuery, TransactionQuery,
    },
    responses::{DecodedResponse, TransactionStatus},
};

fn known<T: std::fmt::Debug>(response: Result<DecodedResponse<T>, NativeClientError>) -> T {
    match response.expect("native RPC response") {
        DecodedResponse::Known(value) => value,
        other => panic!("unsupported native response: {other:?}"),
    }
}

fn rpc_url() -> String {
    std::env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:9545".to_string())
}

fn client() -> NativeClient<HttpClient> {
    NativeClient::new(
        HttpClientBuilder::default()
            .build(rpc_url())
            .expect("bug: HTTP client should build"),
    )
}

fn zero_hash() -> thruput_json_rpc_types::transaction::TxHash {
    thruput_json_rpc_types::transaction::TxHash(thruput_json_rpc_types::base58::Base58(
        thruput_xdr::types::SHA256Hash::from([0u8; 32]),
    ))
}

fn zero_address() -> ContractAddress {
    Base58(thruput_xdr::types::ContractAddress::from([0u8; 32]))
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn chain_tip_returns_valid_response() {
    let c = client();
    let tip = known(c.get_chain_tip(EmptyRequest {}).await);
    // Height should be a non-negative number
    assert!(*tip.block_height >= 0, "height should be non-negative");
    // Typed block hashes decode to 32 bytes.
    assert_eq!(
        tip.block_hash.0.as_ref().len(),
        32,
        "blockHash should be 32 bytes"
    );
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn unknown_tx_returns_unknown_status() {
    let c = client();
    let status = known(
        c.get_transaction_status(TransactionQuery {
            transaction_hash: zero_hash(),
        })
        .await,
    );
    assert!(
        matches!(status, TransactionStatus::Unknown {}),
        "zero hash should return Unknown, got {:?}",
        status
    );
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn unknown_tx_receipt_is_none() {
    let c = client();
    let receipt = known(
        c.get_transaction_receipt(TransactionQuery {
            transaction_hash: zero_hash(),
        })
        .await,
    );
    assert!(receipt.is_none(), "zero hash should return None");
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn get_storage_zero_probe_returns_well_formed_read() {
    let c = client();
    let read = known(
        c.get_storage(StorageQuery {
            address: zero_address(),
            key: HexBytes(vec![0; 32]),
        })
        .await,
    );
    assert!(*read.state_block_height >= 0);
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn get_storage_short_key_is_rejected() {
    let c = client();
    let err = c
        .get_storage(StorageQuery {
            address: zero_address(),
            key: HexBytes(vec![0; 16]),
        })
        .await
        .expect_err("16-byte key must be rejected");
    match err {
        NativeClientError::Application { source, data } => {
            assert_eq!(
                source.code(),
                jsonrpsee::types::error::INVALID_PARAMS_CODE,
                "{data:?}"
            );
            assert!(
                matches!(*data, thruput_json_rpc_types::error::native::DecodedError::Known { ref data, .. } if matches!(**data, thruput_json_rpc_types::error::native::ErrorData::InvalidParams { .. })),
                "{data:?}"
            );
        }
        other => panic!("expected invalid params, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn invalid_hex_is_rejected() {
    let c = client();
    let mut params = jsonrpsee::core::params::ObjectParams::new();
    params.insert("signedTransaction", "not-valid-hex").unwrap();
    let result: Result<serde_json::Value, _> =
        c.into_inner().request("sendTransaction", params).await;
    match result.expect_err("invalid hex should return error") {
        jsonrpsee::core::ClientError::Call(error) => assert_eq!(
            error.code(),
            jsonrpsee::types::error::INVALID_PARAMS_CODE,
            "{error:?}"
        ),
        other => panic!("expected invalid params, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn discovery_advertises_native_core_methods() {
    let version = known(client().get_version_info(EmptyRequest {}).await);
    assert_eq!(version.wire_format.split('.').next(), Some("14"));
    for method in [
        "getVersionInfo",
        "getNetworkInfo",
        "getChainTip",
        "sendTransaction",
        "getTransactionStatus",
        "getTransactionReceipt",
        "getEvents",
    ] {
        assert!(
            version.supported_methods.iter().any(|name| name == method),
            "missing {method}"
        );
    }
    assert!(
        version
            .supported_methods
            .iter()
            .all(|name| !name.starts_with("groundhog_"))
    );
}

/// `getEvents` query that fetches every event emitted by `addr`,
/// with no topic filter applied.
fn events_for_address(addr: [u8; 32], limit: u32) -> EventQuery {
    EventQuery {
        filters: vec![EventFilter {
            addresses: vec![Base58(thruput_xdr::types::ContractAddress::from(addr))],
            topics: None,
        }],
        start: Some(HistoryStart::Descending {
            anchor: DescendingAnchor::AtEnd {},
        }),
        cursor: None,
        limit: Some(limit),
    }
}

#[tokio::test]
#[ignore = "requires running RPC server with populated indexer"]
async fn get_events_returns_well_formed_page() {
    let c = client();
    let query = events_for_address([0; 32], 10);
    let page = known(c.get_events(query).await);

    assert!(
        page.events.len() <= 10,
        "page must respect requested limit, got {}",
        page.events.len()
    );
    assert!(
        *page.oldest_indexed_block_height <= *page.latest_indexed_block_height,
        "oldestBlock {} must not exceed latestBlock {}",
        *page.oldest_indexed_block_height,
        *page.latest_indexed_block_height,
    );
    for ev in &page.events {
        assert!(
            ev.event.topics.len() <= 3,
            "events expose at most 3 topics, got {}",
            ev.event.topics.len()
        );
        assert!(
            *ev.block_height >= *page.oldest_indexed_block_height
                && *ev.block_height <= *page.latest_indexed_block_height,
            "event block_height {} outside [oldest={}, latest={}]",
            *ev.block_height,
            *page.oldest_indexed_block_height,
            *page.latest_indexed_block_height
        );
    }
}

/// Walk pagination cursor with `limit=1` and confirm the cursor either
/// advances or terminates (null).  Pass criterion: at least two
/// non-overlapping pages OR an empty stream — never a stuck cursor that
/// returns the same page repeatedly.
#[tokio::test]
#[ignore = "requires running RPC server with populated indexer"]
async fn get_events_cursor_walks_or_terminates() {
    let c = client();
    let mut query = events_for_address([0; 32], 1);

    let first = known(c.get_events(query.clone()).await);
    assert!(
        first.events.len() <= 1,
        "limit=1 must cap first page, got {}",
        first.events.len()
    );

    if first.next_cursor.is_none() {
        // Stream is exhausted in one page — either no events at all, or
        // exactly one event matched.  Nothing to walk; that is itself a
        // valid `getEvents` outcome and the test passes.
        return;
    }

    query.start = None;
    query.cursor = first.next_cursor.clone();
    let second = known(c.get_events(query).await);
    assert!(
        second.next_cursor.is_none() || second.next_cursor != first.next_cursor,
        "cursor must advance even on empty pages"
    );

    if let (Some(a), Some(b)) = (first.events.first(), second.events.first()) {
        assert!(
            a.transaction_hash != b.transaction_hash || a.sequence_number != b.sequence_number,
            "cursor must advance: page1 and page2 returned the same event \
             (tx={:?} seq={})",
            a.transaction_hash,
            a.sequence_number,
        );
    }
}

#[tokio::test]
#[ignore = "requires running hog-devnode local node or gateway"]
async fn invalid_signature_returns_transaction_rejection() {
    use thruput_signing::{
        BuildCallTransaction, LocalSigner, PubkeyAccount, SignTransactionParams, Signer,
        build_call_transaction, encode_signed_transaction_xdr, sign_transaction,
    };
    use thruput_xdr::{
        contract::ContractEntrypointID,
        types::{NetworkHash, Signature},
    };

    let c = client();
    let network = known(c.get_network_info(EmptyRequest {}).await);
    let signer = LocalSigner::from_secret_key([0x42; 32]);
    let account = PubkeyAccount::new(signer.public_key(), 0);
    let tx = build_call_transaction(BuildCallTransaction {
        invoked_address: account.address(),
        entrypoint: ContractEntrypointID::from([0; 8]),
        calldata: vec![],
        gas_limit: 1_000_000,
        block_expiry_height: i64::MAX,
        nonce: 1,
        auth_claim: account.auth_claim(),
    });
    let mut signed = sign_transaction(
        tx,
        &signer,
        SignTransactionParams::default(),
        &NetworkHash::new(network.network_hash.0),
    )
    .unwrap();
    signed.signatures.0[0].signature = Signature::Ed25519 {
        ed25519: [0; 64].into(),
    };
    let hash = thruput_json_rpc_types::transaction::TxHash(Base58(
        thruput_signing::transaction_hash(&signed.tx).unwrap(),
    ));
    let encoded = encode_signed_transaction_xdr(&signed).unwrap();
    let error = c
        .send_transaction(SendTransactionRequest {
            signed_transaction: HexBytes(encoded),
        })
        .await
        .expect_err("an invalid signature must not return a transaction hash");
    match error {
        NativeClientError::Application { source, data } => {
            use thruput_json_rpc_types::error::native::{
                AttemptAdmission, DecodedError, ErrorData, Reason, TransactionRejectedReason,
            };
            assert_eq!(
                source.code(),
                thruput_json_rpc_types::error::native::codes::TRANSACTION_REJECTED,
                "{data:?}"
            );
            let DecodedError::Known { data, raw } = *data else {
                panic!("unclassified rejection: {data:?}")
            };
            let ErrorData::TransactionRejected {
                admission: AttemptAdmission::Rejected,
                reason,
                transaction_hash,
                ..
            } = *data
            else {
                panic!("unexpected rejection evidence: {raw}")
            };
            assert!(
                matches!(
                    reason,
                    None | Some(Reason::Known(
                        TransactionRejectedReason::AuthenticationRejected
                    ))
                ),
                "{raw}"
            );
            if let Some(returned_hash) = transaction_hash {
                assert_eq!(returned_hash, hash, "{raw}");
            }
        }
        other => panic!("expected a typed transaction rejection, got {other:?}"),
    }
}
