//! Opt-in native-wallet qualification on a disposable local chain with a faucet.
//! Set RPC_URL, NATIVE_WALLET_RUNTIME_MANIFEST and NATIVE_WALLET_COUNTER_RV32A,
//! then run this test with --ignored. The manifest must belong to this local node.
//! The artifact must implement Counter 0.1.0 (initialize(u64), get() -> u64),
//! as in client-ts/examples/fixtures/counter.rv32a and its generated binding.
//! Never point this test at a persistent or public network.

use std::time::Duration;
use thruput_json_rpc_client::{
    HttpClient, HttpClientBuilder,
    native::{
        NativeClient, NativeClientError,
        receipt::{ReceiptPollConfig, poll_receipt},
    },
};
use thruput_json_rpc_types::{
    airdrop::ClientNonce,
    base58::Base58,
    hex::HexBytes,
    requests::{
        AddressQuery, AirdropRequest, EmptyRequest, SendTransactionRequest, TransactionQuery,
    },
    responses::{
        AirdropAttempt, ConflictCode, DecodedResponse, FailureCause, ReceiptExecution,
        RecentAttemptFailure, SchedulerErrorSource, TransactionReceipt, TransactionStatus,
    },
    transaction::TxHash,
};
use thruput_signing::default_wallet::*;
use thruput_signing::{
    LocalSigner, NATIVE_HOG_ADDRESS, NATIVE_HOG_TRANSFER_ENTRYPOINT, PubkeyAccount,
    SignTransactionParams, derive_deploy_address, native_hog_transfer_calldata,
    sign_and_encode_transaction_hex, transaction_hash, wallet_contract_hash,
};
use thruput_xdr::{
    contract::{Contract, ContractBlob, ContractEntrypointID},
    transaction::Transaction,
    types::{ContractAddress, NetworkHash, SHA256Hash},
};
use xdr_common::BytesM;

fn known<T: std::fmt::Debug>(response: Result<DecodedResponse<T>, NativeClientError>) -> T {
    match response.expect("native RPC response") {
        DecodedResponse::Known(value) => value,
        other => panic!("unsupported native response: {other:?}"),
    }
}

async fn submit(
    client: &NativeClient<HttpClient>,
    signer: &LocalSigner,
    network: &NetworkHash,
    tx: Transaction,
) -> TxHash {
    let hash = TxHash(Base58(transaction_hash(&tx).unwrap()));
    let bytes =
        sign_and_encode_transaction_hex(tx, signer, SignTransactionParams::default(), network)
            .unwrap();
    eprintln!("submitting {hash:?}");
    let ack = known(
        client
            .send_transaction(SendTransactionRequest {
                signed_transaction: HexBytes(hex::decode(bytes.trim_start_matches("0x")).unwrap()),
            })
            .await,
    );
    assert_eq!(ack.transaction_hash, hash);
    hash
}

async fn success(
    client: &NativeClient<HttpClient>,
    network: &NetworkHash,
    hash: TxHash,
) -> TransactionReceipt {
    let receipt = poll_receipt(
        client,
        Base58(**network),
        hash,
        None,
        ReceiptPollConfig {
            timeout: Duration::from_secs(30),
            interval: Duration::from_millis(250),
        },
        std::future::pending(),
    )
    .await
    .unwrap();
    assert!(
        matches!(receipt.execution, ReceiptExecution::Return { .. }),
        "{receipt:?}"
    );
    receipt
}

fn returned(receipt: &TransactionReceipt) -> &[u8] {
    match &receipt.execution {
        ReceiptExecution::Return { return_data } => &return_data.0,
        other => panic!("execution did not return: {other:?}"),
    }
}

async fn balance(
    client: &NativeClient<HttpClient>,
    address: ContractAddress,
    min_height: Option<&thruput_json_rpc_types::BlockHeight>,
) -> u128 {
    let balance = known(
        client
            .get_balance(AddressQuery {
                address: Base58(address),
            })
            .await,
    );
    assert_eq!(balance.address.0, address);
    if let Some(min_height) = min_height {
        assert!(
            balance.state_block_height >= *min_height,
            "stale balance at {:?}, need {min_height:?}",
            balance.state_block_height
        );
    }
    u128::try_from(balance.raw_balance).unwrap()
}

