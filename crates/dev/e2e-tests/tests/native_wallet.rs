//! Opt-in native-wallet qualification on a disposable local chain with a faucet.
//! Set RPC_URL, NATIVE_WALLET_RUNTIME_MANIFEST and NATIVE_WALLET_COUNTER_RV32A,
//! then run this test with --ignored. The manifest must belong to this local node.
//! The artifact must implement Counter 0.1.0 (initialize(u64), get() -> u64),
//! as in client-ts/examples/fixtures/counter.rv32a and its generated binding.
//! Never point this test at a persistent or public network.

use groundhog_json_rpc_client::{
    AirdropApiClient, GroundhogApiClient, HttpClient, HttpClientBuilder,
    poll_transaction_receipt_with_client_config,
};
use groundhog_json_rpc_types::{
    airdrop::{AirdropAttemptResponse, AirdropRequest, ClientNonce},
    hex::Hex0x,
    receipt::{ExecStatus, TransactionReceipt},
    transaction::{ConflictCode, TransactionStatus, TxHash},
};
use groundhog_signing::default_wallet::*;
use groundhog_signing::{
    LocalSigner, NATIVE_HOG_ADDRESS, NATIVE_HOG_TRANSFER_ENTRYPOINT, PubkeyAccount,
    SignTransactionParams, derive_deploy_address, native_hog_transfer_calldata,
    sign_and_encode_transaction_hex, transaction_hash, wallet_contract_hash,
};
use groundhog_xdr::{
    contract::{Contract, ContractBlob, ContractEntrypointID},
    transaction::Transaction,
    types::{ContractAddress, NetworkHash, SHA256Hash},
};
use std::time::Duration;
use xdr_common::BytesM;

async fn submit(
    client: &HttpClient,
    signer: &LocalSigner,
    network: &NetworkHash,
    tx: Transaction,
) -> TxHash {
    let hash = TxHash(Hex0x(transaction_hash(&tx).unwrap()));
    let bytes =
        sign_and_encode_transaction_hex(tx, signer, SignTransactionParams::default(), network)
            .unwrap();
    eprintln!("submitting {}", hex::encode(hash.0.0.as_ref()));
    assert_eq!(client.send_raw_transaction(bytes).await.unwrap(), hash);
    hash
}

async fn success(client: &HttpClient, hash: TxHash) -> TransactionReceipt {
    let receipt =
        poll_transaction_receipt_with_client_config(client, hash, 120, Duration::from_millis(250))
            .await
            .unwrap();
    assert!(matches!(receipt.status, ExecStatus::Ok), "{receipt:?}");
    receipt
}

async fn balance(client: &HttpClient, address: ContractAddress) -> u128 {
    client
        .get_balance(Hex0x(address))
        .await
        .unwrap()
        .raw_balance
        .parse()
        .unwrap()
}

#[tokio::test]
#[ignore = "requires explicitly selected disposable local RPC and Counter 0.1.0 artifact"]
async fn native_transfer_and_prefunded_deploy() {
    qualify_native_deployment(false).await;
}

#[tokio::test]
#[ignore = "requires a disposable local RPC with atomic funded deployment support"]
async fn native_atomic_funded_deploy() {
    qualify_native_deployment(true).await;
}

