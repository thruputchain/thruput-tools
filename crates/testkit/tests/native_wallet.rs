//! Public testkit API checks against a recording RPC, including ambiguous outcomes.
use std::sync::{Arc, Mutex};

use jsonrpsee::{
    RpcModule,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
};
use thruput_signing::default_wallet::{
    DEFAULT_WALLET_DEPLOY_ENTRYPOINT, DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
};
use thruput_signing::{
    PubkeyAccount, derive_deploy_address, derive_dev_account_signer, transaction_hash,
};
use thruput_testkit::rpc_types::{
    base58::Base58,
    hex::HexBytes,
    responses::{
        Admission, ReceiptExecution, SchedulerError, SchedulerErrorSource, SendTransactionResult,
        TransactionReceipt, TransactionStatus,
    },
    transaction::TxHash,
};
use thruput_testkit::xdr::{
    contract::ContractEntrypointID, transaction::SignedTransaction, types::SHA256Hash,
};
use thruput_testkit::{
    Agent, ArtifactKind, Contract, DeployOptions, NetworkConfig, Scenario, TestkitError,
    WalletInitCall,
};

#[derive(Clone, Copy)]
enum Outcome {
    Success,
    SendError,
    SendHash,
    ReceiptHash,
    WrongAddress,
    MissingAddress,
    Pending,
    Retry,
    Dropped,
    CommittedThenStale,
    CorruptCommitted,
}
struct State {
    outcome: Mutex<Outcome>,
    submitted: Mutex<Vec<SignedTransaction>>,
    receipt_reads: std::sync::atomic::AtomicUsize,
    status_reads: std::sync::atomic::AtomicUsize,
}
struct Fixture {
    state: Arc<State>,
    server: ServerHandle,
    url: String,
    manifest: tempfile::NamedTempFile,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.server.stop();
    }
}
type DeployCalldata = (
    SHA256Hash,
    SHA256Hash,
    u128,
    Option<(ContractEntrypointID, u64, Vec<u8>)>,
);
impl Fixture {
    async fn new(outcome: Outcome) -> Self {
        let state = Arc::new(State {
            outcome: Mutex::new(outcome),
            submitted: Mutex::new(vec![]),
            receipt_reads: std::sync::atomic::AtomicUsize::new(0),
            status_reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = RpcModule::new(state.clone());
        serve_manifest_network_info(&mut module);
        serve_submission(&mut module);
        serve_receipts(&mut module);
        let server = server.start(module);
        let manifest = tempfile::NamedTempFile::new().unwrap();
        let document = native_runtime_document();
        std::fs::write(manifest.path(), serde_json::to_vec(&document).unwrap()).unwrap();
        Self {
            state,
            server,
            url,
            manifest,
        }
    }
    async fn scenario(&self, bootstrap: bool) -> Scenario {
        let mut builder = Scenario::builder("native-test")
            .network(NetworkConfig::existing(&self.url, self.manifest.path()))
            .artifact_bytes("counter", [1, 2, 3], ArtifactKind::Rv32);
        if bootstrap {
            builder = builder.bootstrap_accounts();
        }
        builder.build().await.unwrap()
    }
}

fn native_runtime_document() -> serde_json::Value {
    let mut document: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/hosted-runtime.json")).unwrap();
    document["kind"] = "hosted-devnet-runtime".into();
    for index in 0..2 {
        let signer = derive_dev_account_signer("groundhog-dev-accounts", index);
        document["bootstrapAccounts"][index]["publicKey"] =
            serde_json::to_value(thruput_json_rpc_types::Ed25519PublicKey::from(
                thruput_xdr::types::Ed25519PublicKey::from(signer.verifying_key_bytes()),
            ))
            .unwrap();
        document["bootstrapAccounts"][index]["walletAddress"] =
            serde_json::to_value(thruput_json_rpc_types::ContractAddress::from(
                PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address(),
            ))
            .unwrap();
    }
    document
}

#[tokio::test]
async fn repeated_scenarios_fund_the_same_account_with_distinct_transactions() {
    let fixture = Fixture::new(Outcome::Success).await;
    for _ in 0..2 {
        let scenario = fixture.scenario(false).await;
        scenario
            .run_agents([Agent::new("alice", |ctx| async move {
                let signer = derive_dev_account_signer("thruput-testkit:native-test", 0);
                assert_eq!(
                    ctx.address(),
                    PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()
                );
                for _ in 0..2 {
                    ctx.fund(12345).await?;
                }
                Ok(())
            })])
            .await
            .unwrap();
    }

    let submitted = fixture.state.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 4);
    for (index, signed) in submitted.iter().enumerate() {
        assert_eq!(signed.tx.invocation, submitted[0].tx.invocation);
        for prior in &submitted[..index] {
            assert_ne!(signed.tx.uniquifier.nonce, prior.tx.uniquifier.nonce);
            assert_ne!(
                transaction_hash(&signed.tx).unwrap(),
                transaction_hash(&prior.tx).unwrap()
            );
        }
    }
}

#[tokio::test]
async fn ambiguous_funding_retains_the_original_transaction_without_retrying() {
    for outcome in [Outcome::SendError, Outcome::SendHash] {
        let fixture = Fixture::new(outcome).await;
        let scenario = fixture.scenario(false).await;
        let state = fixture.state.clone();
        scenario
            .run_agents([Agent::new("alice", move |ctx| async move {
                let error = ctx.submit_fund(12345).await.unwrap_err();
                let TestkitError::Submission {
                    tx_hash,
                    signed_transaction,
                    ..
                } = error
                else {
                    panic!("expected a retained funding submission")
                };
                let signed: SignedTransaction = xdrs::txs::decode(
                    &hex::decode(signed_transaction.trim_start_matches("0x")).unwrap(),
                )
                .unwrap();
                assert_eq!(tx_hash.0.0, transaction_hash(&signed.tx).unwrap());
                let submitted = state.submitted.lock().unwrap();
                assert_eq!(submitted.len(), 1);
                assert_eq!(signed, submitted[0]);
                Ok(())
            })])
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn native_accounts_are_empty_until_explicit_funding_and_calls_unwrap_returns() {
    let fixture = Fixture::new(Outcome::Success).await;
    let scenario = fixture.scenario(false).await;
    assert!(fixture.state.submitted.lock().unwrap().is_empty());
    scenario
        .run_agents([Agent::new("alice", |ctx| async move {
            let signer = derive_dev_account_signer("thruput-testkit:native-test", 0);
            assert_eq!(
                ctx.address(),
                PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()
            );
            ctx.fund(12345).await?;
            let contract = Contract::new([3; 32].into(), "Counter", "0.1.0");
            assert_eq!(
                contract.call_returning::<_, u64>(&ctx, "get", &()).await?,
                42
            );
            Ok(())
        })])
        .await
        .unwrap();
    let submitted = fixture.state.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 2);
    assert_eq!(
        submitted[0].tx.invocation.invoked_address.as_ref(),
        PubkeyAccount::from_public_key_bytes(
            derive_dev_account_signer("groundhog-dev-accounts", 0).verifying_key_bytes(),
            0
        )
        .address()
        .as_ref()
    );
    assert_eq!(
        submitted[1].tx.invocation.entrypoint,
        DEFAULT_WALLET_EXECUTE_ENTRYPOINT
    );
}

#[tokio::test]
async fn deployment_has_exact_effects_and_registers_once() {
    let fixture = Fixture::new(Outcome::Success).await;
    let scenario = fixture.scenario(false).await;
    scenario
        .run_agents([Agent::new("alice", |mut ctx| async move {
            let options = DeployOptions {
                amount: 123,
                initializer: Some(WalletInitCall {
                    entrypoint: [7; 8].into(),
                    gas_limit: 456,
                    calldata: &[8, 9],
                }),
            };
            let first = ctx.deploy_artifact("counter", options).await?;
            assert!(first.registered);
            let second = ctx.deploy_registered("counter", options).await?;
            assert!(!second.registered);
            assert_ne!(first.address, second.address);
            Ok(())
        })])
        .await
        .unwrap();
    let submitted = fixture.state.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 2);
    assert_eq!(submitted[0].tx.contracts_to_deploy.len(), 1);
    assert!(submitted[1].tx.contracts_to_deploy.is_empty());
    for signed in submitted.iter() {
        let (_, _, amount, init): DeployCalldata =
            xdrs::cxs::decode(&signed.tx.invocation.calldata).unwrap();
        assert_eq!(amount, 123);
        assert_eq!(init, Some(([7; 8].into(), 456, vec![8, 9])));
    }
}

#[tokio::test]
async fn funded_bootstrap_accounts_deploy_through_native_wallet() {
    let fixture = Fixture::new(Outcome::Success).await;
    let scenario = fixture.scenario(true).await;
    scenario
        .run_agents([Agent::new("alice", |mut ctx| async move {
            let signer = derive_dev_account_signer("groundhog-dev-accounts", 1);
            assert_eq!(
                ctx.address(),
                PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()
            );
            ctx.deploy_artifact("counter", DeployOptions::funded(0))
                .await?;
            Ok(())
        })])
        .await
        .unwrap();
    let submitted = fixture.state.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 1);
    assert_eq!(
        submitted[0].tx.invocation.entrypoint,
        DEFAULT_WALLET_DEPLOY_ENTRYPOINT
    );
}

