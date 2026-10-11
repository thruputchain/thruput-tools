//! Record the actual hosted faucet submission and verify native account authentication.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, Ordering},
};

use groundhog_faucet::airdrop::{
    AirdropRuntimeConfig, AirdropRuntimeConfigError, AirdropSubmitter, RpcAirdropSubmitter,
};
use groundhog_json_rpc_types::{
    block::ChainTip,
    hex::{Hex0x, HexBytes},
    network_descriptor::{
        FaucetDescriptor, NativeHogDescriptor, NetworkDescriptor, NetworkDescriptorEndpoints,
    },
    transaction::TxHash,
};
use groundhog_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT;
use groundhog_signing::{LocalSigner, PubkeyAccount, transaction_hash};
use groundhog_xdr::{
    contract::ContractEntrypointID,
    transaction::SignedTransaction,
    types::{ContractAddress, SHA256Hash},
};
use jsonrpsee::{RpcModule, server::ServerBuilder, types::ErrorObjectOwned};
use zeroize::Zeroizing;

fn descriptor(account: &PubkeyAccount, rpc_url: String) -> NetworkDescriptor {
    NetworkDescriptor::new(
        "devnet".into(),
        SHA256Hash::from([0x42; 32]).into(),
        "x23".into(),
        Hex0x([0x43; 32].into()),
        NetworkDescriptorEndpoints::new(rpc_url),
        NativeHogDescriptor::new(
            Hex0x([0x61; 32].into()),
            7,
            HexBytes(0x6328_5b35_23f9_dc3f_u64.to_be_bytes().to_vec()),
        ),
        FaucetDescriptor::new(Hex0x(account.address())),
    )
}

fn assert_native_transfers(
    recorded: &[SignedTransaction],
    account: &PubkeyAccount,
    descriptor: &NetworkDescriptor,
    recipient: ContractAddress,
) {
    assert_eq!(recorded.len(), 4);
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
        assert_eq!(calls[0].0, descriptor.native_hog.token_address.0);
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
        .register_method("groundhog_chainTip", |_, state, _| {
            let height = state.1.load(Ordering::SeqCst);
            if height < 0 {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    "tip unavailable",
                    None::<()>,
                ));
            }
            Ok::<_, ErrorObjectOwned>(ChainTip {
                height: height.into(),
                block_hash: Hex0x([0; 32].into()),
            })
        })
        .unwrap();
    module
        .register_method("groundhog_sendRawTransaction", |params, state, _| {
            let (encoded,): (String,) = params.parse()?;
            let signed: SignedTransaction =
                xdrs::txs::decode(&hex::decode(encoded.trim_start_matches("0x")).unwrap()).unwrap();
            let hash = TxHash(Hex0x(transaction_hash(&signed.tx).unwrap()));
            state.0.lock().unwrap().push(signed);
            Ok::<_, ErrorObjectOwned>(if state.2.load(Ordering::SeqCst) {
                TxHash(Hex0x([9; 32].into()))
            } else {
                hash
            })
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
    let recipient = Hex0x(ContractAddress::from([0x71; 32]));
    let first = submitter.prepare_airdrop(recipient).await.unwrap();
    assert!(recorded.lock().unwrap().is_empty());
    // Preparation already read the tip; a later tip outage must not strand
    // the first broadcast, but must prevent an unchecked replay.
    tip.store(-1, Ordering::SeqCst);
    submitter.submit_airdrop(&first, false).await.unwrap();
    assert!(submitter.submit_airdrop(&first, true).await.is_err());
    assert_eq!(recorded.lock().unwrap().len(), 1);
    tip.store(10, Ordering::SeqCst);
    let second = submitter.prepare_airdrop(recipient).await.unwrap();
    submitter.submit_airdrop(&second, false).await.unwrap();
    assert_ne!(first.tx_hash, second.tx_hash);
    submitter.submit_airdrop(&first, true).await.unwrap();
    assert_eq!(recorded.lock().unwrap().len(), 3);
    tip.store(first.block_expiry_height, Ordering::SeqCst);
    assert!(
        submitter
            .submit_airdrop(&first, true)
            .await
            .unwrap_err()
            .to_string()
            .contains("expired")
    );
    assert_eq!(recorded.lock().unwrap().len(), 3);
    tip.store(10, Ordering::SeqCst);
    wrong_hash.store(true, Ordering::SeqCst);
    assert!(
        submitter
            .submit_airdrop(&first, true)
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