async fn qualify_native_deployment(atomic: bool) {
    let url = std::env::var("RPC_URL").expect("RPC_URL must identify a disposable local chain");
    let artifact =
        std::env::var("NATIVE_WALLET_COUNTER_RV32A").expect("Counter 0.1.0 artifact path required");
    let client = HttpClientBuilder::default().build(url).unwrap();
    let network = NetworkHash::new(client.network_info().await.unwrap().network_hash.0);
    let manifest: groundhog_json_rpc_types::runtime_manifest::RuntimeManifest =
        serde_json::from_slice(
            &std::fs::read(
                std::env::var("NATIVE_WALLET_RUNTIME_MANIFEST")
                    .expect("local runtime manifest required"),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(manifest.network_hash.to_network_hash(), network);
    let costs = &manifest.bootstrap.storage_cost_model;
    let balance_cost: u128 = costs.balance_cost.parse().unwrap();
    let replay_cost = costs.base_data_item_cost.parse::<u128>().unwrap()
        + costs.replay_cache_cost_per_entry.parse::<u128>().unwrap();
    let mut secret = [0; 32];
    getrandom::getrandom(&mut secret).unwrap();
    let signer = LocalSigner::from_secret_key(secret);
    let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    assert_eq!(balance(&client, account.address()).await, 0);
    let request = AirdropRequest {
        recipient: Hex0x(account.address()),
        client_nonce: ClientNonce::new(vec![0x37]).unwrap(),
    };
    let mut funded = false;
    for _ in 0..120 {
        match client.request_airdrop(request.clone()).await.unwrap() {
            AirdropAttemptResponse::Succeeded { status, .. } => {
                assert!(matches!(status, ExecStatus::Ok));
                funded = true;
                break;
            }
            AirdropAttemptResponse::Pending { .. } => {
                tokio::time::sleep(Duration::from_millis(250)).await
            }
            other => panic!("unexpected airdrop response: {other:?}"),
        }
    }
    assert!(funded, "airdrop did not confirm");
    let contract = Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(std::fs::read(artifact).unwrap()),
        },
    };
    let hash = wallet_contract_hash(&contract).unwrap();
    let unfunded_salt = SHA256Hash::from([1; 32]);
    let unfunded_target = derive_deploy_address(&account.address(), &unfunded_salt, &hash).unwrap();
    let negative = build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
        account: &account,
        deployment_nonce: unfunded_salt,
        contract: contract.clone(),
        amount: 0,
        initializer: None,
        gas_limit: 1_000_000,
        block_expiry_height: *client.chain_tip().await.unwrap().height + 100,
        nonce: 1,
    })
    .unwrap();
    let negative_hash = submit(&client, &signer, &network, negative).await;
    let mut saw_funding_failure = false;
    for _ in 0..120 {
        match client.get_transaction_status(negative_hash).await.unwrap() {
            TransactionStatus::RecentFailure { conflict, .. }
                if conflict.address.0 == unfunded_target
                    && matches!(
                        conflict.code,
                        ConflictCode::BoundsExceeded | ConflictCode::InsufficientFunds
                    ) =>
            {
                saw_funding_failure = true;
                break;
            }
            TransactionStatus::Committed { .. } => panic!("unfunded deploy unexpectedly committed"),
            TransactionStatus::Invalid { reason } => panic!("unexpected rejection: {reason}"),
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    assert!(
        saw_funding_failure,
        "expected target storage funding failure"
    );
    // RecentFailure is advisory. Never fund that target or replace that attempt:
    // use a different deployment identity for the positive test.
    let salt = SHA256Hash::from([2; 32]);
    let target = derive_deploy_address(&account.address(), &salt, &hash).unwrap();
    let before = balance(&client, account.address()).await;
    let amount = 50_000_000; // 5 HOG tests feasibility, not a minimum-cost estimate.
    assert!(before > amount, "local faucet must fund more than 5 HOG");
    let code_size = match &contract {
        Contract::kRv32 { rv32_contract } => rv32_contract.data.len() as u128,
        _ => unreachable!(),
    };
    if !atomic {
        let calldata = native_hog_transfer_calldata(&target, amount).unwrap();
        let transfer = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
            account: &account,
            target_address: NATIVE_HOG_ADDRESS,
            target_entrypoint: NATIVE_HOG_TRANSFER_ENTRYPOINT,
            target_calldata: &calldata,
            target_gas_limit: 500_000,
            gas_limit: 1_000_000,
            block_expiry_height: *client.chain_tip().await.unwrap().height + 100,
            nonce: 2,
        })
        .unwrap();
        let receipt = success(&client, submit(&client, &signer, &network, transfer).await).await;
        assert!(
            decode_default_wallet_call_return(&receipt.return_data.unwrap().0)
                .unwrap()
                .is_empty()
        );
        // getBalance reports free HOG, after storage collateral is reserved. The
        // sender creates a replay entry; the fresh recipient creates a balance item.
        assert_eq!(
            balance(&client, account.address()).await,
            before - amount - replay_cost
        );
        assert_eq!(balance(&client, target).await, amount - balance_cost);
    }
    let deployment = BuildDefaultWalletDeployTransaction {
        account: &account,
        deployment_nonce: salt,
        contract,
        amount: if atomic { amount } else { 0 },
        initializer: None,
        gas_limit: 1_000_000,
        block_expiry_height: *client.chain_tip().await.unwrap().height + 100,
        nonce: 3,
    };
    let deploy = build_default_wallet_deploy_transaction(deployment).unwrap();
    let receipt = success(&client, submit(&client, &signer, &network, deploy).await).await;
    let deployed: ContractAddress = xdrs::cxs::decode(&receipt.return_data.unwrap().0).unwrap();
    assert_eq!(deployed, target);
    if atomic {
        assert_eq!(
            balance(&client, account.address()).await,
            before - amount - replay_cost
        );
        let deployment_cost = costs.base_contract_deployment_cost.parse::<u128>().unwrap()
            + code_size
                * costs
                    .contract_deployment_cost_per_byte
                    .parse::<u128>()
                    .unwrap();
        assert_eq!(
            balance(&client, target).await,
            amount - balance_cost - deployment_cost
        );
    }
    for (selector, calldata, nonce) in [
        (
            0x2bfe_21a6_2e51_19c4_u64,
            xdrs::cxs::encode(&42_u64).unwrap(),
            4,
        ),
        (0x4a63_f587_ead2_5ca6_u64, vec![], 5),
    ] {
        let call = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
            account: &account,
            target_address: target,
            target_entrypoint: ContractEntrypointID::from(selector.to_be_bytes()),
            target_calldata: &calldata,
            target_gas_limit: 500_000,
            gas_limit: 1_000_000,
            block_expiry_height: *client.chain_tip().await.unwrap().height + 100,
            nonce,
        })
        .unwrap();
        let receipt = success(&client, submit(&client, &signer, &network, call).await).await;
        let returned = decode_default_wallet_call_return(&receipt.return_data.unwrap().0).unwrap();
        if nonce == 5 {
            assert_eq!(xdrs::cxs::decode::<u64>(&returned).unwrap(), 42);
        }
    }
    eprintln!(
        "qualified account={} contract={}",
        hex::encode(account.address().as_ref()),
        hex::encode(target.as_ref())
    );
}
