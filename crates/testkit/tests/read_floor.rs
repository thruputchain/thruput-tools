//! Contract and storage reads use the highest block the handle has seen as
//! their freshness floor, and retry a server that cannot reach it yet.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use serde_json::{Value, json};
use thruput_json_rpc_types::{
    base58::Base58,
    error::native::codes,
    hex::HexBytes,
    requests::TransactionQuery,
    responses::{ReceiptExecution, TransactionReceipt},
    transaction::TxHash,
};
use thruput_testkit::{NetworkConfig, Scenario, TestkitError, xdr::types::ContractAddress};
use thruput_xdr::contract::ContractEntrypointID;

mod common;
use common::{native_runtime_document, serve_manifest_network_info};

/// Mock node state the tests steer and inspect.
#[derive(Default)]
struct Node {
    /// Recorded `simulateInvocation` params.
    reads: Mutex<Vec<Value>>,
    /// How many `simulateInvocation` calls to refuse first.
    refuse: Mutex<u32>,
    /// How long each refusal takes.
    refusal_delay: Mutex<Duration>,
    /// How many `getStorage` calls were made.
    storage_reads: Mutex<u32>,
    /// How many `getStorage` calls to answer from block 6 before block 7.
    storage_lag: Mutex<u32>,
}

/// A transaction whose receipt reports `height`: the hash's first byte.
fn tx_at(height: u8) -> TxHash {
    TxHash(Base58([height; 32].into()))
}

async fn mock_node(node: Arc<Node>) -> (String, jsonrpsee::server::ServerHandle) {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(node);
    serve_manifest_network_info(&mut module);
    module
        .register_method("dev_produceBlock", |_, _, _| {
            Ok::<_, ErrorObjectOwned>(
                json!({"blockHeight": "7", "blockTimepoint": "1700000000000000000"}),
            )
        })
        .unwrap();
    module
        .register_method("getTransactionReceipt", |params, _, _| {
            let hash = params.parse::<TransactionQuery>()?.transaction_hash;
            Ok::<_, ErrorObjectOwned>(Some(TransactionReceipt {
                transaction_hash: hash,
                block_height: i64::from(hash.0.0.into_inner()[0]).into(),
                execution: ReceiptExecution::Return {
                    return_data: HexBytes(vec![]),
                },
                events: vec![],
                gas_used: 1,
            }))
        })
        .unwrap();
    module
        .register_async_method("simulateInvocation", |params, node, _| async move {
            let params: Value = params.parse()?;
            node.reads.lock().unwrap().push(params);
            let refused = {
                let mut refuse = node.refuse.lock().unwrap();
                let refused = *refuse > 0;
                if refused {
                    *refuse -= 1;
                }
                refused
            };
            if refused {
                let delay = *node.refusal_delay.lock().unwrap();
                tokio::time::sleep(delay).await;
                return Err(ErrorObjectOwned::owned(
                    codes::STATE_HEIGHT_UNAVAILABLE,
                    "state height unavailable",
                    Some(json!({
                        "kind": "stateHeightUnavailable",
                        "requestedStateBlockHeight": "7",
                        "observedStateBlockHeight": "6",
                    })),
                ));
            }
            Ok(json!({
                "stateBlockHeight": "7",
                "output": {"result": "return", "returnData": "0x"},
                "events": [],
                "gasUsed": "1",
            }))
        })
        .unwrap();
    module
        .register_method("getStorage", |_, node, _| {
            *node.storage_reads.lock().unwrap() += 1;
            let mut lag = node.storage_lag.lock().unwrap();
            let height = if *lag > 0 {
                *lag -= 1;
                "6"
            } else {
                "7"
            };
            Ok::<_, ErrorObjectOwned>(json!({"stateBlockHeight": height, "item": null}))
        })
        .unwrap();
    (url, server.start(module))
}

async fn scenario(url: &str) -> (Scenario, tempfile::NamedTempFile) {
    let manifest = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        manifest.path(),
        serde_json::to_vec(&native_runtime_document()).unwrap(),
    )
    .unwrap();
    let scenario = Scenario::builder("read-floor")
        .accounts(1)
        .network(NetworkConfig::existing(url, manifest.path()))
        .build()
        .await
        .unwrap();
    (scenario, manifest)
}

async fn read(scenario: &Scenario) -> thruput_testkit::Result<()> {
    scenario
        .network()
        .view_call(
            ContractAddress::from([1; 32]),
            ContractEntrypointID::from([2; 8]),
            &[],
            None,
        )
        .await
        .map(|_| ())
}

#[tokio::test]
async fn a_read_after_a_produced_block_floors_at_its_height() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;

    read(&scenario).await.unwrap();
    scenario.network().produce_block().await.unwrap();
    read(&scenario).await.unwrap();

    let reads = node.reads.lock().unwrap();
    assert!(
        reads[0].get("minStateBlockHeight").is_none(),
        "no block seen yet"
    );
    assert_eq!(reads[1]["minStateBlockHeight"], "7");
    handle.stop().unwrap();
}

