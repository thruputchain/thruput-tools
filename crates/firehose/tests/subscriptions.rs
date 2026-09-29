use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use groundhog_firehose::{
    store::Store,
    subscription::{Config, Cursor, Hub},
};
use groundhog_indexer::{
    EventIndexerPullRequest, EventIndexerPullResult, EventIndexerPulledBlock, IndexerProgress,
};
use jsonrpsee::{
    core::client::{ClientT, Subscription, SubscriptionClientT},
    rpc_params,
    ws_client::{WsClient, WsClientBuilder},
};
use rpc_driver::{ProgVers, RpcError, RpcServer, ServiceMap, handler::RpcHandler};
use serde_json::Value;
use tokio::sync::Notify;

mod common;
use common::{fixture, next, rehash};

fn config() -> Config {
    Config {
        ws_listen: "127.0.0.1:0".parse().unwrap(),
        allow_remote: false,
        max_subscribers: 64,
        max_subscriptions_per_connection: None,
        subscriber_queue: 2,
        subscriber_queue_bytes: 32 * 1024,
        max_record_bytes: 16 * 1024 * 1024,
        live_timeout_ms: 1000,
        catchup_timeout_ms: 1000,
        catchup_progress_ms: 30000,
    }
}

fn cli_config(args: &[&str]) -> Config {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        subscriptions: Config,
    }
    Cli::parse_from(
        ["test", "--ws-listen", "127.0.0.1:0"]
            .into_iter()
            .chain(args.iter().copied()),
    )
    .subscriptions
}

#[test]
fn subscription_connection_defaults_overrides_and_zero_validation() {
    for remote in [false, true] {
        let mut args = vec!["--max-subscribers", "7"];
        if remote {
            args.push("--allow-remote");
        }
        assert_eq!(
            cli_config(&args).subscription_limit_per_connection(),
            if remote { 1 } else { 7 }
        );
        args.extend(["--max-subscriptions-per-connection", "3"]);
        let config = cli_config(&args);
        assert_eq!(config.subscription_limit_per_connection(), 3);
        assert!(config.validate(config.ws_listen).is_ok());
        *args.last_mut().unwrap() = "0";
        let config = cli_config(&args);
        assert!(config.validate(config.ws_listen).is_err());
    }
}

fn assert_connection_limit(error: jsonrpsee::core::client::Error, limit: u32) {
    let jsonrpsee::core::client::Error::Call(error) = error else {
        panic!("expected typed JSON-RPC rejection, got {error:?}");
    };
    assert_eq!(error.code(), -32006);
    assert_eq!(error.message(), "Too many subscriptions on the connection");
    assert_eq!(
        serde_json::from_str::<String>(error.data().unwrap().get()).unwrap(),
        format!("Exceeded max limit of {limit}")
    );
}

#[tokio::test]
async fn public_profile_refuses_second_subscription_and_first_keeps_delivering() {
    use groundhog_firehose::api::FirehoseApiClient;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let hub = Hub::new(
        dir.path().to_owned(),
        store.height(),
        cli_config(&["--allow-remote"]),
    );
    store.subscribe_with(hub.clone());
    let (address, server) = hub.serve().await.unwrap();
    let client = connect(address).await;
    let mut first = subscribe(&client, Cursor::Tip).await;
    let current = fixture();
    store.append(&current).unwrap();
    block(&client, &mut first, 1).await;
    assert_connection_limit(client.follow_blocks(Cursor::Tip).await.unwrap_err(), 1);
    assert_eq!(hub.stats()["currentSubscribers"], 1);
    // Another connection still has its own slot.
    let other = connect(address).await;
    let mut second = subscribe(&other, Cursor::Tip).await;
    store.append(&next(&current)).unwrap();
    block(&client, &mut first, 2).await;
    block(&other, &mut second, 2).await;
    server.stop().unwrap();
    drop((first, second, client, other));
    server.stopped().await;
}

