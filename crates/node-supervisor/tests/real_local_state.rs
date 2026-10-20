//! Qualification against a compatible real runtime; run explicitly with --ignored.

use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClientBuilder;
use thruput_json_rpc_api::params::NamedParams;
use thruput_json_rpc_types::{airdrop::ClientNonce, base58::Base58, requests::FundAccountRequest};
use thruput_json_rpc_types::{requests as input, responses as output};
use thruput_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, LOCAL_FAUCET_KEY_FILE, LOCAL_FUNDING_ATTEMPTS_FILE,
    NodeSupervisorConfig, NodeSupervisorHandle,
};
use thruput_signing::{LocalSigner, PubkeyAccount};

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY pointing to a schema-4 real runtime"]
async fn persistent_funding_manual_production_and_lock_handoff() {
    let binary = std::env::var_os("THRUPUT_DEVNODE_BINARY").expect("runtime binary is required");
    let dir = tempfile::tempdir().unwrap();
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_block_driver(BlockDriverConfig::manual())
        .with_hogd_dev(
            HogdDevConfig::default()
                .with_binary_path(binary)
                .with_data_dir(dir.path())
                .with_ramdb_bytes(128 * 1024 * 1024),
        );
    let config = match std::env::var("GROUNDHOG_DEVNODE_PROTOCOL") {
        Ok(protocol) => config.with_hogd_protocol_override(protocol),
        Err(_) => config,
    };
    let node = NodeSupervisorHandle::start(config.clone()).await.unwrap();
    let manifest = node.hogd_readiness().runtime_manifest.as_ref().unwrap();
    assert_eq!(manifest.schema_version, 4);
    assert!(manifest.bootstrap_account_derivation.is_none());
    assert_eq!(manifest.bootstrap_accounts.len(), 1);
    let faucet = manifest.bootstrap_accounts[0].wallet_address;
    let recipient_signer = LocalSigner::from_secret_key([0x77; 32]);
    let recipient = Base58(
        PubkeyAccount::from_public_key_bytes(recipient_signer.verifying_key_bytes(), 0).address(),
    );
    let request = FundAccountRequest {
        recipient,
        faucet,
        raw_amount: 123_456_789,
        client_nonce: ClientNonce::new(vec![0x42]).unwrap(),
    };
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", node.rpc_addr()))
        .unwrap();
    let funded = client
        .request::<output::FundingSubmission, _>("dev_fundAccount", NamedParams(request.clone()))
        .await
        .unwrap();
    assert!(
        client
            .request::<output::ReceiptResult, _>(
                "getTransactionReceipt",
                NamedParams(input::TransactionQuery {
                    transaction_hash: funded.transaction_hash
                })
            )
            .await
            .unwrap()
            .is_none()
    );
    node.manual_block_producer()
        .unwrap()
        .produce_block()
        .await
        .unwrap();
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(receipt) = client
                .request::<output::ReceiptResult, _>(
                    "getTransactionReceipt",
                    NamedParams(input::TransactionQuery {
                        transaction_hash: funded.transaction_hash,
                    }),
                )
                .await
                .unwrap()
            {
                break receipt;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        receipt.execution,
        output::ReceiptExecution::Return { .. }
    ));
    node.stop().await.unwrap();
    assert!(dir.path().join(LOCAL_FAUCET_KEY_FILE).is_file());
    assert!(dir.path().join(LOCAL_FUNDING_ATTEMPTS_FILE).is_file());
    let restarted = NodeSupervisorHandle::start(config.clone()).await.unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", restarted.rpc_addr()))
        .unwrap();
    let reconciled = client
        .request::<output::FundingSubmission, _>("dev_fundAccount", NamedParams(request))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(reconciled).unwrap(),
        serde_json::to_value(funded).unwrap()
    );
    restarted.stop().await.unwrap();
    std::fs::remove_file(dir.path().join(LOCAL_FAUCET_KEY_FILE)).unwrap();
    assert!(NodeSupervisorHandle::start(config).await.is_err());
    assert!(!dir.path().join(LOCAL_FAUCET_KEY_FILE).exists());
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY pointing to a schema-4 real runtime"]
async fn stopped_raw_process_handle_releases_persistent_chain_lock() {
    use thruput_node_supervisor::HogdDevProcess;
    let binary = std::env::var_os("THRUPUT_DEVNODE_BINARY").expect("runtime binary is required");
    let dir = tempfile::tempdir().unwrap();
    let config = HogdDevConfig::default()
        .with_binary_path(binary)
        .with_data_dir(dir.path())
        .with_ramdb_bytes(128 * 1024 * 1024);
    let mut first = HogdDevProcess::start(&config).await.unwrap();
    first.stop().await.unwrap();
    let mut second = HogdDevProcess::start(&config).await.unwrap();
    let child_pid = i32::try_from(second.child_id().unwrap()).unwrap();
    // SAFETY: kill accepts the live child PID and a valid signal, with no pointers.
    assert_eq!(unsafe { libc::kill(child_pid, libc::SIGTERM) }, 0);
    second.stopped().await.unwrap();
    let mut third = HogdDevProcess::start(&config).await.unwrap();
    third.stop().await.unwrap();
    // Keep both stopped handles alive until after the successful restarts.
    drop(first);
    drop(second);
}
