//! Opt-in real runtime coverage for the candidate native connector.
#![cfg(feature = "native-rpc-qualification")]

use std::{error::Error, path::PathBuf, time::Duration};

use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use thruput_json_rpc_api::GroundhogApiClient;
use thruput_json_rpc_client::native::{
    NativeClient, NativeClientError,
    receipt::{ReceiptPollConfig, poll_receipt},
};
use thruput_json_rpc_server::RpcServerConfig;
use thruput_json_rpc_types::{
    ContractAddress,
    base58::Base58,
    bootstrap_manifest::BootstrapManifestAllocation,
    error::native::{DecodedError, ErrorData},
    events::IterationOrder,
    hex::HexBytes,
    requests as input, responses as output,
    transaction::TxHash,
};
use thruput_node_supervisor::{
    BlockDriverConfig, HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle,
};
use thruput_signing::{
    LocalSigner, NATIVE_HOG_ADDRESS, NATIVE_HOG_TRANSFER_ENTRYPOINT, PubkeyAccount,
    SignTransactionParams,
    default_wallet::{BuildDefaultWalletCallTransaction, build_default_wallet_call_transaction},
    encode_signed_transaction_xdr, native_hog_transfer_calldata, sign_transaction,
    transaction_hash,
};

const AMOUNT: u128 = 50_000_000;
const GAS: u64 = 25_000_000;
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}

fn known<T>(result: output::DecodedResponse<T>) -> TestResult<T> {
    match result {
        output::DecodedResponse::Known(value) => Ok(value),
        other => {
            let raw = match other {
                output::DecodedResponse::Unsupported { raw } => raw.to_string(),
                _ => "unrecognized result envelope".into(),
            };
            Err(std::io::Error::other(format!("unsupported native result: {raw}")).into())
        }
    }
}

fn random_signer() -> TestResult<LocalSigner> {
    let mut secret = zeroize::Zeroizing::new([0; 32]);
    getrandom::fill(secret.as_mut()).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(LocalSigner::from_secret_key(*secret))
}

fn transport(address: std::net::SocketAddr) -> TestResult<HttpClient> {
    Ok(HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(30))
        .build(format!("http://{address}"))?)
}

#[tokio::test]
#[ignore = "requires an absolute THRUPUT_DEVNODE_BINARY for a compatible real runtime"]
async fn real_native_submit_receipt_balance_and_simulation() {
    let binary = PathBuf::from(
        std::env::var_os("THRUPUT_DEVNODE_BINARY")
            .expect("THRUPUT_DEVNODE_BINARY must name a compatible runtime"),
    );
    assert!(binary.is_absolute() && binary.is_file());
    let artifacts = binary.parent().unwrap().join("contracts");
    let sender = random_signer().unwrap();
    let node = NodeSupervisorHandle::start(
        NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_block_driver(BlockDriverConfig::manual())
            .with_hogd_dev(
                HogdDevConfig::default()
                    .with_binary_path(binary)
                    .with_contract_artifact_directories([artifacts])
                    .with_bootstrap_accounts(vec![BootstrapManifestAllocation::new(
                        Base58(sender.verifying_key_bytes().into()),
                        "1000000000000",
                    )]),
            ),
    )
    .await
    .unwrap();
    // The helper returns errors instead of panicking, so both listeners and the
    // runtime are stopped before the test reports a failed assertion.
    let outcome = qualify(&node, &sender).await;
    node.stop().await.unwrap();
    outcome.unwrap();
}

async fn qualify(node: &NodeSupervisorHandle, sender: &LocalSigner) -> TestResult {
    let candidate = node
        .start_native_qualification(RpcServerConfig::default().with_view_gas_limit(GAS))
        .await?;
    let outcome = exercise(node, candidate.rpc_addr(), sender).await;
    candidate.stop().await;
    outcome
}

