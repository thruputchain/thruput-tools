//! Local public funding retains request identity and returns before block production.
use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use thruput_json_rpc_types::{
    base58::Base58,
    hex::HexBytes,
    requests::FundAccountRequest,
    responses::{
        Admission, FundingSubmission, ReceiptExecution, SendTransactionResult, TransactionReceipt,
    },
    transaction::TxHash,
};
use thruput_signing::{
    LocalSigner, PubkeyAccount, SignTransactionParams, Signer, sign_and_encode_transaction_hex,
    transaction_hash,
};
use thruput_testkit::{Agent, NetworkConfig, Scenario, TestkitError};

mod common;
use common::serve_manifest_network_info;

#[tokio::test]
async fn lost_response_reuses_request_and_returns_admitted_bytes_without_receipt_wait() {
    let faucet = LocalSigner::from_secret_key([42; 32]);
    let faucet_public_key = faucet.verifying_key_bytes();
    let faucet_address = PubkeyAccount::new(faucet.public_key(), 0).address();
    let requests = Arc::new(Mutex::new(Vec::<FundAccountRequest>::new()));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(requests.clone());
    serve_uncertain_funding(&mut module, faucet);
    let receipt_ready = Arc::new(AtomicBool::new(false));
    let receipt_flag = receipt_ready.clone();
    module
        .register_method("getTransactionReceipt", move |params, _, _| {
            let hash = params
                .parse::<thruput_json_rpc_types::requests::TransactionQuery>()?
                .transaction_hash;
            if !receipt_flag.load(Ordering::SeqCst) {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    "lost receipt response",
                    None::<()>,
                ));
            }
            Ok(Some(successful_receipt(hash)))
        })
        .unwrap();
    serve_manifest_network_info(&mut module);
    let handle = server.start(module);
    let manifest = local_manifest(faucet_public_key, faucet_address);
    let network_config = NetworkConfig::existing(&url, manifest.path());
    let legacy = Scenario::builder("local-test")
        .network(network_config.clone())
        .bootstrap_accounts()
        .build()
        .await;
    assert!(matches!(legacy, Err(TestkitError::Manifest { .. })));
    let scenario = Scenario::builder("local-test")
        .accounts(1)
        .network(network_config)
        .build()
        .await
        .unwrap();
    scenario
        .run_agents([Agent::new("alice", move |ctx| async move {
            let first = ctx.submit_fund(12345).await.unwrap_err();
            let TestkitError::Submission { tx_hash, signed_transaction, .. } = first else {
                panic!("expected typed unresolved transaction evidence");
            };
            assert!(signed_transaction.starts_with("0x"));
            let second = ctx.submit_fund(12345).await.unwrap_err();
            assert!(matches!(second, TestkitError::Submission { tx_hash: retry_hash, signed_transaction: retry_bytes, .. }
                if retry_hash == tx_hash && retry_bytes == signed_transaction));
            assert!(matches!(
                ctx.submit_fund(67890).await,
                Err(TestkitError::FundingRequestMismatch { .. })
            ));
            assert!(ctx.fund(12345).await.is_err()); // Admitted, but receipt lookup fails.
            let admitted = ctx.submit_fund(12345).await?;
            assert!(admitted.signed_transaction().starts_with("0x"));
            // Admission returned despite unavailable receipts. Manual callers produce a block next.
            receipt_ready.store(true, Ordering::SeqCst);
            admitted.await_receipt().await?;
            let next = ctx.submit_fund(67890).await?;
            assert_ne!(next.tx_hash(), admitted.tx_hash());
            Ok(())
        })])
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 4);
    {
        let seen = requests.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&seen[0]).unwrap(),
            serde_json::to_value(&seen[1]).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&seen[1]).unwrap(),
            serde_json::to_value(&seen[2]).unwrap()
        );
        assert_ne!(seen[2].client_nonce, seen[3].client_nonce);
    }
    scenario.stop().await.unwrap();
    handle.stop().unwrap();
}