#[tokio::test]
async fn public_profile_concurrent_subscriptions_admit_exactly_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let hub = Hub::new(
        dir.path().to_owned(),
        store.height(),
        cli_config(&["--allow-remote"]),
    );
    store.subscribe_with(hub.clone());
    let (address, server) = hub.serve().await.unwrap();
    let client = connect(address).await;
    let attempt = || {
        client.subscribe::<Value, _>(
            "firehose_subscribe",
            rpc_params![Cursor::Tip],
            "firehose_unsubscribe",
        )
    };
    let (first, second) = tokio::join!(attempt(), attempt());
    let (mut accepted, rejected) = match (first, second) {
        (Ok(sub), Err(error)) | (Err(error), Ok(sub)) => (sub, error),
        result => panic!("expected exactly one accepted subscription: {result:?}"),
    };
    assert_connection_limit(rejected, 1);
    assert_eq!(hub.stats()["currentSubscribers"], 1);
    let current = fixture();
    store.append(&current).unwrap();
    block(&client, &mut accepted, 1).await;
    store.append(&next(&current)).unwrap();
    block(&client, &mut accepted, 2).await;
    server.stop().unwrap();
    drop((accepted, client));
    server.stopped().await;
}

#[tokio::test]
async fn loopback_default_and_explicit_remote_override_allow_multiple_cursors() {
    for args in [
        vec![],
        vec!["--allow-remote", "--max-subscriptions-per-connection", "3"],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let current = fixture();
        store.append(&current).unwrap();
        let hub = Hub::new(dir.path().to_owned(), store.height(), cli_config(&args));
        store.subscribe_with(hub.clone());
        let (address, server) = hub.serve().await.unwrap();
        let client = connect(address).await;
        let mut tip = subscribe(&client, Cursor::Tip).await;
        let mut backfill = subscribe(&client, Cursor::Height { height: 1 }).await;
        let mut another = subscribe(&client, Cursor::Tip).await;
        assert_eq!(hub.stats()["currentSubscribers"], 3);
        block(&client, &mut backfill, 1).await;
        store.append(&next(&current)).unwrap();
        block(&client, &mut tip, 2).await;
        block(&client, &mut backfill, 2).await;
        block(&client, &mut another, 2).await;
        server.stop().unwrap();
        drop((tip, backfill, another, client));
        server.stopped().await;
    }
}

async fn connect(address: std::net::SocketAddr) -> WsClient {
    WsClientBuilder::default()
        .max_response_size(40 * 1024 * 1024)
        .build(format!("ws://{address}"))
        .await
        .unwrap()
}

async fn subscribe(client: &WsClient, cursor: Cursor) -> Subscription<Value> {
    client
        .subscribe(
            "firehose_subscribe",
            rpc_params![cursor],
            "firehose_unsubscribe",
        )
        .await
        .unwrap()
}