async fn exercise(
    node: &NodeSupervisorHandle,
    candidate_addr: std::net::SocketAddr,
    sender: &LocalSigner,
) -> TestResult {
    let legacy = transport(node.rpc_addr())?;
    require(
        legacy.version().await?.wire_format == "13.0.0",
        "legacy endpoint changed",
    )?;
    let native = NativeClient::new(transport(candidate_addr)?);
    check_discovery(&native).await?;
    let network = known(native.get_network_info(input::EmptyRequest {}).await?)?.network_hash;
    let signing_network = node
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .ok_or_else(|| std::io::Error::other("validated runtime manifest missing"))?
        .network_hash
        .to_network_hash();
    require(
        network.0 == signing_network.clone().into_inner(),
        "candidate network differs from validated manifest",
    )?;
    let recipient = random_signer()?;
    let recipient =
        Base58(PubkeyAccount::from_public_key_bytes(recipient.verifying_key_bytes(), 0).address());
    let account = PubkeyAccount::from_public_key_bytes(sender.verifying_key_bytes(), 0);
    let calldata = native_hog_transfer_calldata(&recipient.0, AMOUNT)?;
    let costs = known(native.get_chain_config(input::EmptyRequest {}).await?)?
        .execution
        .ok_or_else(|| std::io::Error::other("authoritative execution config missing"))?
        .storage_costs;
    let expected_free_balance = i128::try_from(AMOUNT)? - i128::from(costs.balance_cost);
    let receipt =
        send_transfer(node, &native, sender, recipient, signing_network.clone(), 1).await?;
    require(
        matches!(receipt.execution, output::ReceiptExecution::Return { .. }),
        "transfer execution did not return successfully",
    )?;
    require(
        receipt
            .events
            .iter()
            .any(|event| event.address == Base58(NATIVE_HOG_ADDRESS) && event.topics.len() == 3),
        "transfer receipt omitted its token event",
    )?;
    require(receipt.gas_used > 0, "receipt omitted metering")?;
    check_block(&native, &legacy, &receipt.block_height, &network).await?;
    let funded = known(
        native
            .get_balance(input::AddressQuery { address: recipient })
            .await?,
    )?;
    require(
        funded.address == recipient && funded.raw_balance == expected_free_balance,
        &format!(
            "recipient balance does not reflect exactly one transfer: receipt={receipt:?}; balance={funded:?}; expected_address={recipient:?}; expected_free_balance={expected_free_balance}"
        ),
    )?;
    require(
        funded.state_block_height == receipt.block_height,
        "balance snapshot differs from the only submitted transaction",
    )?;
    check_simulation(
        &native,
        Base58(account.address()),
        recipient,
        calldata,
        funded,
    )
    .await?;
    let second =
        send_transfer(node, &native, sender, recipient, signing_network.clone(), 2).await?;
    check_history(
        &native,
        Base58(account.address()),
        recipient,
        [&receipt, &second],
    )
    .await?;
    check_block_continuations(node, &native, sender, recipient, signing_network).await?;
    require(
        legacy.version().await?.wire_format == "13.0.0",
        "wire 13 stopped serving",
    )
}

async fn send_transfer(
    node: &NodeSupervisorHandle,
    native: &NativeClient<HttpClient>,
    sender: &LocalSigner,
    recipient: ContractAddress,
    signing_network: thruput_xdr::types::NetworkHash,
    nonce: u64,
) -> TestResult<output::TransactionReceipt> {
    let hash = submit_transfer(native, sender, recipient, signing_network.clone(), nonce).await?;
    node.manual_block_producer()
        .ok_or_else(|| std::io::Error::other("manual block producer missing"))?
        .produce_block()
        .await?;
    wait_for_receipt(native, signing_network, hash).await
}

