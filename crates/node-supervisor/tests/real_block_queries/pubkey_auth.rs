//! ML-DSA-65 pubkey-account admission, commitment and history through real native HTTP.

use super::{
    history::{Fixture, Result, require},
    *,
};
use sha2::{Digest, Sha256};
use thruput_json_rpc_types::ContractAddress;
use thruput_signing::{
    MlDsa65Signer, NATIVE_TOKEN_TRANSFER_ENTRYPOINT, SignTransactionParams, Signer as _,
    default_wallet::{BuildDefaultWalletCallTransaction, build_default_wallet_call_transaction},
    encode_signed_transaction_xdr, encode_transaction_xdr, native_token_transfer_calldata,
    sign_transaction, transaction_hash,
};
use thruput_xdr::genesis_addresses::ADDRESS_NATIVE_TOKEN;
use thruput_xdr::{
    auth_claim::{AuthClaim, AuthClaim_source_address_claim},
    transaction::SignedTransaction,
    types::NetworkHash,
};

const FUNDING: u128 = 1_000_000_000_000;
const AMOUNT: u128 = 50_000_000;
const GAS: u64 = 25_000_000;

/// A rejected envelope's observable classification: JSON-RPC code plus the
/// `kind`, `reason` and `admission` fields of the native error data.
#[derive(Debug, PartialEq, Eq)]
struct Rejection {
    code: i32,
    kind: Option<String>,
    reason: Option<String>,
    admission: Option<String>,
}

impl Rejection {
    fn new(code: i32, kind: &str, reason: Option<&str>, admission: &str) -> Self {
        Self {
            code,
            kind: Some(kind.into()),
            reason: reason.map(Into::into),
            admission: Some(admission.into()),
        }
    }
}

fn network(fixture: &Fixture) -> NetworkHash {
    fixture
        .node
        .as_ref()
        .unwrap()
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .unwrap()
        .network_hash
        .to_network_hash()
}

async fn fund(fixture: &Fixture, rpc: &HttpClient, recipient: ContractAddress) -> Result {
    let faucet = fixture
        .node
        .as_ref()
        .unwrap()
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .unwrap()
        .bootstrap_accounts[0]
        .wallet_address;
    let funding = rpc
        .request::<output::FundingSubmission, _>(
            "dev_fundAccount",
            NamedParams(FundAccountRequest {
                recipient,
                faucet,
                raw_amount: FUNDING,
                client_nonce: ClientNonce::new(vec![0x50]).unwrap(),
            }),
        )
        .await?;
    produce(fixture).await?;
    let receipt = wait_receipt(rpc, funding.transaction_hash).await?;
    require(
        matches!(receipt.execution, output::ReceiptExecution::Return { .. }),
        "funding the ML-DSA-65 account did not execute",
    )
}

async fn produce(fixture: &Fixture) -> Result<BlockHeight> {
    Ok(fixture
        .node
        .as_ref()
        .unwrap()
        .manual_block_producer()
        .unwrap()
        .produce_block()
        .await?
        .height
        .into())
}

async fn receipt(rpc: &HttpClient, hash: TxHash) -> Result<output::ReceiptResult> {
    Ok(rpc
        .request(
            "getTransactionReceipt",
            NamedParams(input::TransactionQuery {
                transaction_hash: hash,
            }),
        )
        .await?)
}

async fn status(rpc: &HttpClient, hash: TxHash) -> Result<output::TransactionStatus> {
    Ok(rpc
        .request(
            "getTransactionStatus",
            NamedParams(input::TransactionQuery {
                transaction_hash: hash,
            }),
        )
        .await?)
}

async fn wait_receipt(rpc: &HttpClient, hash: TxHash) -> Result<output::TransactionReceipt> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(receipt) = receipt(rpc, hash).await? {
                break Ok::<_, Box<dyn std::error::Error>>(receipt);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}

async fn balance(
    rpc: &HttpClient,
    address: ContractAddress,
    minimum: &BlockHeight,
) -> Result<i128> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = rpc
                .request::<output::BalanceResult, _>(
                    "getBalance",
                    NamedParams(input::AddressQuery { address }),
                )
                .await?;
            if result.state_block_height >= *minimum {
                break Ok(result.raw_balance);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}

fn request(signed: &SignedTransaction) -> Result<input::SendTransactionRequest> {
    Ok(input::SendTransactionRequest {
        signed_transaction: HexBytes(encode_signed_transaction_xdr(signed)?),
    })
}

async fn reject(rpc: &HttpClient, signed: &SignedTransaction) -> Result<Rejection> {
    let error = rpc
        .request::<output::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(request(signed)?),
        )
        .await
        .err()
        .ok_or_else(|| std::io::Error::other("malformed pubkey envelope was admitted"))?;
    let jsonrpsee::core::ClientError::Call(error) = error else {
        return Err(format!("rejection was not a JSON-RPC error: {error:?}").into());
    };
    let data: serde_json::Value = match error.data() {
        Some(data) => serde_json::from_str(data.get())?,
        None => serde_json::Value::Null,
    };
    let field = |name: &str| data.get(name).and_then(|v| v.as_str()).map(str::to_owned);
    Ok(Rejection {
        code: error.code(),
        kind: field("kind"),
        reason: field("reason"),
        admission: field("admission"),
    })
}