#[tokio::test]
async fn evidence_only_attempt_can_be_reconciled_by_explicit_raw_submission() {
    let faucet = LocalSigner::from_secret_key([42; 32]);
    let faucet_public_key = faucet.verifying_key_bytes();
    let faucet_address = PubkeyAccount::new(faucet.public_key(), 0).address();
    let requests = Arc::new(Mutex::new(Vec::<FundAccountRequest>::new()));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(requests.clone());
    serve_stopped_funding(&mut module, faucet);
    let submitted_hash = Arc::new(Mutex::new(None::<TxHash>));
    let submitted = submitted_hash.clone();
    module
        .register_method("sendTransaction", move |params, _, _| {
            let bytes = thruput_json_rpc_types::hex::prefixed_hex(
                params
                    .parse::<thruput_json_rpc_types::requests::SendTransactionRequest>()?
                    .signed_transaction,
            );
            let signed: thruput_xdr::transaction::SignedTransaction =
                xdrs::txs::decode(&hex::decode(bytes.strip_prefix("0x").unwrap()).unwrap())
                    .unwrap();
            let hash = TxHash(Base58(transaction_hash(&signed.tx).unwrap()));
            *submitted.lock().unwrap() = Some(hash);
            Ok::<_, ErrorObjectOwned>(SendTransactionResult {
                transaction_hash: hash,
                admission: Admission::Accepted,
            })
        })
        .unwrap();
    module
        .register_method("getTransactionReceipt", move |params, _, _| {
            let hash = params
                .parse::<thruput_json_rpc_types::requests::TransactionQuery>()?
                .transaction_hash;
            assert_eq!(*submitted_hash.lock().unwrap(), Some(hash));
            Ok::<_, ErrorObjectOwned>(Some(successful_receipt(hash)))
        })
        .unwrap();
    serve_manifest_network_info(&mut module);
    let handle = server.start(module);
    let manifest = local_manifest(faucet_public_key, faucet_address);
    let scenario = Scenario::builder("explicit-recovery")
        .accounts(1)
        .network(NetworkConfig::existing(&url, manifest.path()))
        .build()
        .await
        .unwrap();
    scenario
        .run_agents([Agent::new("alice", |ctx| async move {
            let error = ctx.submit_fund(12345).await.unwrap_err();
            let TestkitError::Submission {
                tx_hash,
                signed_transaction,
                ..
            } = error
            else {
                panic!("expected typed unresolved evidence");
            };
            assert!(matches!(
                ctx.submit_fund(67890).await,
                Err(TestkitError::FundingRequestMismatch { .. })
            ));
            assert_eq!(
                ctx.network()
                    .submit_raw(signed_transaction.replacen("0x", "0X", 1))
                    .await?,
                tx_hash
            );
            assert!(ctx.network().receipt(tx_hash).await?.is_some());
            let _next = ctx.submit_fund(67890).await?;
            Ok(())
        })])
        .await
        .unwrap();
    {
        let seen = requests.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0].client_nonce, seen[1].client_nonce);
    }
    scenario.stop().await.unwrap();
    handle.stop().unwrap();
}

fn successful_receipt(tx_hash: TxHash) -> TransactionReceipt {
    TransactionReceipt {
        invoked_addresses: vec![],
        transaction_hash: tx_hash,
        block_height: 3.into(),
        execution: ReceiptExecution::Return {
            return_data: HexBytes(vec![]),
        },
        events: vec![],
        gas_used: 123,
    }
}

fn funding_response(request: &FundAccountRequest, faucet: &LocalSigner) -> FundingSubmission {
    let calldata =
        thruput_signing::native_token_transfer_calldata(&request.recipient.0, request.raw_amount)
            .unwrap();
    let tx = thruput_signing::default_wallet::build_default_wallet_call_transaction(
        thruput_signing::default_wallet::BuildDefaultWalletCallTransaction {
            account: &PubkeyAccount::new(faucet.public_key(), 0),
            target_address: thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN,
            target_entrypoint: thruput_signing::NATIVE_TOKEN_TRANSFER_ENTRYPOINT,
            target_calldata: &calldata,
            gas_limit: 20_000_000,
            target_gas_limit: 10_000_000,
            block_expiry_height: 10_000_000,
            nonce: u64::from_le_bytes(request.client_nonce.as_bytes()[..8].try_into().unwrap()),
        },
    )
    .unwrap();
    let hash = TxHash(Base58(transaction_hash(&tx).unwrap()));
    let signed = sign_and_encode_transaction_hex(
        tx,
        faucet,
        SignTransactionParams::default(),
        &thruput_xdr::types::NetworkHash::new([0; 32].into()),
    )
    .unwrap();
    FundingSubmission {
        recipient: request.recipient,
        raw_amount: request.raw_amount,
        transaction_hash: hash,
        signed_transaction: HexBytes(hex::decode(signed.strip_prefix("0x").unwrap()).unwrap()),
    }
}

fn local_manifest(
    faucet_public_key: [u8; 32],
    faucet_address: thruput_xdr::types::ContractAddress,
) -> tempfile::NamedTempFile {
    let manifest = tempfile::NamedTempFile::new().unwrap();
    let mut document: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/hosted-runtime.json")).unwrap();
    document["kind"] = "hogd-dev-runtime".into();
    document["schemaVersion"] = 5.into();
    document
        .as_object_mut()
        .unwrap()
        .remove("bootstrapAccountDerivation");
    document["bootstrapAccounts"] = serde_json::json!([{
        "role":"faucet", "publicKey":thruput_json_rpc_types::Ed25519PublicKey::from(thruput_xdr::types::Ed25519PublicKey::from(faucet_public_key)),
        "walletAddress":thruput_json_rpc_types::ContractAddress::from(faucet_address), "initialStorageTokens":"10000000000000"
    }]);
    std::fs::write(manifest.path(), serde_json::to_vec(&document).unwrap()).unwrap();
    manifest
}