async fn submit_transfer(
    native: &NativeClient<HttpClient>,
    sender: &LocalSigner,
    recipient: ContractAddress,
    signing_network: thruput_xdr::types::NetworkHash,
    nonce: u64,
) -> TestResult<TxHash> {
    let account = PubkeyAccount::from_public_key_bytes(sender.verifying_key_bytes(), 0);
    let calldata = native_hog_transfer_calldata(&recipient.0, AMOUNT)?;
    let tip = known(native.get_chain_tip(input::EmptyRequest {}).await?)?;
    let tx = build_default_wallet_call_transaction(BuildDefaultWalletCallTransaction {
        account: &account,
        target_address: NATIVE_HOG_ADDRESS,
        target_entrypoint: NATIVE_HOG_TRANSFER_ENTRYPOINT,
        target_calldata: &calldata,
        target_gas_limit: GAS,
        gas_limit: GAS,
        block_expiry_height: *tip.block_height + 100,
        nonce,
    })?;
    let expected_hash = TxHash(Base58(transaction_hash(&tx)?));
    let signed = sign_transaction(
        tx,
        sender,
        SignTransactionParams::default(),
        &signing_network,
    )?;
    // Exactly one send. The waiter below has only read methods available.
    let admitted = known(
        native
            .send_transaction(input::SendTransactionRequest {
                signed_transaction: HexBytes(encode_signed_transaction_xdr(&signed)?),
            })
            .await?,
    )?;
    require(
        admitted.transaction_hash == expected_hash,
        "admission hash mismatch",
    )?;
    require(
        matches!(admitted.admission, output::Admission::Accepted),
        "missing admission",
    )?;
    Ok(expected_hash)
}

async fn wait_for_receipt(
    native: &NativeClient<HttpClient>,
    signing_network: thruput_xdr::types::NetworkHash,
    expected_hash: TxHash,
) -> TestResult<output::TransactionReceipt> {
    let receipt = poll_receipt(
        native,
        Base58(signing_network.clone().into_inner()),
        expected_hash,
        None,
        ReceiptPollConfig {
            timeout: Duration::from_secs(30),
            interval: Duration::from_millis(50),
        },
        std::future::pending(),
    )
    .await?;
    Ok(receipt)
}

async fn check_discovery(native: &NativeClient<HttpClient>) -> TestResult {
    let version = known(native.get_version_info(input::EmptyRequest {}).await?)?;
    require(
        version.wire_format == "14.0.0",
        "candidate wire version mismatch",
    )?;
    let mut expected_methods = [
        "getVersionInfo",
        "getNetworkInfo",
        "getServerLimits",
        "getChainTip",
        "getChainConfig",
        "getStorage",
        "getBalance",
        "getRegisteredAuth",
        "simulateInvocation",
        "simulateInvocations",
        "sendTransaction",
        "getTransactionStatus",
        "getTransactionReceipt",
        "getBlockByHeight",
        "getEvents",
        "getTransactionsByRootAddress",
        "getTransactionsByBlock",
    ];
    expected_methods.sort_unstable();
    require(
        version.supported_methods == expected_methods,
        "candidate method map mismatch",
    )?;
    require(
        version.implementation == env!("CARGO_PKG_VERSION"),
        "implementation version mismatch",
    )?;
    require(
        version.release_version.is_none() && version.bundle_manifest_digest.is_none(),
        "candidate claimed an installed release identity",
    )
}

async fn check_simulation(
    native: &NativeClient<HttpClient>,
    sender: ContractAddress,
    recipient: ContractAddress,
    calldata: Vec<u8>,
    before: output::BalanceResult,
) -> TestResult {
    // Native HOG transfer debits GetMsgSender. Explicit caller supplies that
    // frame context; this direct token invocation needs no assumed signature.
    let result = known(
        native
            .simulate_invocation(input::SimulateInvocationRequest {
                address: Base58(NATIVE_HOG_ADDRESS),
                entrypoint: HexBytes(NATIVE_HOG_TRANSFER_ENTRYPOINT.as_ref().to_vec()),
                calldata: HexBytes(calldata),
                caller: Some(sender),
                assumed_auth: None,
                gas_limit: Some(GAS),
                min_state_block_height: Some(before.state_block_height.clone()),
            })
            .await?,
    )?;
    require(
        matches!(result.output, output::InvocationOutput::Return { .. }),
        "token simulation did not return successfully",
    )?;
    require(
        !result.events.is_empty() && result.gas_used > 0,
        "simulation omitted events or gas",
    )?;
    require(
        result.state_block_height == before.state_block_height,
        "simulation snapshot mismatch",
    )?;
    let after = known(
        native
            .get_balance(input::AddressQuery { address: recipient })
            .await?,
    )?;
    require(
        after.raw_balance == before.raw_balance
            && after.state_block_height == before.state_block_height,
        "simulation mutated committed recipient balance",
    )
}

