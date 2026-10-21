//! Exact signed-envelope replay and lost-response recovery through real native HTTP.

use super::{
    history::{Fixture, Result, require},
    *,
};
use sha2::{Digest, Sha256};
use thruput_json_rpc_types::hex::Topic;
use thruput_json_rpc_types::{ContractAddress, bootstrap_manifest::BootstrapManifestAllocation};
use thruput_signing::{
    NATIVE_HOG_ADDRESS, NATIVE_HOG_TRANSFER_ENTRYPOINT, SignTransactionParams,
    default_wallet::{BuildDefaultWalletCallTransaction, build_default_wallet_call_transaction},
    encode_signed_transaction_xdr, native_hog_transfer_calldata, sign_transaction,
    transaction_hash,
};
use tokio::io::AsyncReadExt;

const AMOUNT: u128 = 50_000_000;
const GAS: u64 = 25_000_000;

struct Transfer {
    request: input::SendTransactionRequest,
    hash: TxHash,
    sender: ContractAddress,
    recipient: ContractAddress,
}

fn address(signer: &LocalSigner) -> ContractAddress {
    Base58(PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address())
}

async fn transfer(
    fixture: &Fixture,
    rpc: &HttpClient,
    sender: &LocalSigner,
    recipient: ContractAddress,
) -> Result<Transfer> {
    let tip = rpc
        .request::<output::ChainTip, _>("getChainTip", NamedParams(input::EmptyRequest {}))
        .await?;
    let network = fixture
        .node
        .as_ref()
        .unwrap()
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .unwrap()
        .network_hash
        .to_network_hash();
    let account = PubkeyAccount::from_public_key_bytes(sender.verifying_key_bytes(), 0);
    let calldata = native_hog_transfer_calldata(&recipient.0, AMOUNT)?;
    let tx = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
        account: &account,
        target_address: NATIVE_HOG_ADDRESS,
        target_entrypoint: NATIVE_HOG_TRANSFER_ENTRYPOINT,
        target_calldata: &calldata,
        target_gas_limit: GAS,
        gas_limit: GAS,
        block_expiry_height: *tip.block_height + 100,
        nonce: 1,
    })?;
    let hash = TxHash(Base58(transaction_hash(&tx)?));
    let signed = sign_transaction(tx, sender, SignTransactionParams::default(), &network)?;
    Ok(Transfer {
        request: input::SendTransactionRequest {
            signed_transaction: HexBytes(encode_signed_transaction_xdr(&signed)?),
        },
        hash,
        sender: address(sender),
        recipient,
    })
}

async fn submit(rpc: &HttpClient, transfer: &Transfer) -> Result {
    let result = rpc
        .request::<output::SendTransactionResult, _>(
            "sendTransaction",
            NamedParams(transfer.request.clone()),
        )
        .await?;
    require(
        result.transaction_hash == transfer.hash
            && matches!(result.admission, output::Admission::Accepted),
        "send did not acknowledge admission of the exact signed envelope",
    )
}

