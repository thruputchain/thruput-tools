//! Local public funding retains request identity and returns before block production.
use groundhog_json_rpc_types::{
    airdrop::{FundAccountRequest, FundAccountResponse},
    hex::{Hex0x, HexBytes},
    transaction::TxHash,
};
use groundhog_signing::{
    LocalSigner, PubkeyAccount, SignTransactionParams, Signer, sign_and_encode_transaction_hex,
    transaction_hash,
};
use groundhog_testkit::{Agent, NetworkConfig, Scenario, TestkitError};
use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn lost_response_reuses_request_and_returns_admitted_bytes_without_receipt_wait() {
    let faucet = LocalSigner::from_secret_key([42; 32]);
    let faucet_public_key = faucet.verifying_key_bytes();
    let faucet_address = PubkeyAccount::new(faucet.public_key(), 0).address();
    let requests = Arc::new(Mutex::new(Vec::<FundAccountRequest>::new()));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(requests.clone());
    module
        .register_method("groundhog_fundAccount", move |params, requests, _| {
            let request: FundAccountRequest = params.one()?;
            let mut seen = requests.lock().unwrap();
            seen.push(request.clone());
            if seen.len() == 1 {
                let response = funding_response(request, &faucet);
                return Err(ErrorObjectOwned::owned(
                    groundhog_json_rpc_types::error::codes::LOCAL_FUNDING_UNRESOLVED,
                    "receipt unavailable after durable signing",
                    Some(
                        groundhog_json_rpc_types::airdrop::LocalFundingErrorData::new(
                            response.tx_hash,
                            response.signed_transaction,
                        ),
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
            Ok(funding_response(request, &faucet))
        })
        .unwrap();
    let receipt_ready = Arc::new(AtomicBool::new(false));
    let receipt_flag = receipt_ready.clone();
    module
        .register_method("groundhog_getTransactionReceipt", move |params, _, _| {
            let hash: TxHash = params.one()?;
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
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[1], seen[2]);
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
    module
        .register_method("groundhog_fundAccount", move |params, requests, _| {
            let request: FundAccountRequest = params.one()?;
            let response = funding_response(request.clone(), &faucet);
            let mut seen = requests.lock().unwrap();
            seen.push(request);
            if seen.len() == 1 {
                return Err(ErrorObjectOwned::owned(
                    groundhog_json_rpc_types::error::codes::LOCAL_FUNDING_UNRESOLVED,
                    "admission unverified",
                    Some(
                        groundhog_json_rpc_types::airdrop::LocalFundingErrorData::new(
                            response.tx_hash,
                            response.signed_transaction,
                        ),
                    ),
                ));
            }
            Ok(response)
        })
        .unwrap();
    let submitted_hash = Arc::new(Mutex::new(None::<TxHash>));
    let submitted = submitted_hash.clone();
    module
        .register_method("groundhog_sendRawTransaction", move |params, _, _| {
            let bytes: String = params.one()?;
            let signed: groundhog_xdr::transaction::SignedTransaction =
                xdrs::txs::decode(&hex::decode(bytes.strip_prefix("0x").unwrap()).unwrap())
                    .unwrap();
            let hash = TxHash(Hex0x(transaction_hash(&signed.tx).unwrap()));
            *submitted.lock().unwrap() = Some(hash);
            Ok::<_, ErrorObjectOwned>(hash)
        })
        .unwrap();
    module
        .register_method("groundhog_getTransactionReceipt", move |params, _, _| {
            let hash: TxHash = params.one()?;
            assert_eq!(*submitted_hash.lock().unwrap(), Some(hash));
            Ok::<_, ErrorObjectOwned>(Some(successful_receipt(hash)))
        })
        .unwrap();
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
            assert_eq!(ctx.network().submit_raw(signed_transaction).await?, tx_hash);
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

fn successful_receipt(tx_hash: TxHash) -> groundhog_json_rpc_types::receipt::TransactionReceipt {
    groundhog_json_rpc_types::receipt::TransactionReceipt {
        tx_hash,
        height: 3.into(),
        status: groundhog_json_rpc_types::receipt::ExecStatus::Ok,
        events: vec![],
        return_data: None,
        abort_data: None,
        gas_used: 123,
    }
}

fn funding_response(request: FundAccountRequest, faucet: &LocalSigner) -> FundAccountResponse {
    let calldata = groundhog_signing::native_hog_transfer_calldata(
        &request.recipient.0,
        request.amount().unwrap(),
    )
    .unwrap();
    let tx = groundhog_signing::default_wallet::build_default_wallet_call_transaction(
        groundhog_signing::default_wallet::BuildDefaultWalletCallTransaction {
            account: &PubkeyAccount::new(faucet.public_key(), 0),
            target_address: groundhog_signing::NATIVE_HOG_ADDRESS,
            target_entrypoint: groundhog_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
            target_calldata: &calldata,
            gas_limit: 20_000_000,
            target_gas_limit: 10_000_000,
            block_expiry_height: 10_000_000,
            nonce: u64::from_le_bytes(request.client_nonce.as_bytes()[..8].try_into().unwrap()),
        },
    )
    .unwrap();
    let hash = TxHash(Hex0x(transaction_hash(&tx).unwrap()));
    let signed = sign_and_encode_transaction_hex(
        tx,
        faucet,
        SignTransactionParams::default(),
        &groundhog_xdr::types::NetworkHash::new([0; 32].into()),
    )
    .unwrap();
    FundAccountResponse::new(
        request.recipient,
        request.raw_amount,
        hash,
        HexBytes(hex::decode(signed.strip_prefix("0x").unwrap()).unwrap()),
    )
}

fn local_manifest(
    faucet_public_key: [u8; 32],
    faucet_address: groundhog_xdr::types::ContractAddress,
) -> tempfile::NamedTempFile {
    let manifest = tempfile::NamedTempFile::new().unwrap();
    let mut document: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/hosted-runtime.json")).unwrap();
    document["kind"] = "hogd-dev-runtime".into();
    document["schemaVersion"] = 4.into();
    document
        .as_object_mut()
        .unwrap()
        .remove("bootstrapAccountDerivation");
    document["bootstrapAccounts"] = serde_json::json!([{
        "role":"faucet", "publicKey":format!("0x{}",hex::encode(faucet_public_key)),
        "walletAddress":format!("0x{}",hex::encode(faucet_address.as_ref())), "initialStorageTokens":"10000000000000"
    }]);
    std::fs::write(manifest.path(), serde_json::to_vec(&document).unwrap()).unwrap();
    manifest
}