async fn receive(sub: &mut Subscription<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn ack(client: &WsClient, message: &Value) {
    client
        .request::<(), _>(
            "firehose_ack",
            rpc_params![
                message["subscriberId"].as_u64().unwrap(),
                message["block"]["height"].as_i64().unwrap()
            ],
        )
        .await
        .unwrap();
}

async fn block(client: &WsClient, sub: &mut Subscription<Value>, height: i64) -> Value {
    let message = receive(sub).await;
    assert_eq!(message["type"], "block", "{message}");
    assert_eq!(message["block"]["height"], height);
    let encoded = message["block"]["record"]
        .as_str()
        .unwrap()
        .strip_prefix("0x")
        .unwrap();
    let bytes = hex::decode(encoded).unwrap();
    assert_eq!(
        bytes.len(),
        usize::try_from(message["block"]["byteLength"].as_u64().unwrap()).unwrap()
    );
    let decoded: EventIndexerPulledBlock = xdrs::txs::decode(&bytes).unwrap();
    assert_eq!(
        message["block"]["consensusHash"],
        format!(
            "0x{}",
            hex::encode(decoded.block.identity.consensus_hash.as_ref())
        )
    );
    assert_eq!(
        *decoded
            .block
            .payload
            .consensus_value
            .proposal_header
            .block_height,
        height
    );
    ack(client, &message).await;
    message
}

#[tokio::test]
async fn archive_boundary_resume_and_bytes_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    let received_dir = tempfile::tempdir().unwrap();
    let mut received_store = Store::open(received_dir.path()).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut current = fixture();
    for _ in 0..10 {
        store.append(&current).unwrap();
        current = next(&current);
    }
    let hub = Hub::new(dir.path().to_owned(), store.height(), config());
    store.subscribe_with(hub.clone());
    let (address, server) = hub.serve().await.unwrap();
    let client = connect(address).await;
    let mut sub = subscribe(&client, Cursor::Height { height: 1 }).await;
    for height in 1..10 {
        let message = block(&client, &mut sub, height).await;
        verify_received(&mut received_store, &message);
    }
    // Publish at the exact boundary while the final archive block awaits ACK.
    let last = receive(&mut sub).await;
    assert_eq!(last["block"]["height"], 10);
    verify_received(&mut received_store, &last);
    store.append(&current).unwrap();
    ack(&client, &last).await;
    let delivered = block(&client, &mut sub, 11).await;
    verify_received(&mut received_store, &delivered);
    let raw = hex::decode(
        delivered["block"]["record"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();
    assert_eq!(
        raw,
        std::fs::read(dir.path().join("00000000000000000011.xdr")).unwrap()
    );
    current = next(&current);
    store.append(&current).unwrap();
    let interrupted = receive(&mut sub).await;
    assert_eq!(interrupted["block"]["height"], 12);
    drop(sub);
    drop(client);
    let client = connect(address).await;
    let mut sub = subscribe(&client, Cursor::Height { height: 12 }).await;
    block(&client, &mut sub, 12).await; // Unacknowledged record is the documented repeat.
    current = next(&current);
    store.append(&current).unwrap();
    block(&client, &mut sub, 13).await;
    server.stop().unwrap();
    drop(sub);
    drop(client);
    server.stopped().await;
}

fn verify_received(store: &mut Store, message: &Value) {
    let bytes = hex::decode(
        message["block"]["record"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();
    let decoded = xdrs::txs::decode(&bytes).unwrap();
    // Uses the existing independently fixture-tested verifier for events_hash,
    // txs_hash, consensus hash and chain continuity, on subscriber-received bytes.
    assert!(store.append(&decoded).unwrap());
}

#[tokio::test]
async fn slow_subscriber_demotes_and_disconnects_without_harming_fast_subscriber() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let hub = Hub::new(dir.path().to_owned(), store.height(), config());
    store.subscribe_with(hub.clone());
    let (address, server) = hub.serve().await.unwrap();
    let fast = connect(address).await;
    let slow = connect(address).await;
    let mut fast_sub = subscribe(&fast, Cursor::Tip).await;
    let mut slow_sub = subscribe(&slow, Cursor::Tip).await;
    let mut current = fixture();
    store.append(&current).unwrap();
    block(&fast, &mut fast_sub, 1).await;
    let unacked = receive(&mut slow_sub).await;
    for height in 2..=8 {
        current = next(&current);
        store.append(&current).unwrap();
        block(&fast, &mut fast_sub, height).await;
    }
    let stats = hub.stats();
    assert_eq!(stats["totals"]["demotions"], 1);
    let slow_id = unacked["subscriberId"].as_u64().unwrap();
    let slow_stats = stats["subscribers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == slow_id)
        .unwrap();
    assert_eq!(slow_stats["mode"], "catchup");
    assert_eq!(slow_stats["queuedBlocks"], 0);
    // Let it make progress from disk, then stall again: eviction must be in catch-up.
    ack(&slow, &unacked).await;
    let disk_block = receive(&mut slow_sub).await;
    assert_eq!(disk_block["block"]["height"], 2);
    let closed = receive(&mut slow_sub).await;
    assert_eq!(
        closed,
        serde_json::json!({"type":"closed", "reason":"catchup_timeout", "nextHeight":2})
    );
    for height in 9..=12 {
        current = next(&current);
        store.append(&current).unwrap();
        block(&fast, &mut fast_sub, height).await;
    }
    assert_eq!(store.height(), 12);
    assert_eq!(hub.stats()["totals"]["disconnects"]["catchup_timeout"], 1);
    let mut resumed = subscribe(&slow, Cursor::Height { height: 2 }).await;
    for height in 2..=12 {
        block(&slow, &mut resumed, height).await;
    }
    server.stop().unwrap();
    drop(fast_sub);
    drop(slow_sub);
    drop(resumed);
    drop(fast);
    drop(slow);
    server.stopped().await;
}

#[derive(Default)]
struct Upstream {
    blocks: Mutex<Vec<EventIndexerPulledBlock>>,
    published: Notify,
    calls: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct UpstreamHandler(Arc<Upstream>);
impl std::ops::Deref for UpstreamHandler {
    type Target = Upstream;
    fn deref(&self) -> &Upstream {
        &self.0
    }
}

#[async_trait::async_trait]
impl RpcHandler for UpstreamHandler {
    fn prog_vers(&self) -> ProgVers {
        ProgVers::new(0x4321_aaac, 1)
    }
    async fn dispatch(&self, procedure: u32, data: &[u8]) -> Result<Vec<u8>, RpcError> {
        assert_eq!(procedure, 10, "never call the destructive failure mailbox");
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _guard = Active(&self.active);
        let request: EventIndexerPullRequest = xdrs::txs::decode(data).unwrap();
        let (height, parked) = match request.progress {
            IndexerProgress::IndexerProgressInitialized { initialized } => {
                (*initialized.contiguous_height, false)
            }
            IndexerProgress::IndexerProgressCaughtUp { initialized } => {
                (*initialized.contiguous_height, true)
            }
            _ => (0, false),
        };
        loop {
            let changed = self.published.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let reply = {
                let blocks = self.blocks.lock().unwrap();
                if let Some(block) = blocks.iter().find(|b| {
                    *b.block.payload.consensus_value.proposal_header.block_height > height
                }) {
                    Some(EventIndexerPullResult::EventIndexerPullBlock {
                        pulled_block: block.clone(),
                    })
                } else if parked {
                    None
                } else {
                    Some(EventIndexerPullResult::EventIndexerPullCaughtUp {
                        origin: fixture().origin,
                    })
                }
            };
            if let Some(reply) = reply {
                return Ok(xdrs::txs::encode(&reply).unwrap());
            }
            changed.await;
        }
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

fn traffic_fixture() -> EventIndexerPulledBlock {
    let text: String = include_str!("fixtures/hundred-transactions.hex")
        .split_whitespace()
        .collect();
    let mut block: EventIndexerPulledBlock =
        xdrs::txs::decode(&hex::decode(text).unwrap()).unwrap();
    block.origin = fixture().origin;
    block
        .block
        .payload
        .consensus_value
        .proposal_header
        .block_height = 1.into();
    block.block.payload.consensus_value.prev_hash = block.origin.first_prev_hash.clone();
    rehash(&mut block);
    block
}

async fn process_usage(pid: u32) -> (f64, u64) {
    let output = tokio::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "time=,rss="])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let values: Vec<_> = text.split_whitespace().collect();
    let time: Vec<f64> = values[0]
        .split(':')
        .map(|part| part.parse().unwrap())
        .collect();
    (time[0] * 60.0 + time[1], values[1].parse().unwrap())
}

/// Process-isolated server measurements; clients and synthetic x23 source are in this harness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement run: about 40 seconds, uses ps and loopback sockets"]
#[allow(clippy::too_many_lines)]
async fn measure_fanout_and_catchup() {
    for count in [1_usize, 10, 50] {
        let upstream = Arc::new(Upstream::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let mut services = ServiceMap::new();
        services.add_service(Box::new(UpstreamHandler(upstream.clone())));
        let task = RpcServer::new(listener, services).start().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let dir = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hog-firehose"))
            .args([
                "run",
                "--event-endpoint",
                &endpoint.to_string(),
                "--store",
                dir.path().to_str().unwrap(),
                "--listen",
                "127.0.0.1:0",
                "--ws-listen",
                &address.to_string(),
            ])
            .env("RUST_LOG", "error")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        wait_until(|| upstream.calls.load(Ordering::SeqCst) == 2).await;
        let pid = child.id().unwrap();
        let mut readers = Vec::new();
        for _ in 0..count {
            let client = connect(address).await;
            let mut sub = subscribe(&client, Cursor::Tip).await;
            readers.push(tokio::spawn(async move {
                let mut latencies = Vec::new();
                for height in 1..=40 {
                    let message = receive(&mut sub).await;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_micros();
                    assert_eq!(message["block"]["height"], height);
                    let ingested = message["block"]["ingestedAtUnixMicros"].as_u64().unwrap();
                    latencies.push(u64::try_from(now).unwrap() - ingested);
                    ack(&client, &message).await;
                }
                (latencies, client, sub)
            }));
        }
        let (cpu_start, rss_start) = process_usage(pid).await;
        let started = Instant::now();
        let mut max_rss = rss_start;
        let mut current = traffic_fixture();
        let raw_bytes = xdrs::txs::encode(&current).unwrap().len();
        for _ in 0..40 {
            upstream.blocks.lock().unwrap().push(current.clone());
            upstream.published.notify_waiters();
            tokio::time::sleep(Duration::from_millis(250)).await;
            max_rss = max_rss.max(process_usage(pid).await.1);
            current = next(&current);
        }
        let (cpu_end, rss_end) = process_usage(pid).await;
        let wall = started.elapsed().as_secs_f64();
        let mut latencies = Vec::new();
        let mut connections = Vec::new();
        for reader in readers {
            let (mut samples, client, sub) = reader.await.unwrap();
            latencies.append(&mut samples);
            connections.push((client, sub));
        }
        latencies.sort_unstable();
        assert!(
            latencies.last().is_some_and(|micros| *micros < 250_000),
            "every live delivery must arrive within the configured 250 ms block interval"
        );
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 82);
        assert_eq!(upstream.max_active.load(Ordering::SeqCst), 1);
        let client = connect(address).await;
        let before = Instant::now();
        let mut sub = subscribe(&client, Cursor::Height { height: 1 }).await;
        for height in 1..=40 {
            block(&client, &mut sub, height).await;
        }
        let catchup_seconds = before.elapsed().as_secs_f64();
        let stats: Value = client
            .request("firehose_stats", rpc_params![])
            .await
            .unwrap();
        assert_eq!(stats["totals"]["demotions"], 0);
        println!(
            "{}",
            serde_json::json!({"subscribers":count, "blocks":40, "rawBytes":raw_bytes,
            "wallSeconds":wall, "serverCpuSeconds":cpu_end-cpu_start,
            "serverCpuPercentOneCore":100.0*(cpu_end-cpu_start)/wall,
            "rssStartKiB":rss_start,"rssEndKiB":rss_end,"rssSampledMaxKiB":max_rss,
            "latencySamples":latencies.len(), "latencyP50Micros":latencies[latencies.len()/2],
            "latencyP95Micros":latencies[latencies.len()*95/100],"latencyMaxMicros":latencies.last(),
            "catchupBlocksPerSecond":40.0/catchup_seconds,"catchupSeconds":catchup_seconds,
            "upstreamCalls":82,"maxActivePulls":1})
        );
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_upstream_pull_stream_with_zero_one_and_fifty_real_websocket_subscribers() {
    let upstream = Arc::new(Upstream::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let mut services = ServiceMap::new();
    services.add_service(Box::new(UpstreamHandler(upstream.clone())));
    let upstream_task = RpcServer::new(listener, services).start().await;
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = ws_listener.local_addr().unwrap();
    drop(ws_listener);
    let dir = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_hog-firehose"))
        .args([
            "run",
            "--event-endpoint",
            &endpoint.to_string(),
            "--store",
            dir.path().to_str().unwrap(),
            "--listen",
            "127.0.0.1:0",
            "--ws-listen",
            &address.to_string(),
            "--subscriber-queue",
            "2",
            "--live-timeout-ms",
            "1000",
            "--catchup-timeout-ms",
            "1000",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_until(|| upstream.calls.load(Ordering::SeqCst) == 2).await;
    assert_eq!(upstream.active.load(Ordering::SeqCst), 1);
    let mut clients = Vec::new();
    let mut subs = Vec::new();
    let mut current = fixture();
    for (round, count) in [0, 1, 50].into_iter().enumerate() {
        while clients.len() < count {
            let client = connect(address).await;
            let sub = subscribe(&client, Cursor::Tip).await;
            clients.push(client);
            subs.push(sub);
        }
        let before = upstream.calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            upstream.calls.load(Ordering::SeqCst),
            before,
            "subscribing cannot pull"
        );
        upstream.blocks.lock().unwrap().push(current.clone());
        upstream.published.notify_waiters();
        let published = Instant::now();
        for (client, sub) in clients.iter().zip(&mut subs) {
            block(client, sub, i64::try_from(round).unwrap() + 1).await;
        }
        assert!(published.elapsed() < Duration::from_secs(1));
        wait_until(|| upstream.calls.load(Ordering::SeqCst) == before + 2).await;
        assert_eq!(upstream.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(upstream.active.load(Ordering::SeqCst), 1);
        assert!(dir.path().join(format!("{:020}.xdr", round + 1)).exists());
        current = next(&current);
    }
    assert_slow_client_isolated(address, &upstream, &mut current, &clients, &mut subs).await;
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    upstream_task.abort();
}

async fn assert_slow_client_isolated(
    address: std::net::SocketAddr,
    upstream: &Upstream,
    current: &mut EventIndexerPulledBlock,
    clients: &[WsClient],
    subs: &mut [Subscription<Value>],
) {
    let slow = connect(address).await;
    let mut slow_sub = subscribe(&slow, Cursor::Tip).await;
    for height in 4..=8 {
        upstream.blocks.lock().unwrap().push(current.clone());
        upstream.published.notify_waiters();
        for (client, sub) in clients.iter().zip(subs.iter_mut()) {
            block(client, sub, height).await;
        }
        *current = next(current);
    }
    let first = receive(&mut slow_sub).await;
    assert_eq!(first["block"]["height"], 4);
    let stats: Value = slow.request("firehose_stats", rpc_params![]).await.unwrap();
    assert_eq!(stats["totals"]["demotions"], 1);
    ack(&slow, &first).await;
    let disk = receive(&mut slow_sub).await;
    assert_eq!(disk["block"]["height"], 5);
    for height in 9..=12 {
        upstream.blocks.lock().unwrap().push(current.clone());
        upstream.published.notify_waiters();
        for (client, sub) in clients.iter().zip(subs.iter_mut()) {
            block(client, sub, height).await;
        }
        *current = next(current);
    }
    let closed = receive(&mut slow_sub).await;
    assert_eq!(closed["type"], "closed");
    assert_eq!(closed["reason"], "catchup_timeout");
    assert_eq!(closed["nextHeight"], 5);
    assert_eq!(upstream.max_active.load(Ordering::SeqCst), 1);
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 26);
    let stats: Value = slow.request("firehose_stats", rpc_params![]).await.unwrap();
    assert_eq!(stats["currentSubscribers"], 50);
    for subscriber in stats["subscribers"].as_array().unwrap() {
        assert_eq!(subscriber["nextHeight"], 13);
        assert_eq!(subscriber["lagBlocks"], 0);
    }
}

#[tokio::test]
async fn bounds_connection_scoped_acks_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut config = config();
    config.max_subscribers = 2;
    config.subscriber_queue_bytes = 1;
    let hub = Hub::new(dir.path().to_owned(), store.height(), config);
    store.subscribe_with(hub.clone());
    let (address, server) = hub.serve().await.unwrap();
    let client = connect(address).await;
    let other = connect(address).await;
    let mut sub = subscribe(&client, Cursor::Tip).await;
    let second = subscribe(&client, Cursor::Tip).await;
    assert!(
        client
            .subscribe::<Value, _>(
                "firehose_subscribe",
                rpc_params![Cursor::Tip],
                "firehose_unsubscribe"
            )
            .await
            .is_err()
    );
    store.append(&fixture()).unwrap();
    let message = receive(&mut sub).await;
    assert_eq!(
        hub.stats()["totals"]["demotions"],
        2,
        "byte limit demotes before count limit"
    );
    let id = message["subscriberId"].as_u64().unwrap();
    assert!(
        other
            .request::<(), _>("firehose_ack", rpc_params![id, 1])
            .await
            .is_err()
    );
    assert!(
        client
            .request::<(), _>("firehose_ack", rpc_params![id, 2])
            .await
            .is_err()
    );
    ack(&client, &message).await;
    sub.unsubscribe().await.unwrap();
    second.unsubscribe().await.unwrap();
    wait_until(|| hub.stats()["currentSubscribers"] == 0).await;
    assert!(!hub.render_metrics().contains("subscriber=\""));
    let _replacement = subscribe(&client, Cursor::Tip).await;
    server.stop().unwrap();
    drop(client);
    drop(other);
    server.stopped().await;
}

#[tokio::test]
async fn stalled_live_and_oversize_records_return_resumable_close_reasons() {
    for oversized in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut config = config();
        config.live_timeout_ms = 40;
        config.catchup_timeout_ms = 40;
        if oversized {
            config.max_record_bytes = 100;
        }
        let hub = Hub::new(dir.path().to_owned(), store.height(), config);
        store.subscribe_with(hub.clone());
        let (address, server) = hub.serve().await.unwrap();
        let client = connect(address).await;
        let mut sub = subscribe(&client, Cursor::Tip).await;
        store.append(&fixture()).unwrap();
        if !oversized {
            assert_eq!(receive(&mut sub).await["type"], "block");
        }
        let closed = receive(&mut sub).await;
        assert_eq!(
            closed["reason"],
            if oversized {
                "record_too_large"
            } else {
                "catchup_timeout"
            }
        );
        assert_eq!(closed["nextHeight"], 1);
        assert_eq!(hub.stats()["totals"]["demotions"], u64::from(!oversized));
        assert_eq!(store.height(), 1);
        server.stop().unwrap();
        drop(sub);
        drop(client);
        server.stopped().await;
    }
}

#[test]
fn listeners_and_nonzero_bounds_require_explicit_configuration() {
    let mut config = config();
    assert!(config.validate("0.0.0.0:9550".parse().unwrap()).is_err());
    config.ws_listen = "[::]:9551".parse().unwrap();
    assert!(config.validate("127.0.0.1:9550".parse().unwrap()).is_err());
    config.allow_remote = true;
    assert!(config.validate("127.0.0.1:9550".parse().unwrap()).is_ok());
    config.subscriber_queue = 0;
    assert!(config.validate("127.0.0.1:9550".parse().unwrap()).is_err());
}

#[tokio::test]
async fn growing_archive_lag_disconnects_even_when_individual_acks_meet_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut current = fixture();
    for _ in 0..5 {
        store.append(&current).unwrap();
        current = next(&current);
    }
    let mut config = config();
    config.catchup_progress_ms = 50;
    let hub = Hub::new(dir.path().to_owned(), store.height(), config);
    store.subscribe_with(hub.clone());
    let (address, server) = hub.serve().await.unwrap();
    let client = connect(address).await;
    let mut sub = subscribe(&client, Cursor::Height { height: 1 }).await;
    let message = receive(&mut sub).await;
    assert_eq!(message["block"]["height"], 1);
    for _ in 0..5 {
        store.append(&current).unwrap();
        current = next(&current);
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    ack(&client, &message).await;
    let closed = receive(&mut sub).await;
    assert_eq!(closed["reason"], "catchup_falling_behind");
    assert_eq!(closed["nextHeight"], 2);
    assert!(hub.stats()["totals"]["catchupSeconds"].as_f64().unwrap() >= 0.05);
    server.stop().unwrap();
    drop(sub);
    drop(client);
    server.stopped().await;
}

#[tokio::test]
async fn typed_client_future_cursor_and_invalid_cursor() {
    use groundhog_firehose::api::FirehoseApiClient;
    use groundhog_firehose::types::Notification;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let hub = Hub::new(dir.path().to_owned(), store.height(), config());
    store.subscribe_with(hub.clone());
    let (address, server) = hub.serve().await.unwrap();
    let client = connect(address).await;
    assert!(
        client
            .follow_blocks(Cursor::Height { height: -1 })
            .await
            .is_err()
    );
    let mut sub = client
        .follow_blocks(Cursor::Height { height: 3 })
        .await
        .unwrap();
    let mut current = fixture();
    for _ in 0..3 {
        store.append(&current).unwrap();
        current = next(&current);
    }
    let message = tokio::time::timeout(Duration::from_secs(1), sub.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Notification::Block {
        subscriber_id,
        block,
    } = message
    else {
        panic!("expected block");
    };
    assert_eq!(block.height, 3);
    client.acknowledge(subscriber_id, 3).await.unwrap();
    server.stop().unwrap();
    drop(sub);
    drop(client);
    server.stopped().await;
}

#[tokio::test]
async fn catchup_refuses_archive_corruption_after_recovery() {
    for malformed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut original = fixture();
        let mut store = Store::open(dir.path()).unwrap();
        store.append(&original).unwrap();
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        let hub = Hub::new(dir.path().to_owned(), store.height(), config());
        // Keep valid XDR and the expected height, but invalidate its commitment.
        original.block.payload.consensus_value.prev_hash =
            original.block.identity.consensus_hash.clone();
        let bytes = if malformed {
            vec![0]
        } else {
            xdrs::txs::encode(&original).unwrap()
        };
        std::fs::write(dir.path().join("00000000000000000001.xdr"), bytes).unwrap();
        let (address, server) = hub.serve().await.unwrap();
        let client = connect(address).await;
        let mut sub = subscribe(&client, Cursor::Height { height: 1 }).await;
        assert_eq!(
            receive(&mut sub).await,
            serde_json::json!({
                "type": "closed", "reason": "archive_corrupt", "nextHeight": 1
            })
        );
        assert_eq!(hub.stats()["totals"]["blocksSent"], 0);
        assert_eq!(hub.stats()["totals"]["disconnects"]["archive_corrupt"], 1);
        server.stop().unwrap();
        drop(sub);
        drop(client);
        server.stopped().await;
    }
}

#[tokio::test]
async fn connection_gauge_counts_peers_without_subscriptions_and_clears_on_close() {
    let dir = tempfile::tempdir().unwrap();
    let hub = Hub::new(dir.path().to_owned(), -1, config());
    let (address, server) = hub.serve().await.unwrap();
    let first = connect(address).await;
    let second = connect(address).await;
    assert!(hub.render_metrics().contains("firehose_connections 2\n"));
    assert_eq!(hub.stats()["currentSubscribers"], 0);
    drop(first);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !hub.render_metrics().contains("firehose_connections 1\n") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(second);
    server.stop().unwrap();
    server.stopped().await;
    assert!(hub.render_metrics().contains("firehose_connections 0\n"));
}