fn floors(node: &Node) -> Vec<Value> {
    node.reads
        .lock()
        .unwrap()
        .iter()
        .map(|read| read["minStateBlockHeight"].clone())
        .collect()
}

#[tokio::test]
async fn receipts_raise_the_floor_and_an_older_one_never_lowers_it() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;
    let network = scenario.network();
    let other = network.clone();

    network.receipt(tx_at(9)).await.unwrap();
    read(&scenario).await.unwrap();
    // An older receipt seen through another handle keeps the higher floor.
    other.receipt(tx_at(5)).await.unwrap();
    read(&scenario).await.unwrap();
    // The receipt wait raises it too.
    other.await_receipt(tx_at(11)).await.unwrap();
    read(&scenario).await.unwrap();

    assert_eq!(floors(&node), [json!("9"), json!("9"), json!("11")]);
    handle.stop().unwrap();
}

#[tokio::test]
async fn a_read_retries_a_server_that_has_not_reached_the_floor() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;
    scenario.network().produce_block().await.unwrap();

    *node.refuse.lock().unwrap() = 2;
    read(&scenario).await.unwrap();
    assert_eq!(node.reads.lock().unwrap().len(), 3);
    handle.stop().unwrap();
}

#[tokio::test]
async fn a_slow_refusal_does_not_stretch_the_floor_wait() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;
    scenario.network().produce_block().await.unwrap();

    *node.refuse.lock().unwrap() = u32::MAX;
    *node.refusal_delay.lock().unwrap() = Duration::from_millis(1_500);
    let started = tokio::time::Instant::now();
    let error = read(&scenario).await.unwrap_err();
    let waited = started.elapsed();
    // The second request gets only the half second left, and the read
    // reports the last refusal, not a timeout.
    assert!(waited < Duration::from_millis(2_500), "{waited:?}");
    let TestkitError::NativeCall { source, .. } = error else {
        panic!("expected the server's refusal: {error}");
    };
    assert!(
        source.to_string().contains("state height unavailable"),
        "{source}"
    );
    handle.stop().unwrap();
}

#[tokio::test]
async fn a_read_gives_up_after_the_floor_wait() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;
    scenario.network().produce_block().await.unwrap();

    *node.refuse.lock().unwrap() = u32::MAX;
    let started = tokio::time::Instant::now();
    let error = read(&scenario).await.unwrap_err();
    let waited = started.elapsed();
    assert!(
        matches!(
            error,
            TestkitError::NativeCall {
                method: "simulateInvocation",
                ..
            }
        ),
        "{error}"
    );
    assert!(
        (Duration::from_millis(1_900)..Duration::from_millis(2_500)).contains(&waited),
        "{waited:?}"
    );
    handle.stop().unwrap();
}

/// Read a storage item and return the height it was read at.
async fn read_storage(scenario: &Scenario) -> thruput_testkit::Result<i64> {
    scenario
        .network()
        .get_storage_at(ContractAddress::from([1; 32]), &[3; 32])
        .await
        .map(|storage| *storage.state_block_height)
}

#[tokio::test]
async fn a_storage_read_before_any_block_takes_any_state() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;

    *node.storage_lag.lock().unwrap() = 1;
    assert_eq!(read_storage(&scenario).await.unwrap(), 6);
    assert_eq!(*node.storage_reads.lock().unwrap(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn a_storage_read_below_the_floor_is_read_again() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;
    scenario.network().produce_block().await.unwrap();

    *node.storage_lag.lock().unwrap() = 2;
    assert_eq!(read_storage(&scenario).await.unwrap(), 7);
    assert_eq!(*node.storage_reads.lock().unwrap(), 3);
    handle.stop().unwrap();
}

#[tokio::test]
async fn a_storage_read_gives_up_when_the_state_stays_below_the_floor() {
    let node = Arc::new(Node::default());
    let (url, handle) = mock_node(node.clone()).await;
    let (scenario, _manifest) = scenario(&url).await;
    scenario.network().produce_block().await.unwrap();

    *node.storage_lag.lock().unwrap() = u32::MAX;
    let started = tokio::time::Instant::now();
    let error = read_storage(&scenario).await.unwrap_err();
    let waited = started.elapsed();
    assert!(
        matches!(
            error,
            TestkitError::Rpc {
                method: "getStorage",
                ..
            }
        ),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("state at block 6 is below block 7"),
        "{error}"
    );
    assert!(
        (Duration::from_millis(1_900)..Duration::from_millis(2_500)).contains(&waited),
        "{waited:?}"
    );
    handle.stop().unwrap();
}
