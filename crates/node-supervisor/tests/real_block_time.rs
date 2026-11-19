//! Block production and block time controls against a real runtime; run
//! explicitly with --ignored.

#[path = "support/ramdb.rs"]
mod ramdb;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonrpsee::core::client::{ClientT, Error as ClientError};
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thruput_json_rpc_api::params::NamedParams;
use thruput_json_rpc_types::{
    BlockHeight, airdrop::ClientNonce, base58::Base58, error::native::codes, requests as input,
    requests::FundAccountRequest, responses as output,
};
use thruput_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle,
};
use thruput_xdr::types::ContractAddress;

const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;

fn wall_clock_nanos() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn decimal(value: &Value) -> u64 {
    value.as_str().and_then(|text| text.parse().ok()).unwrap()
}

async fn call(client: &HttpClient, method: &str, params: Value) -> Result<Value, ClientError> {
    client.request(method, NamedParams(params)).await
}

async fn committed_timepoint(client: &HttpClient, height: u64) -> u64 {
    let height = BlockHeight::from(i64::try_from(height).unwrap());
    // The indexer can trail the commit by a moment.
    let block = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let block = client
                .request::<output::BlockResult, _>(
                    "getBlockByHeight",
                    NamedParams(input::BlockQuery {
                        block_height: height.clone(),
                    }),
                )
                .await
                .unwrap();
            if let Some(block) = block {
                return block;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the indexer should serve the produced block");
    *block.block_timepoint
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY pointing to a compatible real runtime"]
async fn block_time_controls_drive_a_real_runtime() {
    let binary = std::env::var_os("THRUPUT_DEVNODE_BINARY").expect("runtime binary is required");
    let dir = tempfile::tempdir().unwrap();
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_block_driver(BlockDriverConfig::manual())
        .with_hogd_dev(
            HogdDevConfig::default()
                .with_binary_path(binary)
                .with_data_dir(dir.path())
                .with_ramdb_bytes(ramdb::ramdb_bytes(256 * ramdb::MIB)),
        );
    let config = match std::env::var("GROUNDHOG_DEVNODE_PROTOCOL") {
        Ok(protocol) => config.with_hogd_protocol_override(protocol),
        Err(_) => config,
    };
    let node = NodeSupervisorHandle::start(config).await.unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", node.rpc_addr()))
        .unwrap();

    // hogd commits an empty block for an empty mempool.
    let empty = call(&client, "dev_produceBlock", json!({})).await.unwrap();
    let empty_height = decimal(&empty["blockHeight"]);

    // A block pinned three days ahead commits with exactly that timepoint.
    let target = wall_clock_nanos() + 3 * DAY;
    let pinned = call(
        &client,
        "dev_produceBlock",
        json!({"blockTimepoint": target.to_string()}),
    )
    .await
    .unwrap();
    let pinned_height = decimal(&pinned["blockHeight"]);
    assert_eq!(pinned_height, empty_height + 1);
    assert_eq!(decimal(&pinned["blockTimepoint"]), target);
    assert_eq!(committed_timepoint(&client, pinned_height).await, target);

    // The next block continues from the pinned time.
    let next = call(&client, "dev_produceBlock", json!({})).await.unwrap();
    let next_timepoint = decimal(&next["blockTimepoint"]);
    assert!(next_timepoint >= target && next_timepoint < target + DAY);
    assert_eq!(
        committed_timepoint(&client, decimal(&next["blockHeight"])).await,
        next_timepoint
    );

    // Blocks never move backward.
    let error = call(
        &client,
        "dev_produceBlock",
        json!({"blockTimepoint": target.to_string()}),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, ClientError::Call(ref error) if error.code() == codes::INVALID_PARAMS),
        "{error}"
    );

    // Increasing time moves every later block forward.
    let clock = call(
        &client,
        "dev_increaseTime",
        json!({"nanoseconds": DAY.to_string()}),
    )
    .await
    .unwrap();
    assert!(decimal(&clock["nextBlockTimepoint"]) >= next_timepoint + DAY);
    let blocks = call(
        &client,
        "dev_produceBlocks",
        json!({"count": 3, "intervalNanoseconds": "12000000000"}),
    )
    .await
    .unwrap();
    let blocks = blocks["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert!(decimal(&blocks[0]["blockTimepoint"]) >= next_timepoint + DAY);
    for pair in blocks.windows(2) {
        assert_eq!(
            decimal(&pair[1]["blockTimepoint"]) - decimal(&pair[0]["blockTimepoint"]),
            12_000_000_000
        );
    }

    transaction_waits_for_a_requested_block(&node, &client).await;

    node.stop().await.unwrap();
}

/// In manual mode a transaction waits for a requested block.
async fn transaction_waits_for_a_requested_block(node: &NodeSupervisorHandle, client: &HttpClient) {
    let manifest = node.hogd_readiness().runtime_manifest.as_ref().unwrap();
    let faucet = manifest.bootstrap_accounts[0].wallet_address;
    // A fresh recipient address and request nonce for each run; the test only
    // needs them to be unique, not secret or tied to a key.
    let run: [u8; 32] = Sha256::digest(wall_clock_nanos().to_be_bytes()).into();
    let recipient = Base58(ContractAddress::from(run));
    let funded = client
        .request::<output::FundingSubmission, _>(
            "dev_fundAccount",
            NamedParams(FundAccountRequest {
                recipient,
                faucet,
                // A new account needs enough to hold its own balance.
                raw_amount: 123_456_789,
                client_nonce: ClientNonce::new(run.to_vec()).unwrap(),
            }),
        )
        .await
        .unwrap();
    let receipt_query = || {
        client.request::<output::ReceiptResult, _>(
            "getTransactionReceipt",
            NamedParams(input::TransactionQuery {
                transaction_hash: funded.transaction_hash,
            }),
        )
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(receipt_query().await.unwrap().is_none());
    let block = call(client, "dev_produceBlock", json!({})).await.unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(receipt) = receipt_query().await.unwrap() {
                return receipt;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the funding transaction should land in the requested block");
    assert_eq!(
        u64::try_from(*receipt.block_height).unwrap(),
        decimal(&block["blockHeight"])
    );
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY pointing to a compatible real runtime"]
async fn a_frozen_clock_gives_every_transaction_block_the_tick_time() {
    // A simulation in the style of fir: a chain in the past, the clock frozen,
    // one pinned block per tick, and transactions mined in instant mode.
    const Y2000: u64 = 946_684_800_000_000_000;
    const HOUR: u64 = 60 * 60 * 1_000_000_000;
    let binary = std::env::var_os("THRUPUT_DEVNODE_BINARY").expect("runtime binary is required");
    let dir = tempfile::tempdir().unwrap();
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_block_driver(BlockDriverConfig::instant())
        .with_hogd_dev(
            HogdDevConfig::default()
                .with_binary_path(binary)
                .with_data_dir(dir.path())
                .with_ramdb_bytes(ramdb::ramdb_bytes(256 * ramdb::MIB)),
        );
    let config = match std::env::var("GROUNDHOG_DEVNODE_PROTOCOL") {
        Ok(protocol) => config.with_hogd_protocol_override(protocol),
        Err(_) => config,
    };
    let node = NodeSupervisorHandle::start(config).await.unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", node.rpc_addr()))
        .unwrap();

    let frozen = call(
        &client,
        "dev_setBlockTimeStep",
        json!({"stepNanoseconds": "0"}),
    )
    .await
    .unwrap();
    assert_eq!(frozen["stepNanoseconds"], "0");

    for tick in [Y2000, Y2000 + HOUR] {
        let block = call(
            &client,
            "dev_produceBlock",
            json!({"blockTimepoint": tick.to_string()}),
        )
        .await
        .unwrap();
        assert_eq!(decimal(&block["blockTimepoint"]), tick);
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let height = fund_and_wait(&node, &client).await;
            assert_eq!(
                committed_timepoint(&client, height).await,
                tick,
                "a transaction block in the tick carries the tick time"
            );
        }
    }

    // Clearing the step continues from the tip, not from today.
    let wall = call(&client, "dev_setBlockTimeStep", json!({}))
        .await
        .unwrap();
    assert!(wall.get("stepNanoseconds").is_none());
    let next = decimal(&wall["nextBlockTimepoint"]);
    assert!((Y2000 + HOUR..Y2000 + HOUR + DAY).contains(&next));

    node.stop().await.unwrap();
}

/// Fund a fresh account and return the height of the block it lands in.
async fn fund_and_wait(node: &NodeSupervisorHandle, client: &HttpClient) -> u64 {
    let manifest = node.hogd_readiness().runtime_manifest.as_ref().unwrap();
    let faucet = manifest.bootstrap_accounts[0].wallet_address;
    let run: [u8; 32] = Sha256::digest(wall_clock_nanos().to_be_bytes()).into();
    let funded = client
        .request::<output::FundingSubmission, _>(
            "dev_fundAccount",
            NamedParams(FundAccountRequest {
                recipient: Base58(ContractAddress::from(run)),
                faucet,
                raw_amount: 123_456_789,
                client_nonce: ClientNonce::new(run.to_vec()).unwrap(),
            }),
        )
        .await
        .unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let receipt = client
                .request::<output::ReceiptResult, _>(
                    "getTransactionReceipt",
                    NamedParams(input::TransactionQuery {
                        transaction_hash: funded.transaction_hash,
                    }),
                )
                .await
                .unwrap();
            if let Some(receipt) = receipt {
                return receipt;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("instant mode should mine the transaction");
    u64::try_from(*receipt.block_height).unwrap()
}
