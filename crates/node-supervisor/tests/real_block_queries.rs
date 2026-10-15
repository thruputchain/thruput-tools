//! Block-query qualification through both HTTP adapters and a real persistent index.

use std::{path::Path, time::Duration};

use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use thruput_json_rpc_api::{GroundhogApiClient, LocalFundingApiClient};
use thruput_json_rpc_types::{
    BlockHeight,
    airdrop::{ClientNonce, FundAccountRequest},
    events::{IterationOrder, PaginationCursor},
    hex::Hex0x,
    transaction::{BlockTransactionQuery, TxHash},
};
use thruput_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle,
};
use thruput_signing::{LocalSigner, PubkeyAccount};

fn node_client(node: &NodeSupervisorHandle) -> HttpClient {
    HttpClientBuilder::default()
        .build(format!("http://{}", node.rpc_addr()))
        .unwrap()
}

async fn gateway(node: &NodeSupervisorHandle, log: &Path) -> (tokio::process::Child, HttpClient) {
    let binary = std::env::var_os("THRUPUT_GATEWAY_BINARY").expect("gateway binary is required");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let child = tokio::process::Command::new(binary)
        .arg("--runtime-manifest")
        .arg(node.hogd_readiness().manifest_path.as_ref().unwrap())
        .arg("--listen")
        .arg(address.to_string())
        .arg("--scheduler-reconnect-grace-ms")
        .arg("0")
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(log).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{address}"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if client.chain_tip().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "gateway startup: {error}; {}",
            std::fs::read_to_string(log).unwrap()
        )
    });
    (child, client)
}

async fn verify(
    client: &HttpClient,
    height: BlockHeight,
    hashes: &[TxHash],
    empty_height: BlockHeight,
) {
    let block = client
        .get_block_by_number(height.clone().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(block.block_height, height);
    assert!(
        client
            .get_block_by_number(BlockHeight::from(0).into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        client
            .get_block_by_number(BlockHeight::from(i64::MAX).into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        client
            .get_block_by_number(BlockHeight::from(-1).into())
            .await
            .is_err()
    );
    for order in [IterationOrder::Ascending, IterationOrder::Descending] {
        let mut query = BlockTransactionQuery {
            height: height.clone(),
            order,
            cursor: PaginationCursor(vec![]),
            limit: 1,
        };
        let mut observed = Vec::new();
        loop {
            let page = client
                .get_transactions_by_block(query.clone())
                .await
                .unwrap();
            assert_eq!(*page.oldest_block, 1);
            assert!(page.latest_block >= empty_height);
            assert_eq!(page.refs.len(), 1);
            for item in page.refs {
                assert_eq!(item.height, height);
                let receipt = client
                    .get_transaction_receipt(item.tx_hash)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(receipt.height, height);
                observed.push(item.tx_hash);
            }
            assert!(
                observed.len() <= hashes.len(),
                "pagination repeated references"
            );
            if page.cursor.0.is_empty() {
                break;
            }
            query.cursor = page.cursor;
        }
        let mut expected = hashes.to_vec();
        expected.sort_by_key(|hash| hash.0.0);
        if order == IterationOrder::Descending {
            expected.reverse();
        }
        assert_eq!(observed, expected);
    }
    assert!(
        client
            .get_block_by_number(empty_height.clone().into())
            .await
            .unwrap()
            .is_some()
    );
    let mut query = BlockTransactionQuery {
        height: empty_height,
        order: IterationOrder::Ascending,
        cursor: PaginationCursor(vec![]),
        limit: 50,
    };
    let empty = client
        .get_transactions_by_block(query.clone())
        .await
        .unwrap();
    assert!(empty.refs.is_empty() && empty.cursor.0.is_empty());
    query.height = height;
    query.cursor = PaginationCursor(vec![1, 2, 3]);
    let invalid = client
        .get_transactions_by_block(query.clone())
        .await
        .unwrap();
    assert!(invalid.refs.is_empty() && invalid.cursor.0.is_empty());
    query.cursor.0.resize(257, 0);
    assert!(client.get_transactions_by_block(query).await.is_err());
}

#[tokio::test]
#[ignore = "requires updated THRUPUT_DEVNODE_BINARY and THRUPUT_GATEWAY_BINARY"]
async fn block_queries_match_receipts_through_both_servers_before_and_after_restart() {
    let binary = std::env::var_os("THRUPUT_DEVNODE_BINARY").expect("runtime binary is required");
    let dir = tempfile::tempdir().unwrap();
    let mut config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_block_driver(BlockDriverConfig::manual())
        .with_hogd_dev(
            HogdDevConfig::default()
                .with_binary_path(binary)
                .with_data_dir(dir.path())
                .with_ramdb_bytes(128 * 1024 * 1024),
        );
    if let Ok(protocol) = std::env::var("GROUNDHOG_DEVNODE_PROTOCOL") {
        config = config.with_hogd_protocol_override(protocol);
    }
    let node = NodeSupervisorHandle::start(config.clone()).await.unwrap();
    let client = node_client(&node);
    let genesis = client.chain_tip().await.unwrap();
    let faucet = node
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .unwrap()
        .bootstrap_accounts[0]
        .wallet_address;
    let signer = LocalSigner::from_secret_key([0x79; 32]);
    let recipient =
        Hex0x(PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address());
    let mut hashes = Vec::new();
    for nonce in 1..=3 {
        let result = client
            .fund_account(FundAccountRequest {
                recipient,
                faucet,
                raw_amount: "100000".into(),
                client_nonce: ClientNonce::new(vec![nonce]).unwrap(),
            })
            .await
            .unwrap();
        hashes.push(result.tx_hash);
    }
    node.manual_block_producer()
        .unwrap()
        .produce_block()
        .await
        .unwrap();
    let height = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(receipt) = client.get_transaction_receipt(hashes[2]).await.unwrap() {
                break receipt.height;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let block = client
        .get_block_by_number(height.clone().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        block.block_hash,
        client.chain_tip().await.unwrap().block_hash
    );
    assert_eq!(block.prev_block_hash, genesis.block_hash);
    node.manual_block_producer()
        .unwrap()
        .produce_block()
        .await
        .unwrap();
    let empty_height = client.chain_tip().await.unwrap().height;
    tokio::time::timeout(Duration::from_secs(30), async {
        while client
            .get_block_by_number(empty_height.clone().into())
            .await
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (mut process, remote) = gateway(&node, &dir.path().join("gateway.log")).await;
    verify(&client, height.clone(), &hashes, empty_height.clone()).await;
    verify(&remote, height.clone(), &hashes, empty_height.clone()).await;
    process.kill().await.unwrap();
    process.wait().await.unwrap();
    node.stop().await.unwrap();

    let node = NodeSupervisorHandle::start(config).await.unwrap();
    let client = node_client(&node);
    let (mut process, remote) = gateway(&node, &dir.path().join("gateway-restart.log")).await;
    verify(&client, height.clone(), &hashes, empty_height.clone()).await;
    verify(&remote, height, &hashes, empty_height).await;
    process.kill().await.unwrap();
    process.wait().await.unwrap();
    node.stop().await.unwrap();
}