async fn check_block(
    native: &NativeClient<HttpClient>,
    legacy: &HttpClient,
    height: &thruput_json_rpc_types::BlockHeight,
    network: &thruput_json_rpc_types::NetworkHash,
) -> TestResult {
    let block = known(
        native
            .get_block_by_height(input::BlockQuery {
                block_height: height.clone(),
            })
            .await?,
    )?
    .ok_or_else(|| std::io::Error::other("committed receipt block is missing"))?;
    let legacy_block = legacy
        .get_block_by_number(height.clone().into())
        .await?
        .ok_or_else(|| std::io::Error::other("legacy receipt block is missing"))?;
    require(
        block.block_height == *height && block.network_hash == *network,
        "block metadata does not match receipt height and signing network",
    )?;
    require(
        serde_json::to_value(&block)? == serde_json::to_value(&legacy_block)?,
        "native block changed the canonical consensus metadata",
    )?;
    let committed_tip = known(native.get_chain_tip(input::EmptyRequest {}).await?)?;
    require(
        block.block_height == committed_tip.block_height
            && block.block_hash == committed_tip.block_hash,
        "indexer block hash differs from the independently read DB tip",
    )?;
    for height in [0, **height + 100] {
        require(
            known(
                native
                    .get_block_by_height(input::BlockQuery {
                        block_height: height.into(),
                    })
                    .await?,
            )?
            .is_none(),
            "block outside authoritative coverage was not null",
        )?;
    }
    Ok(())
}

fn history_start() -> input::HistoryStart {
    input::HistoryStart::Ascending {
        anchor: input::AscendingAnchor::AtBeginning {},
    }
}

fn require_invalid_cursor<T: std::fmt::Debug>(result: Result<T, NativeClientError>) -> TestResult {
    match result {
        Err(NativeClientError::Application { data, .. })
            if matches!(data.as_ref(), DecodedError::Known { data, .. }
                if matches!(data.as_ref(), ErrorData::InvalidCursor { .. })) =>
        {
            Ok(())
        }
        other => Err(std::io::Error::other(format!(
            "changed history query did not produce typed invalidCursor: {other:?}"
        ))
        .into()),
    }
}

async fn check_history(
    native: &NativeClient<HttpClient>,
    sender: ContractAddress,
    recipient: ContractAddress,
    receipts: [&output::TransactionReceipt; 2],
) -> TestResult {
    check_root_history(native, sender, recipient, receipts).await?;
    check_event_history(native, recipient, receipts).await?;
    check_block_history(native, receipts).await
}

async fn check_root_history(
    native: &NativeClient<HttpClient>,
    sender: ContractAddress,
    recipient: ContractAddress,
    receipts: [&output::TransactionReceipt; 2],
) -> TestResult {
    let mut root_query = input::RootAddressQuery {
        address: sender,
        start: Some(history_start()),
        cursor: None,
        limit: Some(1),
    };
    let first = known(
        native
            .get_transactions_by_root_address(root_query.clone())
            .await?,
    )?;
    require(
        first.transactions.len() == 1
            && first.transactions[0].transaction_hash == receipts[0].transaction_hash,
        "root history did not start with the first wallet transaction",
    )?;
    let cursor = first.next_cursor.ok_or_else(|| {
        std::io::Error::other("root history omitted continuation before the second transaction")
    })?;
    root_query.start = None;
    root_query.cursor = Some(cursor);
    let mut changed = root_query.clone();
    changed.address = recipient;
    require_invalid_cursor(native.get_transactions_by_root_address(changed).await)?;
    root_query.limit = Some(10);
    let rest = known(native.get_transactions_by_root_address(root_query).await?)?;
    require(
        rest.transactions.len() == 1
            && rest.transactions[0].transaction_hash == receipts[1].transaction_hash
            && rest.next_cursor.is_none(),
        "root continuation duplicated or omitted a transaction, or ignored a changed limit",
    )?;
    let mut descending = input::RootAddressQuery {
        address: sender,
        start: Some(input::HistoryStart::Descending {
            anchor: input::DescendingAnchor::AtEnd {},
        }),
        cursor: None,
        limit: Some(1),
    };
    let last = known(
        native
            .get_transactions_by_root_address(descending.clone())
            .await?,
    )?;
    require(
        last.transactions.len() == 1
            && last.transactions[0].transaction_hash == receipts[1].transaction_hash,
        "descending root history did not start with the latest transaction",
    )?;
    descending.start = None;
    descending.cursor = Some(last.next_cursor.ok_or_else(|| {
        std::io::Error::other("descending root history omitted its continuation")
    })?);
    let previous = known(native.get_transactions_by_root_address(descending).await?)?;
    require(
        previous.transactions.len() == 1
            && previous.transactions[0].transaction_hash == receipts[0].transaction_hash
            && previous.next_cursor.is_none(),
        "root continuation lost descending order",
    )?;
    let incoming = known(
        native
            .get_transactions_by_root_address(input::RootAddressQuery {
                address: recipient,
                start: Some(history_start()),
                cursor: None,
                limit: Some(10),
            })
            .await?,
    )?;
    require(
        incoming.transactions.is_empty() && incoming.next_cursor.is_none(),
        "root history included incoming transfers",
    )?;
    Ok(())
}

