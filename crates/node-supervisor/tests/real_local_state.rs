//! Qualification against a compatible real runtime; run explicitly with --ignored.

use groundhog_json_rpc_api::{GroundhogApiClient, LocalFundingApiClient};
use groundhog_json_rpc_types::{
    airdrop::{ClientNonce, FundAccountRequest},
    hex::Hex0x,
    receipt::ExecStatus,
};
use groundhog_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, LOCAL_FAUCET_KEY_FILE, LOCAL_FUNDING_ATTEMPTS_FILE,
    NodeSupervisorConfig, NodeSupervisorHandle,
};
use groundhog_signing::{LocalSigner, PubkeyAccount};
use jsonrpsee::http_client::HttpClientBuilder;

#[tokio::test]
#[ignore = "requires GROUNDHOG_DEVNODE_BINARY pointing to a schema-4 real runtime"]
async fn persistent_funding_manual_production_and_lock_handoff() {
    let binary = std::env::var_os("GROUNDHOG_DEVNODE_BINARY").expect("runtime binary is required");
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
    let recipient = Hex0x(
        PubkeyAccount::from_public_key_bytes(recipient_signer.verifying_key_bytes(), 0).address(),
    );
    let request = FundAccountRequest {
        recipient,
        faucet,
        raw_amount: "123456789".into(),
        client_nonce: ClientNonce::new(vec![0x42]).unwrap(),
    };
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", node.rpc_addr()))
        .unwrap();
    let funded = client.fund_account(request.clone()).await.unwrap();
    assert!(
        client
            .get_transaction_receipt(funded.tx_hash)
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
                .get_transaction_receipt(funded.tx_hash)
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
    assert!(matches!(receipt.status, ExecStatus::Ok));
    node.stop().await.unwrap();
    assert!(dir.path().join(LOCAL_FAUCET_KEY_FILE).is_file());
    assert!(dir.path().join(LOCAL_FUNDING_ATTEMPTS_FILE).is_file());
    let restarted = NodeSupervisorHandle::start(config.clone()).await.unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", restarted.rpc_addr()))
        .unwrap();
    assert_eq!(client.fund_account(request).await.unwrap(), funded);
    restarted.stop().await.unwrap();
    std::fs::remove_file(dir.path().join(LOCAL_FAUCET_KEY_FILE)).unwrap();
    assert!(NodeSupervisorHandle::start(config).await.is_err());
    assert!(!dir.path().join(LOCAL_FAUCET_KEY_FILE).exists());
}

#[tokio::test]
#[ignore = "requires GROUNDHOG_DEVNODE_BINARY pointing to a schema-4 real runtime"]
async fn stopped_raw_process_handle_releases_persistent_chain_lock() {
    use groundhog_node_supervisor::HogdDevProcess;
    let binary = std::env::var_os("GROUNDHOG_DEVNODE_BINARY").expect("runtime binary is required");
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