async fn expiry(client: &NativeClient<HttpClient>) -> i64 {
    *known(client.get_chain_tip(EmptyRequest {}).await).block_height + 100
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

struct WalletFixture {
    client: NativeClient<HttpClient>,
    network: NetworkHash,
    manifest: thruput_json_rpc_types::runtime_manifest::RuntimeManifest,
    signer: LocalSigner,
    account: PubkeyAccount,
    contract: Contract,
    funding_block_height: thruput_json_rpc_types::BlockHeight,
}

async fn funded_fixture() -> WalletFixture {
    let url = std::env::var("RPC_URL").expect("RPC_URL must identify a disposable local chain");
    let artifact =
        std::env::var("NATIVE_WALLET_COUNTER_RV32A").expect("Counter 0.1.0 artifact path required");
    let client = NativeClient::new(HttpClientBuilder::default().build(url).unwrap());
    let version = known(client.get_version_info(EmptyRequest {}).await);
    assert_eq!(version.wire_format.split('.').next(), Some("14"));
    for method in [
        "getNetworkInfo",
        "getChainTip",
        "getBalance",
        "faucet_requestAirdrop",
        "sendTransaction",
        "getTransactionStatus",
        "getTransactionReceipt",
    ] {
        assert!(
            version.supported_methods.iter().any(|name| name == method),
            "missing {method}"
        );
    }
    let network = NetworkHash::new(
        known(client.get_network_info(EmptyRequest {}).await)
            .network_hash
            .0,
    );
    let manifest: thruput_json_rpc_types::runtime_manifest::RuntimeManifest =
        serde_json::from_slice(
            &std::fs::read(
                std::env::var("NATIVE_WALLET_RUNTIME_MANIFEST")
                    .expect("local runtime manifest required"),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(manifest.network_hash.to_network_hash(), network);
    let mut secret = [0; 32];
    getrandom::fill(&mut secret).unwrap();
    let signer = LocalSigner::from_secret_key(secret);
    let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    assert_eq!(balance(&client, account.address(), None).await, 0);
    let request = AirdropRequest {
        recipient: Base58(account.address()),
        client_nonce: ClientNonce::new(vec![0x37]).unwrap(),
    };
    let funding = known(client.request_airdrop(request).await);
    let funding_hash = match funding {
        AirdropAttempt::Pending {
            recipient,
            transaction_hash: Some(hash),
            ..
        }
        | AirdropAttempt::Succeeded {
            recipient,
            transaction_hash: hash,
            ..
        } => {
            assert_eq!(recipient.0, account.address());
            hash
        }
        other => panic!("funding cannot be observed without a returned hash: {other:?}"),
    };
    let funding_block_height = success(&client, &network, funding_hash).await.block_height;
    let contract = Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(std::fs::read(artifact).unwrap()),
        },
    };
    WalletFixture {
        client,
        network,
        manifest,
        signer,
        account,
        contract,
        funding_block_height,
    }
}

/// Verifies the indexed conflict remains visible alongside the scheduler retry.
#[tokio::test]
#[ignore = "requires a disposable runtime exposing the advisory unfunded-deployment diagnostic"]
async fn unfunded_deployment_reports_target_storage_conflict() {
    let WalletFixture {
        client,
        network,
        signer,
        account,
        contract,
        ..
    } = funded_fixture().await;
    let hash = wallet_contract_hash(&contract).unwrap();
    let unfunded_salt = SHA256Hash::from([1; 32]);
    let unfunded_target = derive_deploy_address(&account.address(), &unfunded_salt, &hash).unwrap();
    let negative = build_default_wallet_deploy_transaction(BuildDefaultWalletDeployTransaction {
        account: &account,
        deployment_nonce: unfunded_salt,
        code: thruput_signing::default_wallet::DeploymentCode::Attached(contract.clone()),
        amount: 0,
        initializer: None,
        gas_limit: 1_000_000,
        block_expiry_height: expiry(&client).await,
        nonce: 1,
    })
    .unwrap();
    let negative_hash = submit(&client, &signer, &network, negative).await;
    let mut saw_funding_failure = false;
    let mut last_status = None;
    for _ in 0..120 {
        let status = known(
            client
                .get_transaction_status(TransactionQuery {
                    transaction_hash: negative_hash,
                })
                .await,
        );
        last_status = Some(status.clone());
        match status {
            TransactionStatus::Retry {
                error,
                recent_failure:
                    Some(RecentAttemptFailure {
                        cause: FailureCause::DbConflict { conflict },
                        ..
                    }),
            } if conflict.address.0 == unfunded_target
                && matches!(
                    conflict.code,
                    ConflictCode::BoundsExceeded | ConflictCode::InsufficientFunds
                ) =>
            {
                assert!(matches!(error.source, SchedulerErrorSource::Executor));
                assert_eq!(
                    error.code,
                    thruput_xdr::exec_result::ExecResult::ExecResultTxNonDeterministicError.0
                );
                eprintln!("unfunded deployment diagnostic: {last_status:?}");
                saw_funding_failure = true;
                break;
            }
            TransactionStatus::Committed { .. } => panic!("unfunded deploy unexpectedly committed"),
            TransactionStatus::Dropped { error, .. } => panic!("unexpected rejection: {error:?}"),
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    assert!(
        saw_funding_failure,
        "expected target storage funding failure; last status: {last_status:?}"
    );
}

async fn qualify_native_deployment(atomic: bool) {
    // Each positive flow has a fresh account; no uncertain negative attempt can interfere.
    let WalletFixture {
        client,
        network,
        manifest,
        signer,
        account,
        contract,
        funding_block_height,
    } = funded_fixture().await;
    let costs = &manifest.bootstrap.storage_cost_model;
    let balance_cost: u128 = costs.balance_cost.parse().unwrap();
    let replay_cost = costs.base_data_item_cost.parse::<u128>().unwrap()
        + costs.replay_cache_cost_per_entry.parse::<u128>().unwrap();
    let hash = wallet_contract_hash(&contract).unwrap();
    let salt = SHA256Hash::from([2; 32]);
    let target = derive_deploy_address(&account.address(), &salt, &hash).unwrap();
    let before = balance(&client, account.address(), Some(&funding_block_height)).await;
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
            block_expiry_height: expiry(&client).await,
            nonce: 2,
        })
        .unwrap();
        let receipt = success(
            &client,
            &network,
            submit(&client, &signer, &network, transfer).await,
        )
        .await;
        assert!(
            decode_default_wallet_call_return(returned(&receipt))
                .unwrap()
                .is_empty()
        );
        // getBalance reports free HOG, after storage collateral is reserved. The
        // sender creates a replay entry; the fresh recipient creates a balance item.
        assert_eq!(
            balance(&client, account.address(), Some(&receipt.block_height)).await,
            before - amount - replay_cost
        );
        assert_eq!(
            balance(&client, target, Some(&receipt.block_height)).await,
            amount - balance_cost
        );
    }
    let deployment = BuildDefaultWalletDeployTransaction {
        account: &account,
        deployment_nonce: salt,
        code: thruput_signing::default_wallet::DeploymentCode::Attached(contract),
        amount: if atomic { amount } else { 0 },
        initializer: None,
        gas_limit: 1_000_000,
        block_expiry_height: expiry(&client).await,
        nonce: 3,
    };
    let deploy = build_default_wallet_deploy_transaction(deployment).unwrap();
    let receipt = success(
        &client,
        &network,
        submit(&client, &signer, &network, deploy).await,
    )
    .await;
    let deployed: ContractAddress = xdrs::cxs::decode(returned(&receipt)).unwrap();
    assert_eq!(deployed, target);
    if atomic {
        assert_eq!(
            balance(&client, account.address(), Some(&receipt.block_height)).await,
            before - amount - replay_cost
        );
        let deployment_cost = costs.base_contract_deployment_cost.parse::<u128>().unwrap()
            + code_size
                * costs
                    .contract_deployment_cost_per_byte
                    .parse::<u128>()
                    .unwrap();
        assert_eq!(
            balance(&client, target, Some(&receipt.block_height)).await,
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
            block_expiry_height: expiry(&client).await,
            nonce,
        })
        .unwrap();
        let receipt = success(
            &client,
            &network,
            submit(&client, &signer, &network, call).await,
        )
        .await;
        let returned = decode_default_wallet_call_return(returned(&receipt)).unwrap();
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