fn serve_uncertain_funding(
    module: &mut RpcModule<Arc<Mutex<Vec<FundAccountRequest>>>>,
    faucet: LocalSigner,
) {
    module
        .register_method("dev_fundAccount", move |params, requests, _| {
            let request: FundAccountRequest = params.parse()?;
            let mut seen = requests.lock().unwrap();
            seen.push(request.clone());
            if seen.len() == 1 {
                let response = funding_response(&request, &faucet);
                return Err(ErrorObjectOwned::owned(
                    thruput_json_rpc_types::error::native::codes::LOCAL_FUNDING_UNRESOLVED,
                    "receipt unavailable after durable signing",
                    Some(
                        thruput_json_rpc_types::error::native::ErrorData::LocalFundingUnresolved {
                            transaction_hash: response.transaction_hash,
                            signed_transaction: response.signed_transaction,
                            cause: None,
                        },
                    ),
                ));
            }
            if seen.len() == 2 {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    "lost admission response",
                    None::<()>,
                ));
            }
            Ok(funding_response(&request, &faucet))
        })
        .unwrap();
}

#[tokio::test]
async fn conflicting_funding_results_keep_both_identities_and_preserve_original_intent() {
    for error_result in [false, true] {
        conflicting_funding_case(error_result).await;
    }
}

async fn conflicting_funding_case(error_result: bool) {
    use thruput_json_rpc_types::error::native::{ErrorData, codes};
    let faucet = LocalSigner::from_secret_key([42; 32]);
    let manifest = local_manifest(
        faucet.verifying_key_bytes(),
        PubkeyAccount::new(faucet.public_key(), 0).address(),
    );
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(count.clone());
    module
        .register_method("dev_fundAccount", move |params, count, _| {
            let mut request: FundAccountRequest = params.parse()?;
            let attempt = count.fetch_add(1, Ordering::SeqCst);
            if attempt == 1 {
                request.client_nonce =
                    thruput_json_rpc_types::airdrop::ClientNonce::new(vec![99; 16]).unwrap();
            }
            let response = funding_response(&request, &faucet);
            if attempt == 0 || (attempt == 1 && error_result) {
                return Err(ErrorObjectOwned::owned(
                    codes::LOCAL_FUNDING_UNRESOLVED,
                    "unresolved",
                    Some(ErrorData::LocalFundingUnresolved {
                        transaction_hash: response.transaction_hash,
                        signed_transaction: response.signed_transaction,
                        cause: None,
                    }),
                ));
            }
            Ok(response)
        })
        .unwrap();
    serve_manifest_network_info(&mut module);
    let handle = server.start(module);
    let scenario = Scenario::builder("funding-conflict")
        .accounts(1)
        .network(NetworkConfig::existing(&url, manifest.path()))
        .build()
        .await
        .unwrap();
    scenario
        .run_agents([Agent::new("alice", move |ctx| async move {
            let original = ctx.submit_fund(123).await.unwrap_err();
            let TestkitError::Submission {
                tx_hash,
                signed_transaction,
                ..
            } = original
            else {
                panic!("expected retained original")
            };
            let conflict = ctx.submit_fund(123).await.unwrap_err();
            match conflict {
                TestkitError::InvalidFundingResponse {
                    retained: Some(retained),
                    response,
                    ..
                } => {
                    assert!(!error_result);
                    assert_eq!(retained.0, tx_hash);
                    assert_ne!(response.transaction_hash, tx_hash);
                    assert_eq!(
                        thruput_json_rpc_types::hex::prefixed_hex(&retained.1),
                        signed_transaction
                    );
                }
                TestkitError::InvalidFundingEvidence {
                    retained: Some(retained),
                    source,
                    ..
                } => {
                    assert!(error_result);
                    assert_eq!(retained.0, tx_hash);
                    let thruput_json_rpc_client::native::NativeClientError::Application {
                        data,
                        ..
                    } = *source
                    else {
                        panic!("missing conflicting RPC error")
                    };
                    assert_ne!(
                        data.raw()["data"]["transactionHash"],
                        serde_json::to_value(tx_hash).unwrap()
                    );
                }
                other => panic!("missing conflict evidence: {other:?}"),
            }
            let recovered = ctx.submit_fund(123).await?;
            assert_eq!(recovered.tx_hash(), tx_hash);
            assert_eq!(recovered.signed_transaction(), signed_transaction);
            Ok(())
        })])
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
    handle.stop().unwrap();
}

fn serve_stopped_funding(
    module: &mut RpcModule<Arc<Mutex<Vec<FundAccountRequest>>>>,
    faucet: LocalSigner,
) {
    module
        .register_method("dev_fundAccount", move |params, requests, _| {
            let request: FundAccountRequest = params.parse()?;
            let response = funding_response(&request, &faucet);
            let mut seen = requests.lock().unwrap();
            seen.push(request);
            if seen.len() == 1 {
                return Err(ErrorObjectOwned::owned(
                    thruput_json_rpc_types::error::native::codes::FUNDING_ATTEMPT_STOPPED,
                    "admission unverified",
                    Some(
                        thruput_json_rpc_types::error::native::ErrorData::FundingAttemptStopped(
                            thruput_json_rpc_types::error::native::FundingAttemptStoppedData {
                                transaction_hash: Some(response.transaction_hash),
                                signed_transaction: Some(response.signed_transaction),
                                service_code: None,
                                receipt: None,
                            },
                        ),
                    ),
                ));
            }
            Ok(response)
        })
        .unwrap();
}
