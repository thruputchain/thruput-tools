//! Two real signing domains and persistent identity. Run explicitly with --ignored.

use std::time::Duration;

use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use thruput_json_rpc_api::{GroundhogApiClient, LocalFundingApiClient};
use thruput_json_rpc_types::{
    ContractAddress, NetworkHashText,
    airdrop::{ClientNonce, FundAccountRequest},
    base58::Base58,
    bootstrap_manifest::BootstrapManifestAllocation,
    receipt::ExecStatus,
    runtime_manifest::RuntimeManifestChainState,
    transaction::TxHash,
};
use thruput_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle,
};
use thruput_signing::{
    LocalSigner, PubkeyAccount, SignTransactionParams,
    default_wallet::{BuildDefaultWalletCallTransaction, build_default_wallet_call_transaction},
    sign_and_encode_transaction_hex, transaction_hash,
};
use thruput_xdr::transaction::Transaction;

fn config(
    hash: NetworkHashText,
    accounts: Vec<BootstrapManifestAllocation>,
) -> NodeSupervisorConfig {
    let binary = std::env::var_os("THRUPUT_DEVNODE_BINARY").expect("runtime binary is required");
    let artifacts = std::path::Path::new(&binary)
        .parent()
        .unwrap()
        .join("contracts");
    NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_block_driver(BlockDriverConfig::manual())
        .with_hogd_dev(
            HogdDevConfig::default()
                .with_binary_path(binary)
                .with_contract_artifact_directories([artifacts])
                .with_init_network_hash(hash)
                .with_bootstrap_accounts(accounts),
        )
}

fn client(node: &NodeSupervisorHandle) -> HttpClient {
    HttpClientBuilder::default()
        .build(format!("http://{}", node.rpc_addr()))
        .unwrap()
}

async fn balance(client: &HttpClient, address: ContractAddress) -> u128 {
    client
        .get_balance(address)
        .await
        .unwrap()
        .raw_balance
        .parse()
        .unwrap()
}

async fn receipt(client: &HttpClient, hash: TxHash) {
    let receipt = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(receipt) = client.get_transaction_receipt(hash).await.unwrap() {
                break receipt;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(receipt.status, ExecStatus::Ok), "{receipt:?}");
}

async fn check_identity(node: &NodeSupervisorHandle, expected: &NetworkHashText) {
    let readiness = node.hogd_readiness();
    assert_eq!(&readiness.network_hash, expected);
    assert_eq!(
        &readiness.runtime_manifest.as_ref().unwrap().network_hash,
        expected
    );
    assert_eq!(
        client(node)
            .network_info()
            .await
            .unwrap()
            .network_hash
            .as_ref(),
        expected.to_network_hash().as_ref()
    );
}

async fn fund(node: &NodeSupervisorHandle, recipient: ContractAddress) {
    let manifest = node.hogd_readiness().runtime_manifest.as_ref().unwrap();
    let client = client(node);
    let before = balance(&client, recipient).await;
    let response = client
        .fund_account(FundAccountRequest {
            recipient,
            faucet: manifest.bootstrap_accounts[0].wallet_address,
            raw_amount: "10000000".into(),
            client_nonce: ClientNonce::new(vec![1]).unwrap(),
        })
        .await
        .unwrap();
    node.manual_block_producer()
        .unwrap()
        .produce_block()
        .await
        .unwrap();
    receipt(&client, response.tx_hash).await;
    assert_eq!(balance(&client, recipient).await, before + 10_000_000);
}

