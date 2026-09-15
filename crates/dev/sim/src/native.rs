//! Native pubkey account setup and default-wallet ABI.
use crate::{
    actor::ActorState,
    error::SimError,
    manifest::{NativeAccountFunding, NativeAccountsSpec},
    runtime::SimAccount,
    util::write_json_pretty_atomic,
};
use groundhog_json_rpc_client::{AirdropApiClient, GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::{
    airdrop::{AirdropRequest, ClientNonce},
    hex::Hex0x,
    runtime_manifest::RuntimeManifest,
};
use groundhog_signing::{
    BuildCallTransaction, PubkeyAccount, build_call_transaction, derive_dev_account_signer,
};
use groundhog_xdr::{
    contract::ContractEntrypointID, transaction::Transaction, types::ContractAddress,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

/// Native `DefaultWallet` entrypoint for executing a vector of wallet calls.
pub const EXECUTE: ContractEntrypointID =
    ContractEntrypointID::new([0xc3, 0xed, 0xb1, 0x1f, 0xdd, 0x6d, 0x5d, 0x58]);
/// Native `DefaultWallet` entrypoint for deployment; it does not transfer collateral.
pub const DEPLOY: ContractEntrypointID =
    ContractEntrypointID::new([0x6b, 0xe0, 0xcb, 0x0f, 0x6d, 0xe0, 0x91, 0x7a]);
/// Native HOG balance-read entrypoint used by the wallet smoke workload.
pub const BALANCE: ContractEntrypointID =
    ContractEntrypointID::new([0x41, 0xac, 0x95, 0xd6, 0xe0, 0x6e, 0x26, 0x20]);

/// Derive deterministic test signers and nonce-zero pubkey addresses for the manifest network.
/// This does not fund accounts or select an actor's wallet adapter.
pub fn derive_accounts(
    spec: &NativeAccountsSpec,
    count: usize,
    manifest: &RuntimeManifest,
) -> Result<Vec<SimAccount>, SimError> {
    (0..count)
        .map(|index| {
            let signer = derive_dev_account_signer(&spec.seed, index);
            let wallet_address =
                PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address();
            Ok(SimAccount {
                index: u32::try_from(index)
                    .map_err(|_| SimError::Executor("native actor index overflow".into()))?,
                signer,
                wallet_address,
                network_hash: manifest.network_hash.to_network_hash(),
            })
        })
        .collect()
}

/// DefaultWallet.execute accepts CXS Vec<WalletCall>, where each call is
/// (target address, eight-byte selector, gas limit, byte-string calldata).
pub fn execute(
    actor: &ActorState,
    target: ContractAddress,
    entrypoint: ContractEntrypointID,
    calldata: &[u8],
    nonce: u64,
    expiry: i64,
) -> Result<Transaction, groundhog_signing::SigningError> {
    let calls = vec![(
        target,
        entrypoint,
        10_000_000_u64,
        xdr_common::BytesM::<{ u32::MAX }>(calldata.to_vec()),
    )];
    let calldata = xdrs::cxs::encode(&(calls,))
        .map_err(|error| groundhog_signing::SigningError::XdrEncode(error.to_string()))?;
    Ok(build_call_transaction(BuildCallTransaction {
        invoked_address: actor.wallet_address,
        entrypoint: EXECUTE,
        calldata,
        gas_limit: 10_000_000,
        block_expiry_height: expiry,
        nonce,
        auth_claim: actor.auth_claim(),
    }))
}

/// Setup is outside the measured schedule. Faucet funding is explicit and
/// never retried with a new idempotency key. Every actor must meet its minimum.
pub async fn prepare_accounts(
    spec: &NativeAccountsSpec,
    actors: &[ActorState],
    rpc_url: &str,
    out_dir: &Path,
) -> Result<(), SimError> {
    let minimum = spec
        .minimum_balance_raw
        .parse::<u128>()
        .map_err(|_| SimError::Executor("invalid accounts.minimumBalanceRaw".into()))?;
    let client = HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(10))
        .build(rpc_url)
        .map_err(|error| SimError::RpcClient(error.to_string()))?;
    let mut records = Vec::new();
    for actor in actors {
        records.push(json!({"actorId":actor.actor_id,"accountIndex":actor.account_index,"address":format!("0x{}",hex::encode(actor.wallet_address.as_ref())),"status":"started"}));
        save_accounts(out_dir, &records).await?;
        let result = prepare_account(spec, actor, &client, minimum, out_dir, &mut records).await;
        let record = records.last_mut().expect("current actor record");
        match &result {
            Ok(()) => record["status"] = json!("verified"),
            Err(error) => {
                record["status"] = json!("failed");
                record["error"] = json!(error.to_string());
            }
        }
        save_accounts(out_dir, &records).await?;
        result?;
    }
    Ok(())
}

async fn save_accounts(out_dir: &Path, records: &[serde_json::Value]) -> Result<(), SimError> {
    write_json_pretty_atomic(
        out_dir.join("native-accounts.json"),
        &json!({"schemaVersion":1,"kind":"native","accounts":records}),
    )
    .await?;
    Ok(())
}

async fn prepare_account(
    spec: &NativeAccountsSpec,
    actor: &ActorState,
    client: &groundhog_json_rpc_client::HttpClient,
    minimum: u128,
    out_dir: &Path,
    records: &mut [serde_json::Value],
) -> Result<(), SimError> {
    let balance = client
        .get_balance(Hex0x(actor.wallet_address))
        .await
        .map_err(|error| SimError::Executor(format!("native account balance: {error}")))?;
    records.last_mut().expect("current actor record")["balance"] = json!(balance);
    if balance.address.0 != actor.wallet_address {
        return Err(SimError::Executor(
            "balance response address mismatch".into(),
        ));
    }
    if balance
        .raw_balance
        .parse::<u128>()
        .map_err(|_| SimError::Executor("invalid balance response".into()))?
        >= minimum
    {
        return Ok(());
    }
    if spec.funding != NativeAccountFunding::Faucet {
        return Err(SimError::Executor(format!(
            "native actor {} at 0x{} has balance {}, requires {}; pre-fund it or explicitly select accounts.funding=faucet",
            actor.actor_id,
            hex::encode(actor.wallet_address.as_ref()),
            balance.raw_balance,
            minimum
        )));
    }
    let mut digest = Sha256::new();
    digest.update(b"hogsim-native-funding-v1");
    digest.update(actor.wallet_address.as_ref());
    digest.update(actor.nonce_namespace.get().to_le_bytes());
    let request = AirdropRequest {
        recipient: Hex0x(actor.wallet_address),
        client_nonce: ClientNonce::new(digest.finalize().to_vec())
            .map_err(|error| SimError::Executor(error.to_string()))?,
    };
    records.last_mut().expect("current actor record")["fundingRequest"] = json!(request);
    save_accounts(out_dir, records).await?;
    let response = AirdropApiClient::request_airdrop(client, request)
        .await
        .map_err(|error| SimError::Executor(format!("native account funding: {error}")))?;
    records.last_mut().expect("current actor record")["funding"] = json!(response);
    save_accounts(out_dir, records).await?;
    if response.recipient().0 != actor.wallet_address {
        return Err(SimError::Executor("faucet recipient mismatch".into()));
    }
    let balance = tokio::time::timeout(
        Duration::from_secs(30),
        verify_funded_balance(client, actor, minimum),
    )
    .await
    .map_err(|_| {
        SimError::Executor(format!(
            "native actor {} funding did not reach minimum balance in 30s",
            actor.actor_id
        ))
    })??;
    records.last_mut().expect("current actor record")["balance"] = json!(balance);
    Ok(())
}

async fn verify_funded_balance(
    client: &groundhog_json_rpc_client::HttpClient,
    actor: &ActorState,
    minimum: u128,
) -> Result<groundhog_json_rpc_types::balance::BalanceResponse, SimError> {
    loop {
        let balance = client
            .get_balance(Hex0x(actor.wallet_address))
            .await
            .map_err(|error| SimError::Executor(format!("native funding balance: {error}")))?;
        if balance.address.0 != actor.wallet_address {
            return Err(SimError::Executor(
                "balance response address mismatch".into(),
            ));
        }
        if balance
            .raw_balance
            .parse::<u128>()
            .map_err(|_| SimError::Executor("invalid balance response".into()))?
            >= minimum
        {
            return Ok(balance);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nonce::NonceNamespace;
    use groundhog_xdr::types::{NetworkHash, SHA256Hash};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    pub fn actor() -> ActorState {
        let signer = derive_dev_account_signer("native-test", 0);
        let mut actor = ActorState::new(
            0,
            SimAccount {
                index: 0,
                signer,
                wallet_address: ContractAddress::from([9; 32]),
                network_hash: NetworkHash::new(SHA256Hash::new([0; 32])),
            },
            NonceNamespace::new(1).expect("test namespace"),
        );
        actor.use_native_wallet();
        actor
    }

    #[test]
    fn native_actor_and_execute_use_pubkey_identity_and_wallet_abi() {
        type Call = (
            ContractAddress,
            ContractEntrypointID,
            u64,
            xdr_common::BytesM,
        );
        let actor = actor();
        let account = PubkeyAccount::from_public_key_bytes(actor.signer.verifying_key_bytes(), 0);
        assert_eq!(actor.wallet_address, account.address());
        assert_ne!(actor.wallet_address, ContractAddress::from([9; 32]));
        let tx = execute(
            &actor,
            groundhog_signing::NATIVE_HOG_ADDRESS,
            BALANCE,
            &[1, 2, 3],
            42,
            100,
        )
        .expect("native call");
        assert_eq!(tx.auth_claim, account.auth_claim());
        assert_eq!(tx.invocation.entrypoint, EXECUTE);
        assert_eq!(tx.uniquifier.nonce, 42);
        let (calls,): (Vec<Call>,) =
            xdrs::cxs::decode(&tx.invocation.calldata.0).expect("native ABI");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, groundhog_signing::NATIVE_HOG_ADDRESS);
        assert_eq!(calls[0].1, BALANCE);
        assert_eq!(calls[0].3.0, [1, 2, 3]);
    }

    async fn funding_server(
        address: ContractAddress,
        initial: u128,
    ) -> (String, jsonrpsee::server::ServerHandle, Arc<AtomicUsize>) {
        let server = jsonrpsee::server::ServerBuilder::default()
            .build("127.0.0.1:0")
            .await
            .expect("test server");
        let url = format!("http://{}", server.local_addr().expect("server address"));
        let calls = Arc::new(AtomicUsize::new(0));
        let balance_calls = calls.clone();
        let faucet_calls = calls.clone();
        let mut module = jsonrpsee::RpcModule::new(());
        module.register_method("groundhog_getBalance", move |_, (), _| json!({"address":Hex0x(address),"rawBalance":if balance_calls.load(Ordering::SeqCst)>0 {"100".into()} else {initial.to_string()},"height":1})).expect("balance method");
        module.register_method("groundhog_requestAirdrop", move |_, (), _| {faucet_calls.fetch_add(1,Ordering::SeqCst); json!({"attemptStatus":"succeeded","recipient":Hex0x(address),"txHash":format!("0x{}","01".repeat(32)),"rawAmount":"100","height":1,"status":{"result":"ok"}})}).expect("faucet method");
        (url, server.start(module), calls)
    }

    #[tokio::test]
    async fn prefunded_native_accounts_never_call_faucet_and_zero_balance_fails() {
        let actor = actor();
        let spec: NativeAccountsSpec =
            serde_json::from_value(json!({"kind":"native","seed":"native-test"})).expect("config");
        for initial in [0, 100] {
            let (url, server, calls) = funding_server(actor.wallet_address, initial).await;
            let out = tempfile::tempdir().expect("output");
            let result =
                prepare_accounts(&spec, std::slice::from_ref(&actor), &url, out.path()).await;
            assert_eq!(result.is_ok(), initial > 0);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            server.stop().expect("stop server");
        }
    }

    #[tokio::test]
    async fn faucet_failure_retains_request_response_and_verification_error() {
        for transport_failure in [false, true] {
            let actor = actor();
            let address = actor.wallet_address;
            let out = tempfile::tempdir().expect("output");
            let artifact = out.path().join("native-accounts.json");
            let server = jsonrpsee::server::ServerBuilder::default()
                .build("127.0.0.1:0")
                .await
                .expect("server");
            let url = format!("http://{}", server.local_addr().expect("address"));
            let funded = Arc::new(AtomicUsize::new(0));
            let balance_funded = funded.clone();
            let response_artifact = artifact.clone();
            let mut module = jsonrpsee::RpcModule::new(());
            module
                .register_method("groundhog_getBalance", move |_, (), _| {
                    let raw = if balance_funded.load(Ordering::SeqCst) == 0 {
                        "0"
                    } else {
                        let evidence: serde_json::Value = serde_json::from_slice(
                            &std::fs::read(&response_artifact).expect("response checkpoint"),
                        )
                        .expect("JSON");
                        assert!(evidence["accounts"][0]["funding"]["txHash"].is_string());
                        "invalid"
                    };
                    json!({"address":Hex0x(address),"rawBalance":raw,"height":1})
                })
                .expect("balance");
            let request_artifact = artifact.clone();
            module.register_method("groundhog_requestAirdrop", move |params, (), _| {
                let request: Vec<serde_json::Value> = params.parse().expect("request");
                let evidence: serde_json::Value = serde_json::from_slice(&std::fs::read(&request_artifact).expect("request checkpoint")).expect("JSON");
                assert_eq!(evidence["accounts"][0]["fundingRequest"],request[0]);
                if transport_failure { return Err(jsonrpsee::types::ErrorObjectOwned::owned(-32000,"uncertain funding",None::<()>)); }
                funded.store(1,Ordering::SeqCst);
                Ok(json!({"attemptStatus":"succeeded","recipient":Hex0x(address),"txHash":format!("0x{}","01".repeat(32)),"rawAmount":"100","height":1,"status":{"result":"ok"}}))
            }).expect("faucet");
            let handle = server.start(module);
            let spec =
                serde_json::from_value(json!({"kind":"native","seed":"test","funding":"faucet"}))
                    .expect("spec");
            assert!(
                prepare_accounts(&spec, &[actor], &url, out.path())
                    .await
                    .is_err()
            );
            let evidence: serde_json::Value =
                serde_json::from_slice(&std::fs::read(artifact).expect("artifact")).expect("JSON");
            let record = &evidence["accounts"][0];
            assert_eq!(record["status"], "failed");
            assert!(record["fundingRequest"]["clientNonce"].is_string());
            assert!(
                record["error"]
                    .as_str()
                    .expect("error")
                    .contains(if transport_failure {
                        "uncertain funding"
                    } else {
                        "invalid balance response"
                    })
            );
            assert_eq!(record["funding"].is_object(), !transport_failure);
            handle.stop().expect("stop");
        }
    }

    #[tokio::test]
    async fn explicit_faucet_funding_is_verified_before_setup_completes() {
        let actor = actor();
        let spec: NativeAccountsSpec = serde_json::from_value(
            json!({"kind":"native","seed":"native-test","funding":"faucet"}),
        )
        .expect("config");
        let (url, server, calls) = funding_server(actor.wallet_address, 0).await;
        let out = tempfile::tempdir().expect("output");
        prepare_accounts(&spec, &[actor], &url, out.path())
            .await
            .expect("funded actor");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let records: serde_json::Value = serde_json::from_slice(
            &std::fs::read(out.path().join("native-accounts.json")).expect("account artifact"),
        )
        .expect("JSON artifact");
        assert_eq!(records["accounts"][0]["balance"]["rawBalance"], "100");
        server.stop().expect("stop server");
    }
}