async fn balance(
    rpc: &HttpClient,
    address: ContractAddress,
    minimum_height: &BlockHeight,
) -> Result<output::BalanceResult> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = rpc
                .request::<output::BalanceResult, _>(
                    "getBalance",
                    NamedParams(input::AddressQuery { address }),
                )
                .await?;
            require(
                result.address == address,
                "balance belongs to another account",
            )?;
            if result.state_block_height >= *minimum_height {
                break Ok(result);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
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

/// Forward one real JSON-RPC call, then discard its confirmed admission response.
/// Both futures stay in this task, so cancellation also drops the listener/socket.
async fn lose_admission_reply(upstream: &HttpClient, transfer: &Transfer) -> Result {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let client = HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(10))
        .build(format!("http://{}", listener.local_addr()?))?;
    let forward = async {
        let (mut stream, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut buf = [0; 1024];
            let count = stream.read(&mut buf).await?;
            require(
                count > 0 && bytes.len() + count <= 64 * 1024,
                "invalid proxy request headers",
            )?;
            bytes.extend_from_slice(&buf[..count]);
            if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..header_end])?;
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>())
            })
            .ok_or_else(|| std::io::Error::other("proxy requires Content-Length"))??;
        require(length <= 64 * 1024, "oversized proxy request")?;
        while bytes.len() < header_end + length {
            let mut buf = [0; 1024];
            let count = stream.read(&mut buf).await?;
            require(count > 0, "truncated proxy request")?;
            bytes.extend_from_slice(&buf[..count]);
        }
        let call: serde_json::Value =
            serde_json::from_slice(&bytes[header_end..header_end + length])?;
        require(
            call["method"] == "sendTransaction",
            "proxy received an unexpected method",
        )?;
        let request: input::SendTransactionRequest =
            serde_json::from_value(call["params"].clone())?;
        require(
            request.signed_transaction == transfer.request.signed_transaction,
            "proxy changed the signed envelope",
        )?;
        let admitted = upstream
            .request::<output::SendTransactionResult, _>("sendTransaction", NamedParams(request))
            .await?;
        require(
            admitted.transaction_hash == transfer.hash
                && matches!(admitted.admission, output::Admission::Accepted),
            "upstream did not admit the lost-response transaction",
        )?;
        drop(stream); // No downstream HTTP response is written.
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let call = client.request::<output::SendTransactionResult, _>(
        "sendTransaction",
        NamedParams(transfer.request.clone()),
    );
    let (forwarded, observed) = tokio::join!(forward, call);
    forwarded?;
    require(
        matches!(observed, Err(jsonrpsee::core::ClientError::Transport(_))),
        &format!("lost reply did not surface a transport error: {observed:?}"),
    )
}

async fn committed(rpc: &HttpClient, expected: &output::TransactionReceipt) -> Result {
    require(
        serde_json::to_value(receipt(rpc, expected.transaction_hash).await?)?
            == serde_json::to_value(expected)?,
        "committed receipt changed after replay",
    )?;
    require(
        matches!(status(rpc, expected.transaction_hash).await?, output::TransactionStatus::Committed { block_height } if block_height == expected.block_height),
        "status lost the original commitment",
    )
}

