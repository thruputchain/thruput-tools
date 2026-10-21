//! Bounded event scanning and opaque-cursor coverage through both real adapters.

use super::*;
use std::{error::Error, path::PathBuf};
use thruput_json_rpc_types::{ContractAddress, hex::Topic};
use thruput_signing::NATIVE_HOG_ADDRESS;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
const CANDIDATE_BUDGET: usize = 1_000;
const MAX_PAGES: usize = 64;

fn require(condition: bool, message: &str) -> Result {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    config: NodeSupervisorConfig,
    node: Option<NodeSupervisorHandle>,
    gateway: Option<tokio::process::Child>,
    starts: u32,
}

impl Fixture {
    fn new() -> Result<Self> {
        let binary = PathBuf::from(
            std::env::var_os("THRUPUT_DEVNODE_BINARY")
                .ok_or_else(|| std::io::Error::other("THRUPUT_DEVNODE_BINARY is required"))?,
        );
        require(
            binary.is_absolute() && binary.is_file(),
            "runtime path must be an absolute file",
        )?;
        let directory = tempfile::tempdir()?;
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_block_driver(BlockDriverConfig::manual())
            .with_hogd_dev(
                HogdDevConfig::default()
                    .with_binary_path(binary)
                    .with_startup_timeout(Duration::from_mins(1))
                    .with_data_dir(directory.path().join("data"))
                    .with_ramdb_bytes(1024 * 1024 * 1024),
            );
        Ok(Self {
            directory,
            config,
            node: None,
            gateway: None,
            starts: 0,
        })
    }

    async fn start(&mut self) -> Result<(HttpClient, HttpClient)> {
        self.starts += 1;
        eprintln!("starting history fixture phase {}", self.starts);
        self.node = Some(NodeSupervisorHandle::start(self.config.clone()).await?);
        let node = self.node.as_ref().unwrap();
        let binary = PathBuf::from(
            std::env::var_os("THRUPUT_GATEWAY_BINARY")
                .ok_or_else(|| std::io::Error::other("THRUPUT_GATEWAY_BINARY is required"))?,
        );
        require(
            binary.is_absolute() && binary.is_file(),
            "gateway path must be an absolute file",
        )?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        drop(listener);
        self.gateway = Some(
            tokio::process::Command::new(binary)
                .arg("--runtime-manifest")
                .arg(node.hogd_readiness().manifest_path.as_ref().unwrap())
                .arg("--listen")
                .arg(address.to_string())
                .arg("--scheduler-reconnect-grace-ms")
                .arg("0")
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(
                    self.directory
                        .path()
                        .join(format!("gateway-{}.log", self.starts)),
                )?)
                .kill_on_drop(true)
                .spawn()?,
        );
        let local = client(node.rpc_addr())?;
        let remote = client(address)?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if remote
                    .request::<output::ChainTip, _>(
                        "getChainTip",
                        NamedParams(input::EmptyRequest {}),
                    )
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?;
        Ok((local, remote))
    }

    async fn stop(&mut self) -> Result {
        let gateway_result = if let Some(mut child) = self.gateway.take() {
            let _ = child.start_kill();
            child.wait().await.map(|_| ())
        } else {
            Ok(())
        };
        let node_result = if let Some(node) = self.node.take() {
            node.stop().await.map(Some)
        } else {
            Ok(None)
        };
        gateway_result?;
        if let Some(status) = node_result? {
            require(
                status.success(),
                &format!("runtime did not shut down cleanly: {status}"),
            )?;
        }
        Ok(())
    }
}

fn client(address: std::net::SocketAddr) -> Result<HttpClient> {
    Ok(HttpClientBuilder::default()
        .request_timeout(Duration::from_secs(10))
        .build(format!("http://{address}"))?)
}

fn copy_directory(from: &Path, to: &Path) -> Result {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            require(
                entry.file_type()?.is_file(),
                "snapshot contains a non-file entry",
            )?;
            std::fs::copy(entry.path(), target)?;
        }
    }
    std::fs::set_permissions(to, std::fs::metadata(from)?.permissions())?;
    Ok(())
}