#[tokio::test]
async fn ambiguous_submissions_keep_signed_identity_and_reserve_nonce() {
    for outcome in [Outcome::SendError, Outcome::SendHash] {
        let fixture = Fixture::new(outcome).await;
        let scenario = fixture.scenario(false).await;
        scenario
            .run_agents([Agent::new("alice", |ctx| async move {
                for _ in 0..2 {
                    let error = ctx
                        .submit_deploy(&[1, 2, 3], ArtifactKind::Rv32, DeployOptions::funded(0))
                        .await
                        .unwrap_err();
                    let TestkitError::Submission {
                        tx_hash,
                        signed_transaction,
                        ..
                    } = error
                    else {
                        panic!("expected recovery error")
                    };
                    let signed: SignedTransaction = xdrs::txs::decode(
                        &hex::decode(signed_transaction.trim_start_matches("0x")).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(tx_hash.0.0, transaction_hash(&signed.tx).unwrap());
                }
                Ok(())
            })])
            .await
            .unwrap();
        let submitted = fixture.state.submitted.lock().unwrap();
        assert_eq!(submitted.len(), 2);
        assert_ne!(
            submitted[0].tx.uniquifier.nonce,
            submitted[1].tx.uniquifier.nonce
        );
    }
}

#[tokio::test]
async fn registry_never_accepts_unverified_receipts_or_resubmits_pending_attempts() {
    for outcome in [
        Outcome::ReceiptHash,
        Outcome::WrongAddress,
        Outcome::MissingAddress,
        Outcome::Pending,
    ] {
        let fixture = Fixture::new(outcome).await;
        let scenario = fixture.scenario(false).await;
        scenario
            .run_agents([Agent::new("alice", move |mut ctx| async move {
                let submitted = ctx
                    .submit_deploy_artifact("counter", DeployOptions::funded(123))
                    .await?;
                if !matches!(outcome, Outcome::Pending) {
                    assert!(submitted.await_receipt().await.is_err());
                }
                assert!(!ctx.is_registered("counter")?);
                let changed = ctx
                    .submit_deploy_artifact("counter", DeployOptions::funded(124))
                    .await
                    .unwrap_err();
                assert!(matches!(
                    changed,
                    TestkitError::DeploymentRequestMismatch { .. }
                ));
                assert!(
                    ctx.submit_deploy_artifact("counter", DeployOptions::funded(123))
                        .await
                        .is_err()
                );
                Ok(())
            })])
            .await
            .unwrap();
        assert_eq!(fixture.state.submitted.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn high_level_raw_deploy_preserves_unverified_attempt() {
    for outcome in [
        Outcome::WrongAddress,
        Outcome::MissingAddress,
        Outcome::ReceiptHash,
    ] {
        let fixture = Fixture::new(outcome).await;
        let scenario = fixture.scenario(false).await;
        scenario
            .run_agents([Agent::new("alice", |ctx| async move {
                let error = ctx
                    .deploy(&[1, 2, 3], ArtifactKind::Rv32, DeployOptions::funded(123))
                    .await
                    .unwrap_err();
                let TestkitError::UnverifiedDeployment {
                    tx_hash,
                    predicted_address,
                    signed_transaction,
                    ..
                } = error
                else {
                    panic!("expected recoverable deployment failure")
                };
                let signed: SignedTransaction = xdrs::txs::decode(
                    &hex::decode(signed_transaction.trim_start_matches("0x")).unwrap(),
                )
                .unwrap();
                assert_eq!(tx_hash.0.0, transaction_hash(&signed.tx).unwrap());
                let (nonce, code_hash, amount, _): DeployCalldata =
                    xdrs::cxs::decode(&signed.tx.invocation.calldata).unwrap();
                assert_eq!(amount, 123);
                assert_eq!(
                    predicted_address,
                    derive_deploy_address(&ctx.address(), &nonce, &code_hash).unwrap()
                );
                Ok(())
            })])
            .await
            .unwrap();
        assert_eq!(fixture.state.submitted.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn registration_retry_keeps_one_attempt_until_a_receipt_finishes_recovery() {
    let fixture = Fixture::new(Outcome::Retry).await;
    let state = fixture.state.clone();
    fixture
        .scenario(false)
        .await
        .run_agents([Agent::new("alice", move |mut ctx| async move {
            let first = ctx
                .submit_deploy_artifact("counter", DeployOptions::funded(123))
                .await?;
            for _ in 0..2 {
                let error = ctx.submit_deploy_artifact("counter", DeployOptions::funded(123)).await.unwrap_err();
                assert!(matches!(error, TestkitError::ArtifactRegistrationPending { tx_hash, signed_transaction, .. } if tx_hash == first.tx_hash() && signed_transaction == first.signed_transaction()));
                assert_eq!(state.submitted.lock().unwrap().len(), 1);
            }
            *state.outcome.lock().unwrap() = Outcome::Success;
            let recovered = ctx
                .submit_deploy_artifact("counter", DeployOptions::funded(123))
                .await?;
            assert_eq!(recovered.tx_hash(), first.tx_hash());
            recovered.await_receipt().await?;
            assert!(ctx.is_registered("counter")?);
            Ok(())
        })])
        .await
        .unwrap();
    let submitted = fixture.state.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 1);
}

#[tokio::test]
async fn dropped_registration_keeps_original_attempt_and_blocks_replacement() {
    let fixture = Fixture::new(Outcome::Dropped).await;
    let state = fixture.state.clone();
    fixture.scenario(false).await.run_agents([Agent::new("alice", move |mut ctx| async move {
        let first = ctx.submit_register_artifact("counter", DeployOptions::funded(123)).await?;
        let error = ctx.submit_register_artifact("counter", DeployOptions::funded(123)).await.unwrap_err();
        assert!(matches!(error, TestkitError::ArtifactRegistrationPending { tx_hash, signed_transaction, .. } if tx_hash == first.tx_hash() && signed_transaction == first.signed_transaction()));
        assert!(matches!(ctx.submit_register_artifact("counter", DeployOptions::funded(124)).await, Err(TestkitError::DeploymentRequestMismatch { .. })));
        *state.outcome.lock().unwrap() = Outcome::Success;
        let recovered = ctx.submit_register_artifact("counter", DeployOptions::funded(123)).await?;
        assert_eq!(recovered.tx_hash(), first.tx_hash());
        recovered.await_receipt().await?;
        Ok(())
    })]).await.unwrap();
    assert_eq!(fixture.state.submitted.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_later_run_observes_registration_without_replaying_the_original_deployment() {
    let fixture = Fixture::new(Outcome::Retry).await;
    let scenario = fixture.scenario(false).await;
    scenario
        .run_agents([Agent::new("alice", |mut ctx| async move {
            let _submitted = ctx
                .submit_deploy_artifact("counter", DeployOptions::funded(123))
                .await?;
            Ok(())
        })])
        .await
        .unwrap();
    let state = fixture.state.clone();
    scenario
        .run_agents([Agent::new("alice", move |mut ctx| async move {
            let error = ctx
                .submit_deploy_artifact("counter", DeployOptions::funded(124))
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                TestkitError::ArtifactRegistrationPending { .. }
            ));
            *state.outcome.lock().unwrap() = Outcome::Success;
            let deployed = ctx
                .deploy_artifact("counter", DeployOptions::funded(124))
                .await?;
            assert!(!deployed.registered);
            Ok(())
        })])
        .await
        .unwrap();
    let submitted = fixture.state.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 2);
    assert_ne!(
        transaction_hash(&submitted[0].tx).unwrap(),
        transaction_hash(&submitted[1].tx).unwrap()
    );
    assert!(submitted[1].tx.contracts_to_deploy.is_empty());
}

/// Serve the network identity named by the runtime manifest fixture (a zero `networkHash`).
fn serve_manifest_network_info<T: Send + Sync + 'static>(module: &mut RpcModule<T>) {
    module.register_method("getVersionInfo", |_, _, _| Ok::<_, ErrorObjectOwned>(serde_json::json!({
        "wireFormat": "14.0.0", "softwareVersion": "test", "implementation": "test",
        "supportedMethods": ["getNetworkInfo", "sendTransaction", "getTransactionStatus", "getTransactionReceipt", "dev_fundAccount"]
    }))).unwrap();

    module
        .register_method("getNetworkInfo", |_, _, _| {
            Ok::<_, ErrorObjectOwned>(thruput_json_rpc_types::responses::NetworkInfo {
                network_hash: thruput_json_rpc_types::NetworkHash::from(
                    thruput_xdr::types::SHA256Hash::from([0; 32]),
                ),
            })
        })
        .unwrap();
}

#[tokio::test]
async fn committed_then_retry_and_dropped_statuses_do_not_end_receipt_wait_or_resubmit() {
    let fixture = Fixture::new(Outcome::CommittedThenStale).await;
    let state = fixture.state.clone();
    fixture
        .scenario(false)
        .await
        .run_agents([Agent::new("alice", move |mut ctx| async move {
            let submitted = ctx
                .submit_deploy_artifact("counter", DeployOptions::funded(123))
                .await?;
            let hash = submitted.tx_hash();
            let deployed = submitted.await_receipt().await?;
            assert_eq!(deployed.receipt.transaction_hash, hash);
            assert_eq!(*deployed.receipt.block_height, 3);
            assert_eq!(
                state.status_reads.load(std::sync::atomic::Ordering::SeqCst),
                3
            );
            assert_eq!(state.submitted.lock().unwrap().len(), 1);
            Ok(())
        })])
        .await
        .unwrap();
}

fn serve_submission(module: &mut RpcModule<Arc<State>>) {
    module
        .register_method("sendTransaction", |params, state, _| {
            let request: thruput_json_rpc_types::requests::SendTransactionRequest =
                params.parse()?;
            let encoded = thruput_json_rpc_types::hex::prefixed_hex(&request.signed_transaction.0);
            let signed: SignedTransaction =
                xdrs::txs::decode(&hex::decode(encoded.trim_start_matches("0x")).unwrap()).unwrap();
            let hash = TxHash(Base58(transaction_hash(&signed.tx).unwrap()));
            state.submitted.lock().unwrap().push(signed);
            match *state.outcome.lock().unwrap() {
                Outcome::SendError => Err(ErrorObjectOwned::owned(
                    -32000,
                    "ambiguous submission",
                    None::<()>,
                )),
                Outcome::SendHash => Ok(SendTransactionResult {
                    transaction_hash: TxHash(Base58([9; 32].into())),
                    admission: Admission::Accepted,
                }),
                _ => Ok(SendTransactionResult {
                    transaction_hash: hash,
                    admission: Admission::Accepted,
                }),
            }
        })
        .unwrap();
    module
        .register_method("getTransactionStatus", |_, state, _| {
            let error = SchedulerError {
                source: SchedulerErrorSource::Executor,
                code: 7,
            };
            Ok::<_, ErrorObjectOwned>(match *state.outcome.lock().unwrap() {
                Outcome::CommittedThenStale => match state
                    .status_reads
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                {
                    0 => TransactionStatus::Committed {
                        block_height: 3.into(),
                    },
                    1 => TransactionStatus::Retry { error },
                    _ => TransactionStatus::Dropped { error },
                },
                Outcome::Retry => TransactionStatus::Retry { error },
                Outcome::Dropped => TransactionStatus::Dropped { error },
                _ => TransactionStatus::Unknown {},
            })
        })
        .unwrap();
}

fn serve_receipts(module: &mut RpcModule<Arc<State>>) {
    module
        .register_method("getTransactionReceipt", |params, state, _| {
            let hash = params
                .parse::<thruput_json_rpc_types::requests::TransactionQuery>()?
                .transaction_hash;
            if matches!(*state.outcome.lock().unwrap(), Outcome::CorruptCommitted) {
                return Err(ErrorObjectOwned::owned(
                    thruput_json_rpc_types::error::native::codes::BACKEND_DATA_INVALID,
                    "corrupt committed receipt",
                    Some(serde_json::json!({"kind":"backendDataInvalid","reason":"inconsistentResult","observation":{"transactionHash":hash,"blockHeight":"3"}})),
                ));
            }
            if matches!(*state.outcome.lock().unwrap(), Outcome::CommittedThenStale)
                && state
                    .receipt_reads
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    < 3
            {
                return Ok::<_, ErrorObjectOwned>(None);
            }
            if matches!(
                *state.outcome.lock().unwrap(),
                Outcome::SendError | Outcome::Pending | Outcome::Retry | Outcome::Dropped
            ) {
                return Ok::<_, ErrorObjectOwned>(None);
            }
            let submitted = state.submitted.lock().unwrap();
            let signed = submitted
                .iter()
                .find(|signed| transaction_hash(&signed.tx).unwrap() == hash.0.0)
                .unwrap();
            let tx = &signed.tx;
            let return_data = if tx.invocation.entrypoint == DEFAULT_WALLET_DEPLOY_ENTRYPOINT {
                let (nonce, code_hash, _, _): DeployCalldata =
                    xdrs::cxs::decode(&tx.invocation.calldata).unwrap();
                let address =
                    derive_deploy_address(&tx.invocation.invoked_address, &nonce, &code_hash)
                        .unwrap();
                match *state.outcome.lock().unwrap() {
                    Outcome::MissingAddress => None,
                    Outcome::WrongAddress => Some(HexBytes(vec![8; 32])),
                    _ => Some(HexBytes(address.as_ref().to_vec())),
                }
            } else {
                Some(HexBytes(
                    xdrs::cxs::encode(&vec![xdrs::cxs::encode(&42_u64).unwrap()]).unwrap(),
                ))
            };
            Ok(Some(TransactionReceipt {
                transaction_hash: if matches!(*state.outcome.lock().unwrap(), Outcome::ReceiptHash)
                {
                    TxHash(Base58([9; 32].into()))
                } else {
                    hash
                },
                block_height: 3.into(),
                execution: ReceiptExecution::Return {
                    return_data: return_data.unwrap_or_else(|| HexBytes(vec![])),
                },
                events: vec![],
                gas_used: 123,
            }))
        })
        .unwrap();
}

#[tokio::test]
async fn corrupt_committed_receipt_preserves_observation_through_deployment_error() {
    let fixture = Fixture::new(Outcome::CorruptCommitted).await;
    fixture
        .scenario(false)
        .await
        .run_agents([Agent::new("alice", |mut ctx| async move {
            let submitted = ctx
                .submit_deploy_artifact("counter", DeployOptions::funded(123))
                .await?;
            let hash = submitted.tx_hash();
            let error = submitted.await_receipt().await.unwrap_err();
            let TestkitError::UnverifiedDeployment {
                source: Some(source),
                ..
            } = error
            else {
                panic!("missing deployment evidence")
            };
            let TestkitError::ReceiptWait { source } = *source else {
                panic!("missing receipt evidence")
            };
            let commitment = source
                .evidence
                .commitment
                .as_ref()
                .expect("matched commitment retained");
            assert_eq!(commitment.transaction_hash, hash);
            assert_eq!(*commitment.block_height, 3);
            assert!(!ctx.is_registered("counter")?);
            Ok(())
        })])
        .await
        .unwrap();
    assert_eq!(fixture.state.submitted.lock().unwrap().len(), 1);
}
