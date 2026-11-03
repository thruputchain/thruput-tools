//! Record the actual hosted faucet submission and verify native account authentication.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, Ordering},
};

use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use thruput_faucet::airdrop::{
    AirdropRuntimeConfig, AirdropRuntimeConfigError, AirdropSubmitter, RpcAirdropSubmitter,
};
use thruput_json_rpc_types::{
    base58::Base58,
    hex::HexBytes,
    network_descriptor::{
        FaucetDescriptor, NativeTokenDescriptor, NetworkDescriptor, NetworkDescriptorEndpoints,
    },
    requests,
    transaction::TxHash,
};
use thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT;
use thruput_signing::{LocalSigner, PubkeyAccount, transaction_hash};
use thruput_xdr::{
    contract::ContractEntrypointID,
    transaction::SignedTransaction,
    types::{ContractAddress, SHA256Hash},
};
use zeroize::Zeroizing;

fn descriptor(account: &PubkeyAccount, rpc_url: String) -> NetworkDescriptor {
    NetworkDescriptor::new(
        "devnet".into(),
        SHA256Hash::from([0x42; 32]).into(),
        "x23".into(),
        Base58([0x43; 32].into()),
        NetworkDescriptorEndpoints::new(rpc_url),
        NativeTokenDescriptor::new(
            Base58([0x61; 32].into()),
            7,
            HexBytes(0x6328_5b35_23f9_dc3f_u64.to_be_bytes().to_vec()),
        ),
        FaucetDescriptor::new(Base58(account.address())),
    )
}

fn assert_native_transfers(
    recorded: &[SignedTransaction],
    account: &PubkeyAccount,
    descriptor: &NetworkDescriptor,
    recipient: ContractAddress,
) {
    assert_eq!(recorded.len(), 3);
    assert_eq!(
        xdrs::txs::encode(&recorded[0]).unwrap(),
        xdrs::txs::encode(&recorded[2]).unwrap()
    );
    assert_ne!(
        recorded[0].tx.uniquifier.nonce,
        recorded[1].tx.uniquifier.nonce
    );
    for signed in recorded {
        assert_eq!(signed.tx.invocation.invoked_address, account.address());
        assert_eq!(signed.tx.auth_claim, account.auth_claim());
        assert_eq!(
            signed.tx.invocation.entrypoint,
            DEFAULT_WALLET_EXECUTE_ENTRYPOINT
        );
        assert!(signed.tx.contracts_to_deploy.is_empty());
        let calls: Vec<(ContractAddress, ContractEntrypointID, u64, Vec<u8>)> =
            xdrs::cxs::decode(&signed.tx.invocation.calldata).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, descriptor.native_token.token_address.0);
        assert_eq!(
            calls[0].1.as_ref(),
            &0x6328_5b35_23f9_dc3f_u64.to_be_bytes()
        );
        let (to, amount): (ContractAddress, u128) = xdrs::cxs::decode(&calls[0].3).unwrap();
        assert_eq!(to, recipient);
        assert_eq!(amount, 100_000_000);
    }
}