async fn fund_batch(
    fixture: &Fixture,
    rpc: &HttpClient,
    recipient: ContractAddress,
    nonce: &mut u32,
    count: usize,
) -> Result<Vec<output::TransactionReceipt>> {
    let node = fixture.node.as_ref().unwrap();
    let faucet = node
        .hogd_readiness()
        .runtime_manifest
        .as_ref()
        .unwrap()
        .bootstrap_accounts[0]
        .wallet_address;
    let mut hashes = Vec::new();
    for _ in 0..count {
        *nonce += 1;
        let result = rpc
            .request::<output::FundingSubmission, _>(
                "dev_fundAccount",
                NamedParams(FundAccountRequest {
                    recipient,
                    faucet,
                    raw_amount: 100_000,
                    client_nonce: ClientNonce::new(nonce.to_be_bytes().to_vec())?,
                }),
            )
            .await?;
        hashes.push(result.transaction_hash);
    }
    node.manual_block_producer()
        .unwrap()
        .produce_block()
        .await?;
    let mut receipts = Vec::new();
    for transaction_hash in hashes {
        let receipt = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(receipt) = rpc
                    .request::<output::ReceiptResult, _>(
                        "getTransactionReceipt",
                        NamedParams(input::TransactionQuery { transaction_hash }),
                    )
                    .await?
                {
                    break Ok::<_, jsonrpsee::core::ClientError>(receipt);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        require(
            matches!(receipt.execution, output::ReceiptExecution::Return { .. }),
            "fixture funding did not execute successfully",
        )?;
        require(
            receipt.transaction_hash == transaction_hash,
            "fixture receipt hash mismatch",
        )?;
        receipts.push(receipt);
    }
    Ok(receipts)
}

fn identified(receipts: &[output::TransactionReceipt]) -> Vec<output::IdentifiedEvent> {
    let mut events = Vec::new();
    for receipt in receipts {
        for (sequence, event) in receipt.events.iter().enumerate() {
            if event.address == Base58(NATIVE_HOG_ADDRESS) {
                events.push(output::IdentifiedEvent {
                    transaction_hash: receipt.transaction_hash,
                    block_height: receipt.block_height.clone(),
                    sequence_number: u32::try_from(sequence).unwrap(),
                    event: event.clone(),
                });
            }
        }
    }
    events.sort_by_key(|event| {
        (
            *event.block_height,
            event.transaction_hash.0.0,
            event.sequence_number,
        )
    });
    events
}

fn start(order: IterationOrder, height: BlockHeight) -> input::HistoryStart {
    match order {
        IterationOrder::Ascending => input::HistoryStart::Ascending {
            anchor: input::AscendingAnchor::AtHeight {
                block_height: height,
            },
        },
        IterationOrder::Descending => input::HistoryStart::Descending {
            anchor: input::DescendingAnchor::AtHeight {
                block_height: height,
            },
        },
    }
}

fn event_query(
    order: IterationOrder,
    height: BlockHeight,
    topic: Option<Topic>,
) -> input::EventQuery {
    input::EventQuery {
        filters: vec![input::EventFilter {
            addresses: vec![Base58(NATIVE_HOG_ADDRESS)],
            topics: topic.map(|topic| input::TopicFilter {
                topic2: Some(vec![topic]),
                ..Default::default()
            }),
        }],
        start: Some(start(order, height)),
        cursor: None,
        limit: Some(200),
    }
}

async fn walk_events(
    rpc: &HttpClient,
    mut query: input::EventQuery,
    expected: &[output::IdentifiedEvent],
    require_empty_continuation: bool,
) -> Result {
    let mut observed = Vec::new();
    let mut cursors = Vec::new();
    let mut empty_continuations = 0;
    for _ in 0..MAX_PAGES {
        let page = rpc
            .request::<output::EventPage, _>("getEvents", NamedParams(query.clone()))
            .await?;
        require(
            page.events.len() <= usize::try_from(query.limit.unwrap())?,
            "event page exceeded requested limit",
        )?;
        require(
            page.oldest_indexed_block_height <= page.latest_indexed_block_height,
            "event coverage bounds regressed",
        )?;
        if page.events.is_empty() && page.next_cursor.is_some() {
            empty_continuations += 1;
        }
        observed.extend(page.events);
        let Some(cursor) = page.next_cursor else {
            require(
                serde_json::to_value(&observed)? == serde_json::to_value(expected)?,
                "event walk differs from receipt event identities/payloads",
            )?;
            require(
                !require_empty_continuation || empty_continuations > 0,
                "fixture did not force an empty continuation page",
            )?;
            return Ok(());
        };
        require(!cursors.contains(&cursor), "event cursor repeated")?;
        cursors.push(cursor.clone());
        query.start = None;
        query.cursor = Some(cursor);
    }
    Err(std::io::Error::other("event pagination did not terminate within fixture bound").into())
}

async fn walk_root(
    rpc: &HttpClient,
    address: ContractAddress,
    order: IterationOrder,
    receipts: &[output::TransactionReceipt],
) -> Result {
    let mut expected = receipts
        .iter()
        .map(|receipt| output::TransactionRef {
            transaction_hash: receipt.transaction_hash,
            block_height: receipt.block_height.clone(),
        })
        .collect::<Vec<_>>();
    expected.sort_by_key(|item| (*item.block_height, item.transaction_hash.0.0));
    let height = match order {
        IterationOrder::Ascending => expected[0].block_height.clone(),
        IterationOrder::Descending => {
            expected.reverse();
            expected[0].block_height.clone()
        }
    };
    let mut query = input::RootAddressQuery {
        address,
        start: Some(start(order, height)),
        cursor: None,
        limit: Some(50),
    };
    let mut observed = Vec::new();
    let mut cursors = Vec::new();
    for _ in 0..MAX_PAGES {
        let page = rpc
            .request::<output::TransactionRefPage, _>(
                "getTransactionsByRootAddress",
                NamedParams(query.clone()),
            )
            .await?;
        require(
            page.transactions.len() <= usize::try_from(query.limit.unwrap())?,
            "root page exceeded requested limit",
        )?;
        observed.extend(page.transactions);
        let Some(cursor) = page.next_cursor else {
            return require(
                serde_json::to_value(observed)? == serde_json::to_value(expected)?,
                "root walk differs from committed receipts",
            );
        };
        require(!cursors.contains(&cursor), "root cursor repeated")?;
        cursors.push(cursor.clone());
        query.start = None;
        query.cursor = Some(cursor);
    }
    Err(std::io::Error::other("root pagination did not terminate within fixture bound").into())
}

#[derive(Clone)]
struct SavedCursors {
    events: input::EventQuery,
    root: input::RootAddressQuery,
    block: input::BlockPageRequest,
}

async fn save_cursors(
    rpc: &HttpClient,
    faucet: ContractAddress,
    receipts: &[output::TransactionReceipt],
) -> Result<SavedCursors> {
    let mut events = event_query(
        IterationOrder::Ascending,
        receipts[0].block_height.clone(),
        None,
    );
    events.limit = Some(1);
    events.cursor = rpc
        .request::<output::EventPage, _>("getEvents", NamedParams(events.clone()))
        .await?
        .next_cursor;
    events.start = None;
    let mut root = input::RootAddressQuery {
        address: faucet,
        start: Some(start(
            IterationOrder::Ascending,
            receipts[0].block_height.clone(),
        )),
        cursor: None,
        limit: Some(1),
    };
    root.cursor = rpc
        .request::<output::TransactionRefPage, _>(
            "getTransactionsByRootAddress",
            NamedParams(root.clone()),
        )
        .await?
        .next_cursor;
    root.start = None;
    let mut block = input::BlockPageRequest {
        block_height: receipts[1].block_height.clone(),
        start: Some(input::BlockStart {
            order: IterationOrder::Ascending,
        }),
        cursor: None,
        limit: Some(1),
    };
    block.cursor = rpc
        .request::<output::BlockTransactionPage, _>(
            "getTransactionsByBlock",
            NamedParams(block.clone()),
        )
        .await?
        .next_cursor;
    block.start = None;
    require(
        events.cursor.is_some() && root.cursor.is_some() && block.cursor.is_some(),
        "fixture failed to produce all three opaque cursors",
    )?;
    Ok(SavedCursors {
        events,
        root,
        block,
    })
}

async fn continuation_pages(rpc: &HttpClient, saved: &SavedCursors) -> Result<serde_json::Value> {
    let mut events = rpc
        .request::<output::EventPage, _>("getEvents", NamedParams(saved.events.clone()))
        .await?;
    let mut root = rpc
        .request::<output::TransactionRefPage, _>(
            "getTransactionsByRootAddress",
            NamedParams(saved.root.clone()),
        )
        .await?;
    let mut block = rpc
        .request::<output::BlockTransactionPage, _>(
            "getTransactionsByBlock",
            NamedParams(saved.block.clone()),
        )
        .await?;
    require(
        !events.events.is_empty()
            && !root.transactions.is_empty()
            && !block.transactions.is_empty(),
        "positive cursor control returned an empty continuation",
    )?;
    let continuation = [
        events.next_cursor.take().is_some(),
        root.next_cursor.take().is_some(),
        block.next_cursor.take().is_some(),
    ];
    Ok(serde_json::json!({ "pages": [events, root, block], "continuation": continuation }))
}

async fn check_cursor_restart(
    fixture: &mut Fixture,
    clients: [&HttpClient; 2],
    saved: [&SavedCursors; 2],
) -> Result {
    let before = [
        continuation_pages(clients[0], saved[0]).await?,
        continuation_pages(clients[1], saved[1]).await?,
    ];
    fixture.stop().await?;
    let (local, remote) = fixture.start().await?;
    for ((rpc, saved), expected) in [&local, &remote].into_iter().zip(saved).zip(before) {
        require(
            continuation_pages(rpc, saved).await? == expected,
            "plain restart changed an opaque cursor continuation",
        )?;
    }
    Ok(())
}

async fn require_coverage_errors(rpc: &HttpClient, saved: SavedCursors) -> Result {
    use thruput_json_rpc_types::error::native::{DecodedError, ErrorData};
    // An unindexed exact height without a cursor is an empty page. The opaque
    // continuation below must instead reject its now-unavailable saved coverage.
    let mut fresh = saved.block.clone();
    fresh.cursor = None;
    fresh.start = Some(input::BlockStart {
        order: IterationOrder::Ascending,
    });
    let empty = rpc
        .request::<output::BlockTransactionPage, _>("getTransactionsByBlock", NamedParams(fresh))
        .await?;
    require(
        empty.transactions.is_empty() && empty.next_cursor.is_none(),
        "unindexed exact block without a cursor was not empty",
    )?;
    let errors = [
        rpc.request::<output::EventPage, _>("getEvents", NamedParams(saved.events))
            .await
            .err()
            .ok_or_else(|| {
                std::io::Error::other("lost coverage unexpectedly returned a successful page")
            })?,
        rpc.request::<output::TransactionRefPage, _>(
            "getTransactionsByRootAddress",
            NamedParams(saved.root),
        )
        .await
        .err()
        .ok_or_else(|| {
            std::io::Error::other("lost coverage unexpectedly returned a successful page")
        })?,
        rpc.request::<output::BlockTransactionPage, _>(
            "getTransactionsByBlock",
            NamedParams(saved.block),
        )
        .await
        .err()
        .ok_or_else(|| {
            std::io::Error::other("lost coverage unexpectedly returned a successful page")
        })?,
    ];
    for (method, error) in [
        "getEvents",
        "getTransactionsByRootAddress",
        "getTransactionsByBlock",
    ]
    .into_iter()
    .zip(errors)
    {
        let jsonrpsee::core::ClientError::Call(error) = error else {
            return Err(error.into());
        };
        let data: serde_json::Value = serde_json::from_str(
            error
                .data()
                .ok_or_else(|| std::io::Error::other("history error missing typed data"))?
                .get(),
        )?;
        require(
            error.code() == -32013
                && matches!(DecodedError::decode(method, serde_json::json!({"code": error.code(), "message": error.message(), "data": data})), DecodedError::Known { data, .. } if matches!(*data, ErrorData::HistoryUnavailable { .. })),
            "lost coverage did not return typed historyUnavailable",
        )?;
    }
    Ok(())
}

async fn verify_history(
    local: &HttpClient,
    remote: &HttpClient,
    faucet: ContractAddress,
    special: ContractAddress,
    ordinary: ContractAddress,
    receipts: &[output::TransactionReceipt],
) -> Result<usize> {
    let events = identified(receipts);
    let special_topic = Topic(special.0.as_ref().try_into()?);
    let absent_topic = Topic([0x98; 32]);
    for order in [IterationOrder::Ascending, IterationOrder::Descending] {
        let mut expected = events.clone();
        if order == IterationOrder::Descending {
            expected.reverse();
        }
        let height = expected[0].block_height.clone();
        let sparse = expected
            .iter()
            .filter(|event| event.event.topics.get(2) == Some(&special_topic))
            .cloned()
            .collect::<Vec<_>>();
        require(
            sparse.len() >= 2 && sparse.len() < expected.len(),
            "fixture did not produce sparse matching topics",
        )?;
        require(
            expected
                .iter()
                .all(|event| event.event.topics.get(2) != Some(&absent_topic)),
            "absent topic unexpectedly occurred",
        )?;
        for (label, rpc) in [("local", local), ("gateway", remote)] {
            eprintln!("checking {label} history in {order:?} order");
            let mut all_query = event_query(order, height.clone(), None);
            all_query.filters[0].topics = Some(input::TopicFilter {
                topic2: Some(vec![special_topic, Topic(ordinary.0.as_ref().try_into()?)]),
                ..Default::default()
            });
            walk_events(rpc, all_query, &expected, false).await?;
            let mut sparse_query = event_query(order, height.clone(), Some(special_topic));
            sparse_query.limit = Some(1);
            walk_events(rpc, sparse_query, &sparse, true).await?;
            walk_events(
                rpc,
                event_query(order, height.clone(), Some(absent_topic)),
                &[],
                true,
            )
            .await?;
            walk_root(rpc, faucet, order, receipts).await?;
        }
    }
    Ok(events.len())
}

async fn check_restored(
    fixture: &Fixture,
    local: &HttpClient,
    remote: &HttpClient,
    network: &output::NetworkInfo,
    receipts: &[output::TransactionReceipt],
    faucet: ContractAddress,
) -> Result {
    let restored = local
        .request::<output::NetworkInfo, _>("getNetworkInfo", NamedParams(input::EmptyRequest {}))
        .await?;
    require(
        restored.network_hash == network.network_hash,
        "snapshot restoration changed network identity",
    )?;
    require(
        fixture
            .node
            .as_ref()
            .unwrap()
            .hogd_readiness()
            .runtime_manifest
            .as_ref()
            .unwrap()
            .bootstrap_accounts[0]
            .wallet_address
            == faucet,
        "snapshot restoration changed faucet identity",
    )?;
    for rpc in [local, remote] {
        let page = rpc
            .request::<output::TransactionRefPage, _>(
                "getTransactionsByRootAddress",
                NamedParams(input::RootAddressQuery {
                    address: faucet,
                    start: Some(start(
                        IterationOrder::Ascending,
                        receipts[0].block_height.clone(),
                    )),
                    cursor: None,
                    limit: Some(1),
                }),
            )
            .await?;
        require(
            page.latest_indexed_block_height == receipts[0].block_height
                && page.latest_indexed_block_height < receipts.last().unwrap().block_height,
            "snapshot did not restore lower indexed coverage",
        )?;
    }
    Ok(())
}

async fn exercise(fixture: &mut Fixture) -> Result {
    let (local, _) = fixture.start().await?;
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
    let special = Base58(
        PubkeyAccount::from_public_key_bytes(
            LocalSigner::from_secret_key([0x73; 32]).verifying_key_bytes(),
            0,
        )
        .address(),
    );
    let ordinary = Base58(
        PubkeyAccount::from_public_key_bytes(
            LocalSigner::from_secret_key([0x74; 32]).verifying_key_bytes(),
            0,
        )
        .address(),
    );
    let network = local
        .request::<output::NetworkInfo, _>("getNetworkInfo", NamedParams(input::EmptyRequest {}))
        .await?;
    let mut nonce = 0;
    let mut receipts = fund_batch(fixture, &local, special, &mut nonce, 1).await?;
    // A complete stopped snapshot preserves chain identity and real index files.
    fixture.stop().await?;
    copy_directory(
        &fixture.directory.path().join("data"),
        &fixture.directory.path().join("snapshot"),
    )?;
    let (local, remote) = fixture.start().await?;
    eprintln!("seeding more than {CANDIDATE_BUDGET} real event candidates");
    let mut middle_events = 0;
    for _ in 0..40 {
        let count = (CANDIDATE_BUDGET + 1 - middle_events).min(32);
        let batch = fund_batch(fixture, &local, ordinary, &mut nonce, count).await?;
        middle_events += identified(&batch).len();
        receipts.extend(batch);
        if middle_events > CANDIDATE_BUDGET {
            break;
        }
    }
    require(
        middle_events > CANDIDATE_BUDGET,
        "fixture did not exceed the real event candidate budget",
    )?;
    receipts.extend(fund_batch(fixture, &local, special, &mut nonce, 1).await?);
    let event_count = verify_history(&local, &remote, faucet, special, ordinary, &receipts).await?;
    let local_cursors = save_cursors(&local, faucet, &receipts).await?;
    let remote_cursors = save_cursors(&remote, faucet, &receipts).await?;
    check_cursor_restart(
        fixture,
        [&local, &remote],
        [&local_cursors, &remote_cursors],
    )
    .await?;
    fixture.stop().await?;
    std::fs::remove_dir_all(fixture.directory.path().join("data"))?;
    copy_directory(
        &fixture.directory.path().join("snapshot"),
        &fixture.directory.path().join("data"),
    )?;
    let (local, remote) = fixture.start().await?;
    check_restored(fixture, &local, &remote, &network, &receipts, faucet).await?;
    require_coverage_errors(&local, local_cursors).await?;
    require_coverage_errors(&remote, remote_cursors).await?;
    eprintln!(
        "qualified {} receipts / {} HOG events through both endpoints and restored coverage",
        receipts.len(),
        event_count
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires published THRUPUT_DEVNODE_BINARY and THRUPUT_GATEWAY_BINARY; seeds >1000 real events"]
async fn real_history_scanning_and_coverage_match_both_servers() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_ansi(false)
        .with_test_writer()
        .try_init();
    let mut fixture = Fixture::new().unwrap();
    let result = tokio::time::timeout(Duration::from_mins(5), exercise(&mut fixture)).await;
    let cleanup = fixture.stop().await;
    if !matches!(&result, Ok(Ok(()))) {
        eprintln!("history qualification failed: {result:?}");
        eprintln!(
            "gateway diagnostics: {}",
            std::fs::read_to_string(
                fixture
                    .directory
                    .path()
                    .join(format!("gateway-{}.log", fixture.starts))
            )
            .unwrap_or_default()
        );
    }
    cleanup.unwrap();
    result.unwrap().unwrap();
}