fn transfer(signer: &LocalSigner, recipient: ContractAddress) -> Transaction {
    let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    let calldata = thruput_signing::native_hog_transfer_calldata(&recipient.0, 1_000_000).unwrap();
    build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
        account: &account,
        target_address: thruput_signing::NATIVE_HOG_ADDRESS,
        target_entrypoint: thruput_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
        target_calldata: &calldata,
        gas_limit: 25_000_000,
        target_gas_limit: 25_000_000,
        block_expiry_height: 1000,
        nonce: 42,
    })
    .unwrap()
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY pointing to a compatible schema-4 real runtime"]
#[expect(
    clippy::too_many_lines,
    reason = "one process qualification covers domain isolation and restart recovery"
)]
async fn distinct_domains_and_persistent_identity() {
    let sender = LocalSigner::from_secret_key([0x71; 32]);
    let recipient = LocalSigner::from_secret_key([0x72; 32]);
    let accounts = [&sender, &recipient]
        .map(|signer| {
            BootstrapManifestAllocation::new(
                Base58(signer.verifying_key_bytes().into()),
                "1000000000",
            )
        })
        .to_vec();
    let sender_address =
        Base58(PubkeyAccount::from_public_key_bytes(sender.verifying_key_bytes(), 0).address());
    let recipient_address =
        Base58(PubkeyAccount::from_public_key_bytes(recipient.verifying_key_bytes(), 0).address());
    let a_hash = NetworkHashText::from([0xaa; 32]);
    let b_hash = NetworkHashText::from([0xbb; 32]);
    let a_config = config(a_hash.clone(), accounts.clone());
    let dir = tempfile::tempdir().unwrap();
    let mut b_config = config(b_hash.clone(), accounts);
    b_config.hogd_dev = b_config.hogd_dev.with_data_dir(dir.path());
    let a = NodeSupervisorHandle::start(a_config).await.unwrap();
    let b = NodeSupervisorHandle::start(b_config.clone()).await.unwrap();
    check_identity(&a, &a_hash).await;
    check_identity(&b, &b_hash).await;
    fund(&a, sender_address).await;
    fund(&b, sender_address).await;
    let a_client = client(&a);
    let b_client = client(&b);
    let tx = transfer(&sender, recipient_address);
    let hash = TxHash(Base58(transaction_hash(&tx).unwrap()));
    let a_signed = sign_and_encode_transaction_hex(
        tx.clone(),
        &sender,
        SignTransactionParams::default(),
        &a_hash.to_network_hash(),
    )
    .unwrap();
    let b_signed = sign_and_encode_transaction_hex(
        tx,
        &sender,
        SignTransactionParams::default(),
        &b_hash.to_network_hash(),
    )
    .unwrap();
    assert_ne!(a_signed, b_signed);
    // The identical payload succeeds on A; only the signature domain differs on B.
    assert_eq!(
        a_client
            .send_raw_transaction(a_signed.clone())
            .await
            .unwrap(),
        hash
    );
    a.manual_block_producer()
        .unwrap()
        .produce_block()
        .await
        .unwrap();
    receipt(&a_client, hash).await;
    let sender_before = balance(&b_client, sender_address).await;
    let recipient_before = balance(&b_client, recipient_address).await;
    let wrong_domain = b_client.send_raw_transaction(a_signed).await;
    assert!(
        matches!(&wrong_domain, Err(jsonrpsee::core::ClientError::Call(error))
        if error.message().contains("authentication")),
        "{wrong_domain:?}"
    );
    b.manual_block_producer()
        .unwrap()
        .produce_block()
        .await
        .unwrap();
    assert!(
        b_client
            .get_transaction_receipt(hash)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(balance(&b_client, sender_address).await, sender_before);
    assert_eq!(
        balance(&b_client, recipient_address).await,
        recipient_before
    );
    eprintln!("A-domain transaction on B: {wrong_domain:?}; no receipt or balance effects");
    assert_eq!(b_client.send_raw_transaction(b_signed).await.unwrap(), hash);
    b.manual_block_producer()
        .unwrap()
        .produce_block()
        .await
        .unwrap();
    receipt(&b_client, hash).await;
    let sender_after = balance(&b_client, sender_address).await;
    let recipient_after = balance(&b_client, recipient_address).await;
    assert!(sender_after <= sender_before - 1_000_000);
    assert_eq!(recipient_after, recipient_before + 1_000_000);
    let height = *b_client.chain_tip().await.unwrap().height;
    assert!(height >= 3);
    b.stop().await.unwrap();
    a.stop().await.unwrap();
    // Reinitialization is forbidden, even with the original hash, and preserves state.
    let journal_path = dir
        .path()
        .join(thruput_node_supervisor::LOCAL_FUNDING_ATTEMPTS_FILE);
    let journal_before = std::fs::read(&journal_path).unwrap();
    for requested in [a_hash, b_hash.clone(), [0; 32].into()] {
        let mut wrong = b_config.clone();
        wrong.hogd_dev.init_network_hash = Some(requested);
        let error = NodeSupervisorHandle::start(wrong).await.unwrap_err();
        assert!(error.to_string().contains("already initialized"), "{error}");
        assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);
    }
    // Restart supplies no identity; the persisted journal drives bootstrap and validation.
    b_config.hogd_dev.init_network_hash = None;
    let restarted = NodeSupervisorHandle::start(b_config).await.unwrap();
    check_identity(&restarted, &b_hash).await;
    assert_eq!(
        restarted.hogd_readiness().chain_state,
        Some(RuntimeManifestChainState::Recovered)
    );
    let restarted_client = client(&restarted);
    assert!(*restarted_client.chain_tip().await.unwrap().height >= height);
    receipt(&restarted_client, hash).await;
    assert_eq!(
        balance(&restarted_client, sender_address).await,
        sender_after
    );
    assert_eq!(
        balance(&restarted_client, recipient_address).await,
        recipient_after
    );
    // Also validate retained faucet signatures on restart, then continue the chain.
    fund(&restarted, recipient_address).await;
    assert!(*restarted_client.chain_tip().await.unwrap().height > height);
    eprintln!(
        "B-domain transfer committed; custom hash, receipts, balances and funding survived restart at height {height}"
    );
    restarted.stop().await.unwrap();
}