/// Each malformed envelope must fail admission identically through both adapters,
/// without consuming the account nonce the valid transfer later uses.
async fn negative_cases(
    clients: [&HttpClient; 2],
    valid: &SignedTransaction,
    signer: &MlDsa65Signer,
    network: &NetworkHash,
) -> Result {
    let mut missing = valid.clone();
    missing.pubkey_claim_key = None;

    let mut source_claim = valid.clone();
    source_claim.tx.auth_claim = AuthClaim::kSourceAddressAuthData {
        source_address_claim: AuthClaim_source_address_claim {
            allowed_keys: 1,
            required_weight: 1,
        },
    };

    let mut substituted = valid.clone();
    substituted.pubkey_claim_key = Some(MlDsa65Signer::from_seed(&[0x5b; 32]).public_key());

    let other_network = NetworkHash::new(thruput_xdr::types::SHA256Hash::new([0xee; 32]));
    require(
        *network != other_network,
        "wrong-network fixture collides with the real network",
    )?;
    let wrong_network = sign_transaction(
        valid.tx.clone(),
        signer,
        SignTransactionParams::default(),
        &other_network,
    )?;

    let mut tampered_signature = valid.clone();
    let thruput_xdr::types::Signature::MlDsa65 { mldsa65 } =
        &mut tampered_signature.signatures.0[0].signature
    else {
        return Err("valid envelope does not carry an ML-DSA-65 signature".into());
    };
    mldsa65[0] ^= 1;

    // Key presence is checked before dispatch and currently carries no reason, although
    // the catalogue offers `invalidAuthEnvelope`. Key, signature and network mismatches
    // reach hogd and come back as an authentication rejection.
    let invalid = || Rejection::new(-32000, "transactionInvalid", None, "notAttempted");
    let rejected = || {
        Rejection::new(
            -32001,
            "transactionRejected",
            Some("authenticationRejected"),
            "rejected",
        )
    };
    let cases = [
        ("missing pubkey_claim_key", missing, invalid()),
        (
            "pubkey_claim_key on a source-address claim",
            source_claim,
            invalid(),
        ),
        ("substituted pubkey_claim_key", substituted, rejected()),
        ("wrong-network signature", wrong_network, rejected()),
        (
            "tampered ML-DSA-65 signature",
            tampered_signature,
            rejected(),
        ),
    ];
    for (name, signed, expected) in cases {
        for (index, client) in clients.into_iter().enumerate() {
            let observed = reject(client, &signed).await?;
            eprintln!(
                "{name} via {}: {observed:?}",
                if index == 0 {
                    "local HTTP"
                } else {
                    "gateway HTTP"
                }
            );
            require(
                observed == expected,
                &format!("{name}: expected {expected:?}, observed {observed:?}"),
            )?;
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one qualification follows a single ML-DSA-65 transfer from funding to history"
)]
async fn exercise(fixture: &mut Fixture) -> Result {
    let (local, gateway) = fixture.start().await?;
    let clients = [&local, &gateway];
    let network = network(fixture);
    let signer = MlDsa65Signer::from_seed(&[0x5a; 32]);
    let account = PubkeyAccount::new(signer.public_key(), 0);
    let sender = Base58(account.address());
    let recipient = Base58(
        PubkeyAccount::from_public_key_bytes(
            LocalSigner::from_secret_key([0x5c; 32]).verifying_key_bytes(),
            0,
        )
        .address(),
    );
    fund(fixture, &local, sender).await?;

    let tip = local
        .request::<output::ChainTip, _>("getChainTip", NamedParams(input::EmptyRequest {}))
        .await?;
    let calldata = native_token_transfer_calldata(&recipient.0, AMOUNT)?;
    let tx = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
        account: &account,
        target_address: ADDRESS_NATIVE_TOKEN,
        target_entrypoint: NATIVE_TOKEN_TRANSFER_ENTRYPOINT,
        target_calldata: &calldata,
        target_gas_limit: GAS,
        gas_limit: GAS,
        block_expiry_height: *tip.block_height + 100,
        nonce: 1,
    })?;
    // The canonical hash covers only the unsigned body, never the 1952-byte key
    // or 3309-byte signature the envelope adds.
    let hash = TxHash(Base58(transaction_hash(&tx)?));
    require(
        hash.0.0 == <[u8; 32]>::from(Sha256::digest(encode_transaction_xdr(&tx)?)).into(),
        "transaction hash is not sha256 of the unsigned body",
    )?;
    let envelope = sign_transaction(tx, &signer, SignTransactionParams::default(), &network)?;
    require(
        envelope.pubkey_claim_key == Some(signer.public_key()),
        "ML-DSA-65 envelope does not carry its claimed key",
    )?;

    negative_cases(clients, &envelope, &signer, &network).await?;

    let costs = local
        .request::<output::ChainConfig, _>("getChainConfig", NamedParams(input::EmptyRequest {}))
        .await?
        .execution
        .ok_or_else(|| std::io::Error::other("authoritative storage costs missing"))?
        .storage_costs;
    let amount = i128::try_from(AMOUNT)?;
    let balance_cost = i128::from(costs.balance_cost);
    require(
        balance(&local, sender, &tip.block_height).await?
            == i128::try_from(FUNDING)? - balance_cost
            && balance(&local, recipient, &tip.block_height).await? == 0,
        "ML-DSA-65 account funding did not charge exactly one new balance row",
    )?;
    // The recipient's new balance row is paid from the transfer. The sender's first
    // commitment creates one replay-cache item, as for an Ed25519 pubkey account.
    let expected_recipient = amount - balance_cost;
    let expected_sender = i128::try_from(FUNDING)?
        - balance_cost
        - amount
        - i128::from(costs.base_data_item_cost)
        - i128::from(costs.replay_cache_cost_per_entry);
    let admitted = gateway
        .request::<output::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(request(&envelope)?),
        )
        .await?;
    require(
        admitted.transaction_hash == hash
            && matches!(admitted.admission, output::Admission::Accepted),
        "ML-DSA-65 transfer was not admitted under its unsigned-body hash",
    )?;
    for client in clients {
        require(
            matches!(
                status(client, hash).await?,
                output::TransactionStatus::Pending { .. }
            ),
            "admitted ML-DSA-65 transfer is not pending",
        )?;
    }
    let height = produce(fixture).await?;
    let committed = wait_receipt(&local, hash).await?;
    require(
        committed.transaction_hash == hash
            && committed.block_height == height
            && matches!(committed.execution, output::ReceiptExecution::Return { .. })
            && committed.events.len() == 1,
        "ML-DSA-65 transfer did not execute once successfully",
    )?;
    for client in clients {
        require(
            serde_json::to_value(receipt(client, hash).await?)?
                == serde_json::to_value(Some(&committed))?,
            "adapters disagree on the ML-DSA-65 receipt",
        )?;
        require(
            matches!(status(client, hash).await?, output::TransactionStatus::Committed { block_height } if block_height == height),
            "status does not report the ML-DSA-65 commitment",
        )?;
        require(
            balance(client, recipient, &height).await? == expected_recipient
                && balance(client, sender, &height).await? == expected_sender,
            "ML-DSA-65 transfer did not move exactly the transfer and storage charges",
        )?;
    }
    produce(fixture).await?;
    for client in clients {
        history(client, sender, hash, &height).await?;
    }
    eprintln!(
        "qualified ML-DSA-65 pubkey transfer: hash {hash:?}, height {}",
        *height
    );
    Ok(())
}

