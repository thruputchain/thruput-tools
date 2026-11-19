//! Contract reads use the highest block the handle has seen as their
//! freshness floor, and retry a server that cannot reach it yet.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use serde_json::{Value, json};
use thruput_testkit::{NetworkConfig, Scenario, TestkitError, xdr::types::ContractAddress};
use thruput_xdr::contract::ContractEntrypointID;

mod common;
use common::{native_runtime_document, serve_manifest_network_info};

/// Recorded `simulateInvocation` params, and how many calls to refuse first.
#[derive(Default)]
struct Node {
    reads: Mutex<Vec<Value>>,
    refuse: Mutex<u32>,
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
        .register_method("simulateInvocation", |params, node, _| {
            let params: Value = params.parse()?;
            node.reads.lock().unwrap().push(params);
            let mut refuse = node.refuse.lock().unwrap();
            if *refuse > 0 {
                *refuse -= 1;
                return Err(ErrorObjectOwned::owned(
                    -32012,
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
        (Duration::from_secs(2)..Duration::from_secs(4)).contains(&waited),
        "{waited:?}"
    );
    handle.stop().unwrap();
}
