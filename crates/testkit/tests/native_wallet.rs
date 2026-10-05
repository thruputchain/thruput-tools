//! Public testkit API checks against a recording RPC, including ambiguous outcomes.
use std::sync::{Arc, Mutex};

use groundhog_signing::default_wallet::{
    DEFAULT_WALLET_DEPLOY_ENTRYPOINT, DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
};
use groundhog_signing::{
    PubkeyAccount, derive_deploy_address, derive_dev_account_signer, transaction_hash,
};
use groundhog_testkit::rpc_types::{
    hex::{Hex0x, HexBytes},
    receipt::{ExecStatus, TransactionReceipt},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_testkit::xdr::{
    contract::ContractEntrypointID, transaction::SignedTransaction, types::SHA256Hash,
};
use groundhog_testkit::{
    Agent, ArtifactKind, Contract, DeployOptions, NetworkConfig, Scenario, TestkitError,
    WalletInitCall,
};
use jsonrpsee::{
    RpcModule,
    server::{ServerBuilder, ServerHandle},
    types::ErrorObjectOwned,
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
}
struct State {
    outcome: Outcome,
    submitted: Mutex<Vec<SignedTransaction>>,
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
            outcome,
            submitted: Mutex::new(vec![]),
        });
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = RpcModule::new(state.clone());
        module
            .register_method("groundhog_sendRawTransaction", |params, state, _| {
                let (encoded,): (String,) = params.parse()?;
                let signed: SignedTransaction =
                    xdrs::txs::decode(&hex::decode(encoded.trim_start_matches("0x")).unwrap())
                        .unwrap();
                let hash = TxHash(Hex0x(transaction_hash(&signed.tx).unwrap()));
                state.submitted.lock().unwrap().push(signed);
                match state.outcome {
                    Outcome::SendError => Err(ErrorObjectOwned::owned(
                        -32000,
                        "ambiguous submission",
                        None::<()>,
                    )),
                    Outcome::SendHash => Ok(TxHash(Hex0x([9; 32].into()))),
                    _ => Ok(hash),
                }
            })
            .unwrap();
        module
            .register_method("groundhog_getTransactionStatus", |_, _, _| {
                Ok::<_, ErrorObjectOwned>(TransactionStatus::Unknown)
            })
            .unwrap();
        module
            .register_method("groundhog_getTransactionReceipt", |params, state, _| {
                let (hash,): (TxHash,) = params.parse()?;
                if matches!(state.outcome, Outcome::SendError | Outcome::Pending) {
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
                    match state.outcome {
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
                    tx_hash: if matches!(state.outcome, Outcome::ReceiptHash) {
                        TxHash(Hex0x([9; 32].into()))
                    } else {
                        hash
                    },
                    height: 3.into(),
                    status: ExecStatus::Ok,
                    events: vec![],
                    return_data,
                    abort_data: None,
                    gas_used: 123,
                }))
            })
            .unwrap();
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
    let mut document: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/gateway/examples/runtime-manifest.hogd-dev.example.json"
    ))
    .unwrap();
    for index in 0..2 {
        let signer = derive_dev_account_signer("groundhog-dev-accounts", index);
        document["bootstrapAccounts"][index]["publicKey"] =
            format!("0x{}", hex::encode(signer.verifying_key_bytes())).into();
        document["bootstrapAccounts"][index]["walletAddress"] = format!(
            "0x{}",
            hex::encode(
                PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0)
                    .address()
                    .as_ref()
            )
        )
        .into();
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
                let signer = derive_dev_account_signer("groundhog-testkit:native-test", 0);
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
        .run_agents([Agent::new("alice", |mut ctx| async move {
            let signer = derive_dev_account_signer("groundhog-testkit:native-test", 0);
            assert_eq!(
                ctx.address(),
                PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()
            );
            ctx.fund(12345).await?;
            let contract = Contract::new([3; 32].into(), "Counter", "0.1.0");
            assert_eq!(
                contract
                    .call_returning::<_, u64>(&mut ctx, "get", &())
                    .await?,
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
            .run_agents([Agent::new("alice", |mut ctx| async move {
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
            .run_agents([Agent::new("alice", |mut ctx| async move {
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