async fn check_event_history(
    native: &NativeClient<HttpClient>,
    recipient: ContractAddress,
    receipts: [&output::TransactionReceipt; 2],
) -> TestResult {
    let mut event_query = input::EventQuery {
        filters: vec![input::EventFilter {
            addresses: vec![Base58(NATIVE_HOG_ADDRESS)],
            topics: None,
        }],
        // Bootstrap transactions also emit token events. Start at the first
        // transfer so this walk can be compared exactly with its two receipts.
        start: Some(input::HistoryStart::Ascending {
            anchor: input::AscendingAnchor::AtHeight {
                block_height: receipts[0].block_height.clone(),
            },
        }),
        cursor: None,
        limit: Some(1),
    };
    let first = known(native.get_events(event_query.clone()).await?)?;
    require(
        first.events.len() == 1,
        "event page did not honor limit one",
    )?;
    let cursor = first.next_cursor.ok_or_else(|| {
        std::io::Error::other("event history omitted continuation before the second transfer")
    })?;
    event_query.start = None;
    event_query.cursor = Some(cursor);
    let mut changed = event_query.clone();
    changed.filters[0].addresses = vec![recipient];
    require_invalid_cursor(native.get_events(changed).await)?;
    let mut events = first.events;
    // Empty nonterminal pages must be followed. This range has only two
    // transactions, so this guard catches a cursor that stops making progress.
    for _ in 0..16 {
        let page = known(native.get_events(event_query.clone()).await?)?;
        events.extend(page.events);
        if page.next_cursor.is_none() {
            event_query.cursor = None;
            break;
        }
        require(
            page.next_cursor != event_query.cursor,
            "event cursor did not advance",
        )?;
        event_query.cursor = page.next_cursor;
    }
    require(
        event_query.cursor.is_none(),
        "event history failed to terminate",
    )?;
    check_history_event_payloads(&events, receipts)?;
    // Descending history also contains older bootstrap events. Compare the
    // newest prefix with the reversed ascending range before reaching those.
    let mut descending = input::EventQuery {
        filters: event_query.filters,
        start: Some(input::HistoryStart::Descending {
            anchor: input::DescendingAnchor::AtHeight {
                block_height: receipts[1].block_height.clone(),
            },
        }),
        cursor: None,
        limit: Some(1),
    };
    for expected in events.iter().rev() {
        let page = known(native.get_events(descending.clone()).await?)?;
        require(
            page.events.len() == 1
                && serde_json::to_value(&page.events[0])? == serde_json::to_value(expected)?,
            "event continuation lost descending order or event identity",
        )?;
        require(
            page.next_cursor != descending.cursor,
            "descending event cursor did not advance",
        )?;
        descending.start = None;
        descending.cursor = page.next_cursor;
    }
    Ok(())
}