#[tokio::test]
async fn hosted_airdrops_use_native_authentication_and_distinct_transaction_nonces() {
    let recorded = Arc::new(Mutex::new(Vec::<SignedTransaction>::new()));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let tip = Arc::new(AtomicI64::new(10));
    let wrong_hash = Arc::new(AtomicBool::new(false));
    let mut module = RpcModule::new((recorded.clone(), tip.clone(), wrong_hash.clone()));
    module
        .register_method("getChainTip", |params, state, _| {
            let _: requests::EmptyRequest = params.parse()?;
            let height = state.1.load(Ordering::SeqCst);
            if height == -2 {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    "tip unavailable",
                    None::<()>,
                ));
            }
            Ok::<_, ErrorObjectOwned>(serde_json::json!({"blockHeight":height.to_string(), "blockHash":Base58(SHA256Hash::from([0;32]))}))
        })
        .unwrap();
    module
        .register_method("sendTransaction", |params, state, _| {
            let request: requests::SendTransactionRequest = params.parse()?;
            let signed: SignedTransaction =
                xdrs::txs::decode(&request.signed_transaction.0).unwrap();
            let hash = TxHash(Base58(transaction_hash(&signed.tx).unwrap()));
            state.0.lock().unwrap().push(signed);
            let hash = if state.2.load(Ordering::SeqCst) {
                TxHash(Base58([9; 32].into()))
            } else {
                hash
            };
            Ok::<_, ErrorObjectOwned>(
                serde_json::json!({"transactionHash":hash, "admission":"accepted"}),
            )
        })
        .unwrap();
    module.register_method("getVersionInfo", |params, _, _| {
        let _: requests::EmptyRequest = params.parse()?;
        Ok::<_, ErrorObjectOwned>(serde_json::json!({"wireFormat":"15.0.0", "implementation":"test",
            "supportedMethods":["getNetworkInfo","getChainTip","sendTransaction","getTransactionReceipt","getTransactionStatus"]}))
    }).unwrap();
    module
        .register_method("getNetworkInfo", |params, _, _| {
            let _: requests::EmptyRequest = params.parse()?;
            Ok::<_, ErrorObjectOwned>(
                serde_json::json!({"networkHash":Base58(SHA256Hash::from([0x42;32]))}),
            )
        })
        .unwrap();
    let handle = server.start(module);
    let signer = LocalSigner::from_secret_key([0x11; 32]);
    let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    let descriptor = descriptor(&account, url);
    let config = AirdropRuntimeConfig::from_wire("100000000", Zeroizing::new([0x11; 32])).unwrap();
    let submitter =
        RpcAirdropSubmitter::new(&descriptor, &config, SHA256Hash::from([0x42; 32]).into())
            .unwrap();
    let recipient = Base58(ContractAddress::from([0x71; 32]));
    let first = submitter.prepare_airdrop(recipient).await.unwrap();
    assert!(recorded.lock().unwrap().is_empty());
    // Preparation already read the tip; a later tip outage must not strand
    // the first broadcast.
    tip.store(-2, Ordering::SeqCst);
    submitter.submit_airdrop(&first).await.unwrap();
    assert_eq!(recorded.lock().unwrap().len(), 1);
    tip.store(10, Ordering::SeqCst);
    let second = submitter.prepare_airdrop(recipient).await.unwrap();
    submitter.submit_airdrop(&second).await.unwrap();
    assert_ne!(first.tx_hash, second.tx_hash);
    for invalid_tip in [-1, i64::MAX] {
        tip.store(invalid_tip, Ordering::SeqCst);
        assert!(matches!(
            submitter.prepare_airdrop(recipient).await.unwrap_err(),
            thruput_faucet::airdrop::AirdropSubmitError::Preflight(_)
        ));
        assert_eq!(recorded.lock().unwrap().len(), 2);
    }
    tip.store(10, Ordering::SeqCst);
    wrong_hash.store(true, Ordering::SeqCst);
    assert!(
        submitter
            .submit_airdrop(&first)
            .await
            .unwrap_err()
            .to_string()
            .contains("hash differs")
    );
    let recorded = recorded.lock().unwrap();
    assert_native_transfers(&recorded, &account, &descriptor, recipient.0);
    handle.stop().unwrap();
}

#[test]
fn submitter_rejects_address_of_another_account_nonce() {
    let signer = LocalSigner::from_secret_key([0x11; 32]);
    let descriptor = descriptor(
        &PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 1),
        "https://rpc.example".into(),
    );
    let config = AirdropRuntimeConfig::from_wire("100000000", Zeroizing::new([0x11; 32])).unwrap();
    assert!(matches!(
        RpcAirdropSubmitter::new(&descriptor, &config, SHA256Hash::from([0x42; 32]).into()),
        Err(AirdropRuntimeConfigError::FaucetAddressMismatch)
    ));
}

