//! Block production and block time controls call the node's `dev_` methods.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use serde_json::{Value, json};
use thruput_testkit::{BlockDriverMode, NetworkConfig, Scenario, xdr::types::TimePoint};

mod common;
use common::{native_runtime_document, serve_manifest_network_info};

type Calls = Arc<Mutex<Vec<(&'static str, Value)>>>;

fn record(module: &mut RpcModule<Calls>, method: &'static str, result: Value) {
    module
        .register_method(method, move |params, calls, _| {
            let params: Value = params.parse()?;
            calls.lock().unwrap().push((method, params));
            Ok::<_, ErrorObjectOwned>(result.clone())
        })
        .unwrap();
}

/// A mock node that records each `dev_` request and returns a fixed result.
async fn mock_node(calls: Calls) -> (String, jsonrpsee::server::ServerHandle) {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(calls.clone());
    serve_manifest_network_info(&mut module);
    record(
        &mut module,
        "dev_produceBlock",
        json!({"blockHeight": "7", "blockTimepoint": "1700000000000000000"}),
    );
    record(
        &mut module,
        "dev_produceBlocks",
        json!({"blocks": [
            {"blockHeight": "8", "blockTimepoint": "1700000000000000000"},
            {"blockHeight": "9", "blockTimepoint": "1700000012000000000"},
        ]}),
    );
    record(
        &mut module,
        "dev_setNextBlockTimepoint",
        json!({"nextBlockTimepoint": "1800000000000000000"}),
    );
    record(
        &mut module,
        "dev_increaseTime",
        json!({"nextBlockTimepoint": "1800259200000000000"}),
    );
    record(
        &mut module,
        "dev_setBlockDriver",
        json!({"mode": "interval", "intervalMilliseconds": "250"}),
    );
    (url, server.start(module))
}

#[tokio::test]
async fn block_controls_send_decimal_nanoseconds_and_decode_results() {
    let calls = Calls::default();
    let (url, handle) = mock_node(calls.clone()).await;
    let manifest = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        manifest.path(),
        serde_json::to_vec(&native_runtime_document()).unwrap(),
    )
    .unwrap();
    let scenario = Scenario::builder("block-control")
        .accounts(1)
        .network(NetworkConfig::existing(&url, manifest.path()))
        .build()
        .await
        .unwrap();
    let network = scenario.network();

    let block = network.produce_block().await.unwrap();
    assert_eq!(block.height, 7);
    assert_eq!(*block.timepoint, 1_700_000_000_000_000_000);
    network
        .produce_block_at(TimePoint::new(1_700_000_000_000_000_000))
        .await
        .unwrap();
    let blocks = network
        .produce_blocks(2, Some(Duration::from_secs(12)))
        .await
        .unwrap();
    assert_eq!(
        blocks.iter().map(|block| block.height).collect::<Vec<_>>(),
        [8, 9]
    );
    let next = network
        .set_next_block_timepoint(TimePoint::new(1_800_000_000_000_000_000))
        .await
        .unwrap();
    assert_eq!(*next, 1_800_000_000_000_000_000);
    let next = network
        .increase_time(Duration::from_hours(72))
        .await
        .unwrap();
    assert_eq!(*next, 1_800_259_200_000_000_000);
    network
        .set_block_driver(BlockDriverMode::Interval, Some(Duration::from_millis(250)))
        .await
        .unwrap();

    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("dev_produceBlock", json!({})),
            (
                "dev_produceBlock",
                json!({"blockTimepoint": "1700000000000000000"})
            ),
            (
                "dev_produceBlocks",
                json!({"count": 2, "intervalNanoseconds": "12000000000"})
            ),
            (
                "dev_setNextBlockTimepoint",
                json!({"blockTimepoint": "1800000000000000000"})
            ),
            (
                "dev_increaseTime",
                json!({"nanoseconds": "259200000000000"})
            ),
            (
                "dev_setBlockDriver",
                json!({"mode": "interval", "intervalMilliseconds": "250"})
            ),
        ]
    );
    handle.stop().unwrap();
}