async fn history(
    rpc: &HttpClient,
    transfer: &Transfer,
    expected: &output::TransactionReceipt,
    latest: &BlockHeight,
) -> Result {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let page = rpc
                .request::<output::BlockTransactionPage, _>(
                    "getTransactionsByBlock",
                    NamedParams(input::BlockPageRequest {
                        block_height: latest.clone(),
                        start: Some(input::BlockStart {
                            order: IterationOrder::Ascending,
                        }),
                        cursor: None,
                        limit: Some(1),
                    }),
                )
                .await?;
            if page.latest_indexed_block_height >= *latest {
                break Ok::<_, jsonrpsee::core::ClientError>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let root = rpc
        .request::<output::TransactionRefPage, _>(
            "getTransactionsByRootAddress",
            NamedParams(input::RootAddressQuery {
                address: transfer.sender,
                start: Some(input::HistoryStart::Ascending {
                    anchor: input::AscendingAnchor::AtBeginning {},
                }),
                cursor: None,
                limit: Some(10),
            }),
        )
        .await?;
    require(
        root.transactions.len() == 1
            && root.transactions[0].transaction_hash == transfer.hash
            && root.transactions[0].block_height == expected.block_height
            && root.next_cursor.is_none(),
        "root history did not retain exactly one original commitment",
    )?;
    require(
        root.latest_indexed_block_height >= *latest,
        "history has not indexed the replay blocks",
    )?;
    event_history(rpc, transfer, expected, latest).await?;
    for height in *expected.block_height..=**latest {
        let page = rpc
            .request::<output::BlockTransactionPage, _>(
                "getTransactionsByBlock",
                NamedParams(input::BlockPageRequest {
                    block_height: height.into(),
                    start: Some(input::BlockStart {
                        order: IterationOrder::Ascending,
                    }),
                    cursor: None,
                    limit: Some(10),
                }),
            )
            .await?;
        let expected_count = usize::from(height == *expected.block_height);
        require(
            page.transactions.len() == expected_count && page.next_cursor.is_none(),
            "replay appeared in an additional committed block",
        )?;
        if let Some(item) = page.transactions.first() {
            require(
                item.transaction_hash == transfer.hash
                    && item.block_height == expected.block_height,
                "block reference differs from the original receipt",
            )?;
        }
    }
    Ok(())
}

async fn event_history(
    rpc: &HttpClient,
    transfer: &Transfer,
    expected: &output::TransactionReceipt,
    latest: &BlockHeight,
) -> Result {
    let events = rpc
        .request::<output::EventPage, _>(
            "getEvents",
            NamedParams(input::EventQuery {
                filters: vec![input::EventFilter {
                    addresses: vec![Base58(NATIVE_HOG_ADDRESS)],
                    topics: Some(input::TopicFilter {
                        topic1: Some(vec![thruput_json_rpc_types::hex::Topic(
                            transfer.sender.0.as_ref().try_into()?,
                        )]),
                        ..Default::default()
                    }),
                }],
                start: Some(input::HistoryStart::Ascending {
                    anchor: input::AscendingAnchor::AtHeight {
                        block_height: expected.block_height.clone(),
                    },
                }),
                cursor: None,
                limit: Some(10),
            }),
        )
        .await?;
    require(
        events.events.len() == 1 && events.next_cursor.is_none(),
        "replay emitted another Transfer event",
    )?;
    require(
        events.latest_indexed_block_height >= *latest,
        "event scan has not indexed the replay blocks",
    )?;
    let event = &events.events[0];
    // Pin the native-token event wire identity independently of receipt decoding.
    // Canonical TransferEvent: signature, sender, recipient, CXS i128.
    let topics = vec![
        Topic(Sha256::digest(b"0.1::groundhog-erc20-if::TransferEvent").into()),
        Topic(transfer.sender.0.as_ref().try_into()?),
        Topic(transfer.recipient.0.as_ref().try_into()?),
    ];
    require(
        event.event.address == Base58(NATIVE_HOG_ADDRESS)
            && event.event.topics == topics
            && event.event.data.0 == xdrs::cxs::encode(&i128::try_from(AMOUNT)?)?,
        "Transfer event does not match the signed transfer's participants and amount",
    )?;

    require(
        event.transaction_hash == transfer.hash
            && event.block_height == expected.block_height
            && event.sequence_number == 0
            && serde_json::to_value(&expected.events)?
                == serde_json::to_value(vec![event.event.clone()])?,
        "event identity or payload differs from the original receipt",
    )?;
    Ok(())
}

async fn expected_sender_balance(rpc: &HttpClient, before: i128) -> Result<i128> {
    let costs = rpc
        .request::<output::ChainConfig, _>("getChainConfig", NamedParams(input::EmptyRequest {}))
        .await?
        .execution
        .ok_or_else(|| std::io::Error::other("authoritative storage costs missing"))?
        .storage_costs;
    // Each bootstrap-funded sender has no prior transactions: its first commitment
    // creates one replay-cache item. Both balance rows already exist; gas is metering.
    Ok(before
        - i128::try_from(AMOUNT)?
        - i128::from(costs.base_data_item_cost)
        - i128::from(costs.replay_cache_cost_per_entry))
}

async fn exercise_case(
    fixture: &Fixture,
    clients: [&HttpClient; 2],
    submitter: usize,
    sender: &LocalSigner,
    recipient: ContractAddress,
    lost_reply: bool,
) -> Result {
    let rpc = clients[submitter];
    let tx = transfer(fixture, rpc, sender, recipient).await?;
    let before = balance(rpc, recipient, &BlockHeight::from(0)).await?;
    let sender_before = balance(rpc, tx.sender, &before.state_block_height).await?;
    let expected_sender_balance = expected_sender_balance(rpc, sender_before.raw_balance).await?;
    require(
        receipt(rpc, tx.hash).await?.is_none(),
        "unsigned fixture already has a committed receipt",
    )?;
    if lost_reply {
        lose_admission_reply(rpc, &tx).await?;
    } else {
        for _ in 0..3 {
            submit(rpc, &tx).await?;
        }
    }
    // A successful admission (including the reply lost above) is not a receipt.
    for client in clients {
        require(
            receipt(client, tx.hash).await?.is_none(),
            "admission fabricated commitment before a manual block",
        )?;
        require(
            matches!(
                status(client, tx.hash).await?,
                output::TransactionStatus::Pending { .. }
            ),
            "admitted transfer is not pending",
        )?;
        require(
            balance(client, recipient, &before.state_block_height)
                .await?
                .raw_balance
                == before.raw_balance,
            "pending transfer changed committed balance",
        )?;
        let pending_sender = balance(client, tx.sender, &sender_before.state_block_height).await?;
        require(
            pending_sender.raw_balance == sender_before.raw_balance,
            "pending submissions charged the sender",
        )?;
    }
    let height = produce(fixture).await?;
    // Lost-response recovery uses the locally computed hash and read methods only.
    let original = wait_receipt(rpc, tx.hash).await?;
    require(
        original.transaction_hash == tx.hash
            && original.block_height == height
            && matches!(original.execution, output::ReceiptExecution::Return { .. })
            && original.events.len() == 1,
        "original transfer did not execute once successfully",
    )?;
    for client in clients {
        let committed_sender = balance(client, tx.sender, &original.block_height).await?;
        require(
            committed_sender.raw_balance == expected_sender_balance,
            "first commitment did not charge exactly one transfer and replay-cache entry",
        )?;
    }
    let expected_balance = before.raw_balance + i128::try_from(AMOUNT)?;
    let mut latest = height;
    for _ in 0..2 {
        if !lost_reply {
            // Admission remains attempt-local: a post-commit repeat can be accepted.
            for client in clients {
                submit(client, &tx).await?;
                committed(client, &original).await?;
            }
        }
        latest = produce(fixture).await?;
        for client in clients {
            committed(client, &original).await?;
            require(
                balance(client, recipient, &latest).await?.raw_balance == expected_balance
                    && balance(client, tx.sender, &latest).await?.raw_balance
                        == expected_sender_balance,
                "replay changed committed token balances",
            )?;
        }
    }
    for client in clients {
        history(client, &tx, &original, &latest).await?;
    }
    eprintln!(
        "qualified {} via {}: hash {:?}, original height {}",
        if lost_reply {
            "lost-response recovery without redispatch"
        } else {
            "pre/post-commit exact-envelope replay"
        },
        if submitter == 0 {
            "local HTTP"
        } else {
            "gateway HTTP"
        },
        tx.hash,
        *original.block_height
    );
    Ok(())
}

async fn exercise(fixture: &mut Fixture, signers: &[LocalSigner]) -> Result {
    let (local, gateway) = fixture.start().await?;
    for (index, sender) in signers[..4].iter().enumerate() {
        exercise_case(
            fixture,
            [&local, &gateway],
            index % 2,
            sender,
            address(&signers[index + 4]),
            index >= 2,
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires published THRUPUT_DEVNODE_BINARY and THRUPUT_GATEWAY_BINARY"]
async fn real_duplicate_submission_and_lost_reply_match_both_servers() {
    let signers: Vec<_> = (0x81..=0x88)
        .map(|seed| LocalSigner::from_secret_key([seed; 32]))
        .collect();
    let mut fixture = Fixture::new().unwrap();
    fixture.config.hogd_dev.bootstrap_accounts = signers
        .iter()
        .map(|signer| {
            BootstrapManifestAllocation::new(
                Base58(signer.verifying_key_bytes().into()),
                "1000000000000",
            )
        })
        .collect();
    let result =
        tokio::time::timeout(Duration::from_mins(3), exercise(&mut fixture, &signers)).await;
    let cleanup = fixture.stop().await;
    if !matches!(&result, Ok(Ok(()))) || cleanup.is_err() {
        eprintln!("duplicate qualification failed: {result:?}; cleanup: {cleanup:?}");
        fixture.print_diagnostics();
    }
    cleanup.unwrap();
    result.unwrap().unwrap();
}