#[tokio::test]
async fn hosted_receipt_wait_validates_native_identity_execution_and_events() {
    let hash = TxHash(Base58([0x22; 32].into()));
    let receipt = serde_json::json!({
        "transactionHash":hash, "blockHeight":"42", "gasUsed":"7", "invokedAddresses":[], "events":[],
        "execution":{"result":"return", "returnData":"0x0102"}
    });
    let value = Arc::new(Mutex::new(receipt.clone()));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let mut module = RpcModule::new(value.clone());
    module.register_method("getVersionInfo", |params, _, _| {
        let _: requests::EmptyRequest = params.parse()?;
        Ok::<_, ErrorObjectOwned>(serde_json::json!({"wireFormat":"15.0.0", "implementation":"test",
            "supportedMethods":["getNetworkInfo","getChainTip","sendTransaction","getTransactionReceipt","getTransactionStatus"]}))
    }).unwrap();
    module
        .register_method("getNetworkInfo", |params, _, _| {
            let _: requests::EmptyRequest = params.parse()?;
            Ok::<_, ErrorObjectOwned>(
                serde_json::json!({"networkHash":Base58(SHA256Hash::from([0x42;32]))}),
            )
        })
        .unwrap();
    module
        .register_method("getTransactionReceipt", move |params, value, _| {
            let request: requests::TransactionQuery = params.parse()?;
            assert_eq!(request.transaction_hash, hash);
            Ok::<_, ErrorObjectOwned>(value.lock().unwrap().clone())
        })
        .unwrap();
    // No write or status method is registered: receipt waiting must remain read-only.
    let handle = server.start(module);
    let signer = LocalSigner::from_secret_key([0x11; 32]);
    let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
    let descriptor = descriptor(&account, url);
    let config = AirdropRuntimeConfig::from_wire("100000000", Zeroizing::new([0x11; 32])).unwrap();
    let submitter =
        RpcAirdropSubmitter::new(&descriptor, &config, SHA256Hash::from([0x42; 32]).into())
            .unwrap();
    let actual = submitter.poll_airdrop(hash, None).await.unwrap();
    assert_eq!(*actual.block_height, 42);
    assert!(
        matches!(actual.execution, thruput_json_rpc_types::responses::ReceiptExecution::Return { return_data } if return_data.0 == [1,2])
    );
    for mutation in 0..6 {
        let mut bad = receipt.clone();
        match mutation {
            0 => {
                bad["transactionHash"] =
                    serde_json::to_value(TxHash(Base58([3; 32].into()))).unwrap();
            }
            1 => bad["execution"] = serde_json::json!({"result":"return"}),
            2 => bad["events"] = serde_json::json!([{"address":"bad"}]),
            3 => {
                bad["execution"] =
                    serde_json::json!({"result":"futureResult", "payload":"preserved"});
            }
            4 => bad["blockHeight"] = serde_json::json!("-1"),
            _ => bad["blockHeight"] = serde_json::json!("43"),
        }
        *value.lock().unwrap() = bad.clone();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            submitter.poll_airdrop(hash, Some(42)),
        )
        .await
        .unwrap()
        .unwrap_err();
        let thruput_faucet::airdrop::AirdropSubmitError::Poll(error) = error else {
            panic!("expected receipt evidence, got {error:?}");
        };
        assert_eq!(error.evidence.transaction_hash, hash);
        assert_eq!(
            *error.evidence.commitment.as_ref().unwrap().block_height,
            42
        );
        if mutation == 3 {
            assert!(matches!(error.cause,
                thruput_json_rpc_client::native::receipt::ReceiptPollCause::Unsupported { raw, .. }
                if raw == bad));
        }
    }
    handle.stop().unwrap();
}

#[tokio::test]
async fn incompatible_endpoint_never_receives_a_retained_transaction() {
    for (wire, network) in [("13.0.0", [0x42; 32]), ("14.0.0", [0x99; 32])] {
        let sent = Arc::new(AtomicBool::new(false));
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = RpcModule::new(sent.clone());
        module.register_method("getVersionInfo", move |params, _, _| {
            let _: requests::EmptyRequest = params.parse()?;
            Ok::<_, ErrorObjectOwned>(serde_json::json!({"wireFormat":wire, "implementation":"test",
                "supportedMethods":["getNetworkInfo","getChainTip","sendTransaction","getTransactionReceipt","getTransactionStatus"]}))
        }).unwrap();
        module
            .register_method("getNetworkInfo", move |params, _, _| {
                let _: requests::EmptyRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    serde_json::json!({"networkHash":Base58(SHA256Hash::from(network))}),
                )
            })
            .unwrap();
        module
            .register_method("sendTransaction", |_, sent, _| {
                sent.store(true, Ordering::SeqCst);
                serde_json::Value::Null
            })
            .unwrap();
        let handle = server.start(module);
        let signer = LocalSigner::from_secret_key([0x11; 32]);
        let account = PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0);
        let descriptor = descriptor(&account, url);
        let config =
            AirdropRuntimeConfig::from_wire("100000000", Zeroizing::new([0x11; 32])).unwrap();
        let submitter =
            RpcAirdropSubmitter::new(&descriptor, &config, SHA256Hash::from([0x42; 32]).into())
                .unwrap();
        let retained = thruput_faucet::airdrop::PreparedAirdrop {
            raw_amount: "100000000".into(),
            tx_hash: TxHash(Base58([0x22; 32].into())),
            signed_transaction: "0x0102".into(),
            block_expiry_height: 1000,
        };
        assert!(matches!(
            submitter.submit_airdrop(&retained).await,
            Err(thruput_faucet::airdrop::AirdropSubmitError::Preflight(_))
        ));
        assert!(!sent.load(Ordering::SeqCst));
        handle.stop().unwrap();
    }
}