async fn history(
    rpc: &HttpClient,
    sender: ContractAddress,
    hash: TxHash,
    height: &BlockHeight,
) -> Result {
    let root = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let page = rpc
                .request::<output::AddressTransactionPage, _>(
                    "getTransactionsByAddress",
                    NamedParams(input::AddressHistoryQuery {
                        address: sender,
                        start: Some(input::HistoryStart::Ascending {
                            anchor: input::AscendingAnchor::AtBeginning {},
                        }),
                        cursor: None,
                        limit: Some(10),
                    }),
                )
                .await?;
            if page.coverage.through_block_height >= *height {
                break Ok::<_, jsonrpsee::core::ClientError>(page);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    require(
        root.transactions.len() == 1
            && root.transactions[0].transaction_hash == hash
            && root.transactions[0].block_height == *height,
        "root history does not hold exactly the ML-DSA-65 transfer",
    )?;
    let block = rpc
        .request::<output::BlockTransactionPage, _>(
            "getTransactionsByBlock",
            NamedParams(input::BlockPageRequest {
                block_height: height.clone(),
                start: Some(input::BlockStart {
                    order: IterationOrder::Ascending,
                }),
                cursor: None,
                limit: Some(10),
            }),
        )
        .await?;
    require(
        block.transactions.len() == 1 && block.transactions[0].transaction_hash == hash,
        "block history does not hold the ML-DSA-65 transfer",
    )
}

#[tokio::test]
#[ignore = "requires published THRUPUT_DEVNODE_BINARY and THRUPUT_GATEWAY_BINARY"]
async fn real_mldsa65_pubkey_transfer_and_envelope_rejections_match_both_servers() {
    let mut fixture = Fixture::new().unwrap();
    let result = tokio::time::timeout(Duration::from_mins(3), exercise(&mut fixture)).await;
    let cleanup = fixture.stop().await;
    if !matches!(&result, Ok(Ok(()))) || cleanup.is_err() {
        eprintln!("ML-DSA-65 qualification failed: {result:?}; cleanup: {cleanup:?}");
        fixture.print_diagnostics();
    }
    cleanup.unwrap();
    result.unwrap().unwrap();
}
