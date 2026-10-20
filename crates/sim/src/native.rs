//! Native pubkey account setup and default-wallet ABI.
use crate::rpc::known;
use crate::{
    actor::ActorState,
    error::SimError,
    manifest::{NativeAccountFunding, NativeAccountsSpec},
    runtime::SimAccount,
    util::write_json_pretty_atomic,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use thruput_json_rpc_client::{HttpClientBuilder, native::NativeClient};
use thruput_json_rpc_types::{
    airdrop::ClientNonce,
    base58::Base58,
    requests::{AddressQuery, AirdropRequest, FundAccountRequest},
    responses::{AirdropAttempt, BalanceResult},
    runtime_manifest::RuntimeManifest,
};
use thruput_signing::{
    BuildCallTransaction, PubkeyAccount, build_call_transaction, derive_dev_account_signer,
};
use thruput_xdr::{
    contract::ContractEntrypointID, transaction::Transaction, types::ContractAddress,
};

/// Native `DefaultWallet` entrypoint for executing a vector of wallet calls.
pub const EXECUTE: ContractEntrypointID =
    ContractEntrypointID::new([0xc3, 0xed, 0xb1, 0x1f, 0xdd, 0x6d, 0x5d, 0x58]);
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
) -> Result<Transaction, thruput_signing::SigningError> {
    let calls = vec![(
        target,
        entrypoint,
        10_000_000_u64,
        xdr_common::BytesM::<{ u32::MAX }>(calldata.to_vec()),
    )];
    let calldata = xdrs::cxs::encode(&(calls,))
        .map_err(|error| thruput_signing::SigningError::XdrEncode(error.to_string()))?;
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
    runtime: &RuntimeManifest,
    rpc_url: &str,
    out_dir: &Path,
    request_timeout: Duration,
) -> Result<(), SimError> {
    if !cfg!(unix)
        && spec.funding == NativeAccountFunding::Faucet
        && runtime.kind == thruput_json_rpc_types::runtime_manifest::HOGD_DEV_RUNTIME_MANIFEST_KIND
    {
        return Err(SimError::Executor(
            "local faucet setup requires a Unix filesystem for durable funding evidence; pre-fund fixtures on this platform".into(),
        ));
    }
    let evidence = out_dir.join("native-accounts.json");
    let reservation = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&evidence)
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                SimError::Executor(format!(
                    "native funding evidence already exists at {}; preserve and reconcile its fundingRequest before starting a new run in a new output directory",
                    evidence.display()
                ))
            } else {
                SimError::Executor(format!("reserve native funding evidence: {error}"))
            }
        })?;
    reservation.sync_all().await?;
    #[cfg(unix)]
    tokio::fs::File::open(out_dir).await?.sync_all().await?;
    drop(reservation);
    let minimum = spec
        .minimum_balance_raw
        .parse::<u128>()
        .map_err(|_| SimError::Executor("invalid accounts.minimumBalanceRaw".into()))?;
    let client = HttpClientBuilder::default()
        .request_timeout(request_timeout)
        .build(rpc_url)
        .map_err(|error| SimError::RpcClient(error.to_string()))?;
    let mut records = Vec::new();
    for actor in actors {
        records.push(json!({"actorId":actor.actor_id,"accountIndex":actor.account_index,"address":Base58(actor.wallet_address),"status":"started"}));
        save_accounts(out_dir, &records).await?;
        let result = prepare_account(
            spec,
            actor,
            runtime,
            &client,
            minimum,
            out_dir,
            &mut records,
        )
        .await;
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
    runtime: &RuntimeManifest,
    client: &thruput_json_rpc_client::HttpClient,
    minimum: u128,
    out_dir: &Path,
    records: &mut [serde_json::Value],
) -> Result<(), SimError> {
    let balance = read_balance(client, actor).await?;
    records.last_mut().expect("current actor record")["balance"] = json!(balance);
    if balance.address.0 != actor.wallet_address {
        return Err(SimError::Executor(
            "balance response address mismatch".into(),
        ));
    }
    if u128::try_from(balance.raw_balance)
        .map_err(|_| SimError::Executor("negative balance response".into()))?
        >= minimum
    {
        return Ok(());
    }
    if spec.funding != NativeAccountFunding::Faucet {
        return Err(SimError::Executor(format!(
            "native actor {} at {} has balance {}, requires {}; pre-fund it or explicitly select accounts.funding=faucet",
            actor.actor_id,
            Base58(actor.wallet_address),
            balance.raw_balance,
            minimum
        )));
    }
    if runtime.kind == thruput_json_rpc_types::runtime_manifest::HOGD_DEV_RUNTIME_MANIFEST_KIND {
        fund_local_account(spec, actor, runtime, client, out_dir, records).await?;
        let balance = tokio::time::timeout(
            Duration::from_secs(30),
            verify_funded_balance(client, actor, minimum),
        )
        .await
        .map_err(|_| {
            SimError::Executor("local funding did not reach the configured minimum in 30s".into())
        })??;
        records.last_mut().expect("bug: current actor record")["balance"] = json!(balance);
        return Ok(());
    }
    if spec.funding_amount_raw.is_some() {
        return Err(SimError::Executor(
            "fundingAmountRaw requires a local runtime; hosted faucets retain their fixed amount"
                .into(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"hogsim-native-funding-v1");
    digest.update(actor.wallet_address.as_ref());
    digest.update(actor.nonce_namespace.get().to_le_bytes());
    let request = AirdropRequest {
        recipient: Base58(actor.wallet_address),
        client_nonce: ClientNonce::new(digest.finalize().to_vec())
            .map_err(|error| SimError::Executor(error.to_string()))?,
    };
    records.last_mut().expect("current actor record")["fundingRequest"] = json!(request);
    save_accounts(out_dir, records).await?;
    match known(
        "faucet_requestAirdrop",
        NativeClient::new(client.clone())
            .request_airdrop(request)
            .await,
    ) {
        Ok(response) => {
            records.last_mut().expect("bug: current actor record")["funding"] = json!(response);
            save_accounts(out_dir, records).await?;
            reconcile_airdrop(client, actor.wallet_address, response).await?;
        }
        Err(error) => {
            records.last_mut().expect("bug: current actor record")["fundingError"] =
                error.evidence();
            save_accounts(out_dir, records).await?;
            return Err(SimError::Executor(format!(
                "native account funding: {error}; reconcile retained evidence in native-accounts.json"
            )));
        }
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

async fn reconcile_airdrop(
    client: &thruput_json_rpc_client::HttpClient,
    address: ContractAddress,
    response: AirdropAttempt,
) -> Result<(), SimError> {
    let (AirdropAttempt::Pending { recipient, .. } | AirdropAttempt::Succeeded { recipient, .. }) =
        &response
    else {
        return Err(SimError::Executor("unsupported funding response".into()));
    };
    if recipient.0 != address {
        return Err(SimError::Executor("faucet recipient mismatch".into()));
    }
    if let AirdropAttempt::Pending {
        transaction_hash, ..
    } = response
    {
        let hash = transaction_hash.ok_or_else(|| SimError::Executor("faucet funding remains pending without a transaction hash; reconcile native-accounts.json".into()))?;
        wait_funding_receipt(client, hash).await?;
    }
    Ok(())
}

async fn fund_local_account(
    spec: &NativeAccountsSpec,
    actor: &ActorState,
    runtime: &RuntimeManifest,
    client: &thruput_json_rpc_client::HttpClient,
    out_dir: &Path,
    records: &mut [serde_json::Value],
) -> Result<(), SimError> {
    let mut faucets = runtime.bootstrap_accounts.iter().filter(|a| {
        a.role == thruput_json_rpc_types::runtime_manifest::BOOTSTRAP_ACCOUNT_ROLE_FAUCET
    });
    let faucet = faucets
        .next()
        .ok_or_else(|| SimError::Executor("local runtime faucet is missing".into()))?;
    if faucets.next().is_some()
        || PubkeyAccount::from_public_key_bytes(*faucet.public_key.0, 0).address()
            != faucet.wallet_address.0
    {
        return Err(SimError::Executor(
            "local faucet metadata does not match a unique native account".into(),
        ));
    }
    let raw_amount = spec.funding_amount_raw.clone().ok_or_else(|| {
        SimError::Executor("local faucet setup requires explicit accounts.fundingAmountRaw".into())
    })?;
    let mut digest = Sha256::new();
    digest.update(b"hogsim-local-funding-v1");
    digest.update(runtime.network_hash.to_network_hash().as_ref());
    digest.update(faucet.wallet_address.0.as_ref());
    digest.update(actor.wallet_address.as_ref());
    digest.update(actor.nonce_namespace.get().to_le_bytes());
    let request = FundAccountRequest {
        recipient: Base58(actor.wallet_address),
        raw_amount: raw_amount
            .parse()
            .map_err(|_| SimError::Executor("invalid funding amount".into()))?,
        faucet: faucet.wallet_address,
        client_nonce: ClientNonce::new(digest.finalize().to_vec())
            .map_err(|e| SimError::Executor(e.to_string()))?,
    };
    let record = records.last_mut().expect("bug: current actor record");
    record["networkHash"] = json!(runtime.network_hash);
    record["fundingRequest"] = json!(request);
    save_accounts(out_dir, records).await?;
    let result = known(
        "dev_fundAccount",
        NativeClient::new(client.clone())
            .fund_account(request.clone())
            .await,
    );
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            records.last_mut().expect("bug: current actor record")["fundingError"] =
                error.evidence();
            save_accounts(out_dir, records).await?;
            return Err(SimError::Executor(format!(
                "local funding: {error}; reconcile retained evidence in native-accounts.json"
            )));
        }
    };
    // Retain all returned evidence before interpreting it, including invalid signed bytes.
    records.last_mut().expect("bug: current actor record")["funding"] = json!(response);
    save_accounts(out_dir, records).await?;
    let signed: thruput_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&response.signed_transaction.0)
            .map_err(|e| SimError::Executor(e.to_string()))?;
    let hash = thruput_signing::transaction_hash(&signed.tx)
        .map_err(|e| SimError::Executor(e.to_string()))?;
    if response.recipient != request.recipient
        || response.raw_amount != request.raw_amount
        || response.transaction_hash.0.0 != hash
    {
        return Err(SimError::Executor(
            "local funding response does not match the retained request/hash".into(),
        ));
    }
    wait_funding_receipt(client, response.transaction_hash).await
}

async fn wait_funding_receipt(
    client: &thruput_json_rpc_client::HttpClient,
    hash: thruput_json_rpc_types::transaction::TxHash,
) -> Result<(), SimError> {
    let receipt = thruput_json_rpc_client::poll_receipt(
        &crate::rpc::SimReceiptSource(client),
        hash,
        crate::rpc::receipt_poll_config(Duration::from_secs(30)),
    )
    .await
    .map_err(|e| SimError::Executor(format!("funding receipt: {e}")))?;
    if !matches!(
        receipt.status,
        thruput_json_rpc_types::receipt::ExecStatus::Ok
    ) {
        return Err(SimError::Executor(format!(
            "funding execution failed: {}",
            json!(receipt)
        )));
    }
    Ok(())
}

async fn verify_funded_balance(
    client: &thruput_json_rpc_client::HttpClient,
    actor: &ActorState,
    minimum: u128,
) -> Result<BalanceResult, SimError> {
    loop {
        let balance = read_balance(client, actor).await?;
        if balance.address.0 != actor.wallet_address {
            return Err(SimError::Executor(
                "balance response address mismatch".into(),
            ));
        }
        if u128::try_from(balance.raw_balance)
            .map_err(|_| SimError::Executor("negative balance response".into()))?
            >= minimum
        {
            return Ok(balance);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn read_balance(
    client: &thruput_json_rpc_client::HttpClient,
    actor: &ActorState,
) -> Result<BalanceResult, SimError> {
    known(
        "getBalance",
        NativeClient::new(client.clone())
            .get_balance(AddressQuery {
                address: Base58(actor.wallet_address),
            })
            .await,
    )
    .map_err(|error| SimError::Executor(format!("native account balance: {}", error.evidence())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nonce::NonceNamespace;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use thruput_xdr::types::{NetworkHash, SHA256Hash};

    fn hosted_runtime() -> RuntimeManifest {
        serde_json::from_str(include_str!(
            "../../testkit/tests/fixtures/hosted-runtime.json"
        ))
        .expect("hosted fixture")
    }

    fn local_runtime() -> RuntimeManifest {
        let mut runtime = hosted_runtime();
        runtime.kind = "hogd-dev-runtime".into();
        runtime.schema_version = 4;
        runtime.bootstrap_account_derivation = None;
        runtime.bootstrap_accounts.truncate(1);
        let faucet = derive_dev_account_signer("local-faucet-fixture", 0);
        let allocation = &mut runtime.bootstrap_accounts[0];
        allocation.index = None;
        allocation.public_key = Base58(faucet.verifying_key_bytes().into());
        allocation.wallet_address =
            Base58(PubkeyAccount::from_public_key_bytes(faucet.verifying_key_bytes(), 0).address());
        runtime
    }

    fn local_funding_response(
        request: &FundAccountRequest,
    ) -> thruput_json_rpc_types::responses::FundingSubmission {
        use thruput_signing::{
            SignTransactionParams, sign_and_encode_transaction_hex, transaction_hash,
        };
        let faucet = derive_dev_account_signer("local-faucet-fixture", 0);
        let calldata =
            thruput_signing::native_hog_transfer_calldata(&request.recipient.0, request.raw_amount)
                .unwrap();
        let tx = thruput_signing::default_wallet::build_default_wallet_call_transaction(
            thruput_signing::default_wallet::BuildDefaultWalletCallTransaction {
                account: &PubkeyAccount::from_public_key_bytes(faucet.verifying_key_bytes(), 0),
                target_address: thruput_signing::NATIVE_HOG_ADDRESS,
                target_entrypoint: thruput_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
                target_calldata: &calldata,
                gas_limit: 20_000_000,
                target_gas_limit: 10_000_000,
                block_expiry_height: 1000,
                nonce: 123,
            },
        )
        .unwrap();
        let hash =
            thruput_json_rpc_types::transaction::TxHash(Base58(transaction_hash(&tx).unwrap()));
        let encoded = sign_and_encode_transaction_hex(
            tx,
            &faucet,
            SignTransactionParams::default(),
            &NetworkHash::new([0; 32].into()),
        )
        .unwrap();
        serde_json::from_value(json!({"recipient":request.recipient,"rawAmount":request.raw_amount.to_string(),"transactionHash":hash,"signedTransaction":encoded})).unwrap()
    }

    #[tokio::test]
    async fn repeated_output_directory_preserves_unresolved_funding_evidence() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("native-accounts.json");
        let original =
            br#"{"accounts":[{"fundingRequest":{"clientNonce":"0x1234"},"status":"failed"}]}"#;
        tokio::fs::write(&path, original).await.expect("evidence");
        let spec = NativeAccountsSpec {
            seed: "explicit-fixture".into(),
            funding: NativeAccountFunding::Faucet,
            minimum_balance_raw: "1".into(),
            funding_amount_raw: Some("1000000".into()),
            deployment_collateral_raw: None,
            kind: crate::manifest::NativeAccountKind::Native,
        };
        let error = prepare_accounts(
            &spec,
            &[],
            &local_runtime(),
            "http://127.0.0.1:1",
            dir.path(),
            Duration::from_millis(10),
        )
        .await
        .expect_err("existing evidence");
        assert!(error.to_string().contains("preserve and reconcile"));
        assert_eq!(tokio::fs::read(path).await.expect("evidence"), original);
    }

    #[tokio::test]
    async fn local_funding_persists_request_and_does_not_repeat_after_lost_response() {
        let actor = actor();
        let address = actor.wallet_address;
        let out = tempfile::tempdir().unwrap();
        let artifact = out.path().join("native-accounts.json");
        let requests = Arc::new(std::sync::Mutex::new(Vec::<FundAccountRequest>::new()));
        let server = jsonrpsee::server::ServerBuilder::default()
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = jsonrpsee::RpcModule::new(requests.clone());
        module.register_method("getBalance", move |_, requests, _| json!({"address":Base58(address),"rawBalance":if requests.lock().unwrap().is_empty(){"0"}else{"100"},"stateBlockHeight":"3"})).unwrap();
        module
            .register_async_method("dev_fundAccount", move |params, requests, _| {
                let artifact = artifact.clone();
                async move {
                    let request: FundAccountRequest = params.parse()?;
                    let saved: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&artifact).unwrap()).unwrap();
                    assert_eq!(saved["accounts"][0]["fundingRequest"], json!(request));
                    let first = {
                        let mut seen = requests.lock().unwrap();
                        seen.push(request.clone());
                        seen.len() == 1
                    };
                    if first {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Ok::<_, jsonrpsee::types::ErrorObjectOwned>(local_funding_response(&request))
                }
            })
            .unwrap();
        module.register_method("getTransactionReceipt", |params, _, _| {
            let request: thruput_json_rpc_types::requests::TransactionQuery = params.parse()?;
            let hash = request.transaction_hash;
            Ok::<_, jsonrpsee::types::ErrorObjectOwned>(json!({"transactionHash":hash,"blockHeight":"3","execution":{"result":"return","returnData":"0x"},"events":[],"gasUsed":"1"}))
        }).unwrap();
        let handle = server.start(module);
        let spec = serde_json::from_value(json!({"kind":"native","seed":"fixture","funding":"faucet","fundingAmountRaw":"100","minimumBalanceRaw":"50"})).unwrap();
        let runtime = local_runtime();
        prepare_accounts(
            &spec,
            &[actor],
            &runtime,
            &url,
            out.path(),
            Duration::from_millis(30),
        )
        .await
        .expect_err("uncertain funding must not be repeated");
        {
            let seen = requests.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].faucet, runtime.bootstrap_accounts[0].wallet_address);
        }
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(out.path().join("native-accounts.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["accounts"][0]["status"], "failed");
        assert!(saved["accounts"][0]["fundingError"].is_object());
        handle.stop().unwrap();
    }

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
            thruput_signing::NATIVE_HOG_ADDRESS,
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
        assert_eq!(calls[0].0, thruput_signing::NATIVE_HOG_ADDRESS);
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
        module.register_method("getBalance", move |_, (), _| json!({"address":Base58(address),"rawBalance":if balance_calls.load(Ordering::SeqCst)>0 {"100".into()} else {initial.to_string()},"stateBlockHeight":"1"})).expect("balance method");
        module.register_method("faucet_requestAirdrop", move |_, (), _| {faucet_calls.fetch_add(1,Ordering::SeqCst); json!({"attemptStatus":"succeeded","recipient":Base58(address),"transactionHash":Base58(SHA256Hash::from([1; 32])),"rawAmount":"100","blockHeight":"1","execution":{"result":"return","returnData":"0x"}})}).expect("faucet method");
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
            let result = prepare_accounts(
                &spec,
                std::slice::from_ref(&actor),
                &hosted_runtime(),
                &url,
                out.path(),
                Duration::from_secs(10),
            )
            .await;
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
                .register_method("getBalance", move |_, (), _| {
                    let raw = if balance_funded.load(Ordering::SeqCst) == 0 {
                        "0"
                    } else {
                        let evidence: serde_json::Value = serde_json::from_slice(
                            &std::fs::read(&response_artifact).expect("response checkpoint"),
                        )
                        .expect("JSON");
                        assert!(evidence["accounts"][0]["funding"]["transactionHash"].is_string());
                        "invalid"
                    };
                    json!({"address":Base58(address),"rawBalance":raw,"stateBlockHeight":"1"})
                })
                .expect("balance");
            let request_artifact = artifact.clone();
            module.register_method("faucet_requestAirdrop", move |params, (), _| {
                let request: serde_json::Value = params.parse().expect("request");
                let evidence: serde_json::Value = serde_json::from_slice(&std::fs::read(&request_artifact).expect("request checkpoint")).expect("JSON");
                assert_eq!(evidence["accounts"][0]["fundingRequest"],request);
                if transport_failure { return Err(jsonrpsee::types::ErrorObjectOwned::owned(-32000,"uncertain funding",None::<()>)); }
                funded.store(1,Ordering::SeqCst);
                Ok(json!({"attemptStatus":"succeeded","recipient":Base58(address),"transactionHash":Base58(SHA256Hash::from([1; 32])),"rawAmount":"100","blockHeight":"1","execution":{"result":"return","returnData":"0x"}}))
            }).expect("faucet");
            let handle = server.start(module);
            let spec =
                serde_json::from_value(json!({"kind":"native","seed":"test","funding":"faucet"}))
                    .expect("spec");
            assert!(
                prepare_accounts(
                    &spec,
                    &[actor],
                    &hosted_runtime(),
                    &url,
                    out.path(),
                    Duration::from_secs(10)
                )
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
                        "native account balance"
                    })
            );
            assert_eq!(record["funding"].is_object(), !transport_failure);
            handle.stop().expect("stop");
        }
    }

    #[tokio::test]
    async fn funding_errors_and_pending_without_hash_stop_without_more_reads_or_writes() {
        for mode in ["timeout", "application", "pending"] {
            let actor = actor();
            let address = actor.wallet_address;
            let out = tempfile::tempdir().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let reads = Arc::new(AtomicUsize::new(0));
            let server = jsonrpsee::server::ServerBuilder::default()
                .build("127.0.0.1:0")
                .await
                .unwrap();
            let url = format!("http://{}", server.local_addr().unwrap());
            let mut module = jsonrpsee::RpcModule::new(());
            let observed = reads.clone();
            module
                .register_method("getBalance", move |_, (), _| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    json!({"address":Base58(address),"rawBalance":"0","stateBlockHeight":"1"})
                })
                .unwrap();
            let writes = calls.clone();
            module.register_async_method("faucet_requestAirdrop", move |_, _, _| {
                writes.fetch_add(1,Ordering::SeqCst);
                async move {
                    if mode == "timeout" { tokio::time::sleep(Duration::from_millis(100)).await; }
                    if mode == "application" { return Err(jsonrpsee::types::ErrorObjectOwned::owned(-32051,"unknown funding outcome",Some(json!({"kind":"backendTimeout","proof":"retained"})))); }
                    Ok(json!({"attemptStatus":"pending","recipient":Base58(address),"rawAmount":"100","retryAfterSeconds":"1"}))
                }
            }).unwrap();
            let handle = server.start(module);
            let spec = serde_json::from_value(
                json!({"kind":"native","seed":"fixture","funding":"faucet"}),
            )
            .unwrap();
            prepare_accounts(
                &spec,
                &[actor],
                &hosted_runtime(),
                &url,
                out.path(),
                Duration::from_millis(30),
            )
            .await
            .expect_err("stop on uncertain result");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                reads.load(Ordering::SeqCst),
                1,
                "no post-error balance reconciliation"
            );
            let evidence: serde_json::Value = serde_json::from_slice(
                &std::fs::read(out.path().join("native-accounts.json")).unwrap(),
            )
            .unwrap();
            let record = &evidence["accounts"][0];
            assert_eq!(record["status"], "failed");
            assert!(record["fundingRequest"].is_object());
            if mode == "pending" {
                assert_eq!(record["funding"]["attemptStatus"], "pending");
            } else {
                assert!(record["fundingError"].is_object());
            }
            if mode == "application" {
                assert_eq!(
                    record["fundingError"]["rpcError"]["data"]["proof"],
                    "retained"
                );
            }
            handle.stop().unwrap();
        }
    }

    #[tokio::test]
    async fn hosted_pending_with_hash_waits_for_receipt_then_verifies_balance() {
        let actor = actor();
        let address = actor.wallet_address;
        let hash = thruput_json_rpc_types::transaction::TxHash(Base58(SHA256Hash::from([7; 32])));
        let out = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let receipts = Arc::new(AtomicUsize::new(0));
        let server = jsonrpsee::server::ServerBuilder::default()
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = jsonrpsee::RpcModule::new(());
        let observed = reads.clone();
        let receipt_reads = receipts.clone();
        module.register_method("getBalance", move |_, (), _| {
            let read = observed.fetch_add(1, Ordering::SeqCst);
            if read > 0 {
                assert_eq!(receipt_reads.load(Ordering::SeqCst), 2, "wait before balance");
            }
            json!({"address":Base58(address),"rawBalance":if read == 0 {"0"} else {"100"},"stateBlockHeight":"3"})
        }).unwrap();
        let writes = calls.clone();
        module.register_method("faucet_requestAirdrop", move |_, (), _| {
            writes.fetch_add(1, Ordering::SeqCst);
            json!({"attemptStatus":"pending","recipient":Base58(address),"rawAmount":"100","transactionHash":hash,"retryAfterSeconds":"1"})
        }).unwrap();
        let receipt_reads = receipts.clone();
        module.register_method("getTransactionReceipt", move |params, (), _| {
            let request: thruput_json_rpc_types::requests::TransactionQuery = params.parse().unwrap();
            assert_eq!(request.transaction_hash, hash);
            if receipt_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                return serde_json::Value::Null;
            }
            json!({"transactionHash":hash,"blockHeight":"3","execution":{"result":"return","returnData":"0x"},"events":[],"gasUsed":"1"})
        }).unwrap();
        module
            .register_method("getTransactionStatus", |_, (), _| -> serde_json::Value {
                panic!("funding wait only reads receipts")
            })
            .unwrap();
        let handle = server.start(module);
        let spec = serde_json::from_value(
            json!({"kind":"native","seed":"fixture","funding":"faucet","minimumBalanceRaw":"50"}),
        )
        .unwrap();
        prepare_accounts(
            &spec,
            &[actor],
            &hosted_runtime(),
            &url,
            out.path(),
            Duration::from_secs(2),
        )
        .await
        .expect("funded actor");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert_eq!(receipts.load(Ordering::SeqCst), 2);
        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(out.path().join("native-accounts.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            evidence["accounts"][0]["funding"]["transactionHash"],
            json!(hash)
        );
        assert_eq!(evidence["accounts"][0]["balance"]["rawBalance"], "100");
        handle.stop().unwrap();
    }

    #[tokio::test]
    async fn local_funding_receipt_failure_retains_submission_and_raw_evidence() {
        let actor = actor();
        let address = actor.wallet_address;
        let out = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let writes = calls.clone();
        let server = jsonrpsee::server::ServerBuilder::default()
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let mut module = jsonrpsee::RpcModule::new(());
        module.register_method("getBalance",move |_,(),_|json!({"address":Base58(address),"rawBalance":"0","stateBlockHeight":"1"})).unwrap();
        module
            .register_method("dev_fundAccount", move |params, (), _| {
                writes.fetch_add(1, Ordering::SeqCst);
                let request: FundAccountRequest = params.parse().unwrap();
                json!(local_funding_response(&request))
            })
            .unwrap();
        module.register_method("getTransactionReceipt",|params,(),_| {
            let request:thruput_json_rpc_types::requests::TransactionQuery=params.parse().unwrap();
            json!({"transactionHash":request.transaction_hash,"blockHeight":"3","execution":{"result":"future","evidence":"retain-me"},"events":[],"gasUsed":"1"})
        }).unwrap();
        let handle = server.start(module);
        let spec=serde_json::from_value(json!({"kind":"native","seed":"fixture","funding":"faucet","fundingAmountRaw":"100","minimumBalanceRaw":"50"})).unwrap();
        prepare_accounts(
            &spec,
            &[actor],
            &local_runtime(),
            &url,
            out.path(),
            Duration::from_secs(1),
        )
        .await
        .expect_err("unsupported receipt");
        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(out.path().join("native-accounts.json")).unwrap(),
        )
        .unwrap();
        let record = &evidence["accounts"][0];
        assert!(record["error"].as_str().unwrap().contains("retain-me"));
        assert!(record["funding"]["transactionHash"].is_string());
        assert!(record["funding"]["signedTransaction"].is_string());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        handle.stop().unwrap();
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
        prepare_accounts(
            &spec,
            &[actor],
            &hosted_runtime(),
            &url,
            out.path(),
            Duration::from_secs(10),
        )
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