fn check_history_event_payloads(
    events: &[output::IdentifiedEvent],
    receipts: [&output::TransactionReceipt; 2],
) -> TestResult {
    let expected_count = receipts
        .iter()
        .map(|receipt| {
            receipt
                .events
                .iter()
                .filter(|event| event.address == Base58(NATIVE_HOG_ADDRESS))
                .count()
        })
        .sum::<usize>();
    require(
        events.len() == expected_count,
        &format!(
            "event history returned {} token events; expected {expected_count}",
            events.len()
        ),
    )?;
    for receipt in receipts {
        let actual = events
            .iter()
            .filter(|event| event.transaction_hash == receipt.transaction_hash)
            .collect::<Vec<_>>();
        let expected = receipt
            .events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.address == Base58(NATIVE_HOG_ADDRESS))
            .collect::<Vec<_>>();
        require(
            actual.len() == expected.len(),
            "event transaction identity mismatch",
        )?;
        for (event, (sequence, emitted)) in actual.into_iter().zip(expected) {
            require(
                event.block_height == receipt.block_height
                    && event.sequence_number as usize == sequence
                    && serde_json::to_value(&event.event)? == serde_json::to_value(emitted)?,
                "event identity or payload differs from the committed receipt",
            )?;
        }
    }
    Ok(())
}

async fn check_block_history(
    native: &NativeClient<HttpClient>,
    receipts: [&output::TransactionReceipt; 2],
) -> TestResult {
    for receipt in receipts {
        let block = known(
            native
                .get_transactions_by_block(input::BlockPageRequest {
                    block_height: receipt.block_height.clone(),
                    start: Some(input::BlockStart {
                        order: IterationOrder::Ascending,
                    }),
                    cursor: None,
                    limit: Some(10),
                })
                .await?,
        )?;
        require(
            block.transactions.len() == 1
                && block.transactions[0].transaction_hash == receipt.transaction_hash
                && block.transactions[0].block_height == receipt.block_height
                && block.next_cursor.is_none(),
            "exact-block history did not contain exactly its committed transaction",
        )?;
    }
    Ok(())
}

async fn check_block_continuations(
    node: &NodeSupervisorHandle,
    native: &NativeClient<HttpClient>,
    sender: &LocalSigner,
    recipient: ContractAddress,
    signing_network: thruput_xdr::types::NetworkHash,
) -> TestResult {
    // Manual production keeps both admitted transactions in the same block;
    // there is no timer or race with the instant block driver.
    let mut hashes = Vec::new();
    for nonce in [3, 4] {
        hashes.push(
            submit_transfer(native, sender, recipient, signing_network.clone(), nonce).await?,
        );
    }
    let block = node
        .manual_block_producer()
        .ok_or_else(|| std::io::Error::other("manual block producer missing"))?
        .produce_block()
        .await?;
    for hash in &hashes {
        let receipt = wait_for_receipt(native, signing_network.clone(), *hash).await?;
        require(
            *receipt.block_height == block.height,
            "batch transactions committed in different blocks",
        )?;
    }
    hashes.sort_by_key(|hash| hash.0.0);
    for order in [IterationOrder::Ascending, IterationOrder::Descending] {
        let mut expected = hashes.clone();
        if order == IterationOrder::Descending {
            expected.reverse();
        }
        let mut query = input::BlockPageRequest {
            block_height: block.height.into(),
            start: Some(input::BlockStart { order }),
            cursor: None,
            limit: Some(1),
        };
        for (index, hash) in expected.iter().enumerate() {
            let page = known(native.get_transactions_by_block(query.clone()).await?)?;
            require(
                page.transactions.len() == 1
                    && page.transactions[0].transaction_hash == *hash
                    && page.transactions[0].block_height == query.block_height,
                "exact-block continuation lost order or transaction identity",
            )?;
            require(
                page.next_cursor.is_some() == (index + 1 < expected.len()),
                "exact-block continuation ended early or repeated its terminal row",
            )?;
            if let Some(cursor) = &page.next_cursor {
                require(
                    Some(cursor) != query.cursor.as_ref(),
                    "block cursor did not advance",
                )?;
                let mut changed = query.clone();
                changed.start = None;
                changed.cursor = Some(cursor.clone());
                changed.block_height = (block.height + 1).into();
                require_invalid_cursor(native.get_transactions_by_block(changed).await)?;
            }
            query.start = None;
            query.cursor = page.next_cursor;
        }
    }
    Ok(())
}
