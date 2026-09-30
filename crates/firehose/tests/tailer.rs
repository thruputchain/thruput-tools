use std::{
    fs,
    time::{Duration, SystemTime},
};

use groundhog_firehose::{
    metrics::Metrics,
    store::Store,
    subscription::{Config, Cursor, Hub},
    tailer::Tailer,
};
use jsonrpsee::{
    core::client::{ClientT, SubscriptionClientT},
    rpc_params,
    ws_client::WsClientBuilder,
};
use serde_json::Value;

mod common;
use common::{fixture, next};

fn config() -> Config {
    Config {
        ws_listen: "127.0.0.1:0".parse().unwrap(),
        allow_remote: false,
        max_subscribers: 64,
        max_subscriptions_per_connection: None,
        subscriber_queue: 8,
        subscriber_queue_bytes: 2 * 1024 * 1024,
        max_record_bytes: 4 * 1024 * 1024,
        live_timeout_ms: 5000,
        catchup_timeout_ms: 10000,
        catchup_progress_ms: 30000,
    }
}

#[test]
fn discovery_is_name_only_read_only_and_empty_is_healthy() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    let metrics = Metrics::new().unwrap();
    let tailer = Tailer::new(absent.clone()).unwrap();
    tailer.update_health(&metrics, Duration::ZERO);
    assert_eq!(tailer.tip(), -1);
    assert!(metrics.is_healthy());
    assert!(metrics.tip_age.get().is_nan());
    assert!(!absent.exists());
    fs::write(dir.path().join("pending"), b"not a record").unwrap();
    fs::write(dir.path().join("origin.xdr"), b"not decoded").unwrap();
    for height in [2, 9, 10] {
        fs::write(dir.path().join(format!("{height:020}.xdr")), b"not decoded").unwrap();
    }
    assert_eq!(Tailer::new(dir.path().to_owned()).unwrap().tip(), 10);
    assert!(!dir.path().join("LOCK").exists());
}

#[tokio::test]
async fn empty_archive_publishes_first_nonzero_record_to_tip_subscriber() {
    let dir = tempfile::tempdir().unwrap();
    let mut tailer = Tailer::new(dir.path().to_owned()).unwrap();
    let hub = Hub::new(dir.path().to_owned(), tailer.tip(), config());
    let (address, server) = hub.serve().await.unwrap();
    let client = WsClientBuilder::default()
        .build(format!("ws://{address}"))
        .await
        .unwrap();
    let mut sub = client
        .subscribe::<Value, _>(
            "firehose_subscribe",
            rpc_params![Cursor::Tip],
            "firehose_unsubscribe",
        )
        .await
        .unwrap();
    for _ in 0..3 {
        tailer.poll(&hub).await.unwrap();
        assert_eq!(tailer.tip(), -1);
    }
    let mut first = fixture();
    first.origin.first_height = 700_000.into();
    first
        .block
        .payload
        .consensus_value
        .proposal_header
        .block_height = 700_000.into();
    common::rehash(&mut first);
    let mut writer = Store::open(dir.path()).unwrap();
    writer.append(&first).unwrap();
    writer.append(&next(&first)).unwrap();
    tailer.poll(&hub).await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(message["block"]["height"], 700_000);
    assert_eq!(tailer.tip(), 700_001);
    server.stop().unwrap();
    drop(sub);
    drop(client);
    server.stopped().await;
}

#[tokio::test]
async fn writer_stops_and_returns_without_disconnect_gap_or_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Store::open(dir.path()).unwrap();
    let first = fixture();
    writer.append(&first).unwrap();
    let mut tailer = Tailer::new(dir.path().to_owned()).unwrap();
    let hub = Hub::new(dir.path().to_owned(), tailer.tip(), config());
    let (address, server) = hub.serve().await.unwrap();
    let client = WsClientBuilder::default()
        .build(format!("ws://{address}"))
        .await
        .unwrap();
    let mut sub = client
        .subscribe::<Value, _>(
            "firehose_subscribe",
            rpc_params![Cursor::Tip],
            "firehose_unsubscribe",
        )
        .await
        .unwrap();
    let mut current = first;
    for _ in 0..3 {
        current = next(&current);
        writer.append(&current).unwrap();
    }
    tailer.poll(&hub).await.unwrap();
    for height in 2..=4 {
        let message = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(message["block"]["height"], height);
        client
            .request::<(), _>(
                "firehose_ack",
                rpc_params![message["subscriberId"].as_u64().unwrap(), height],
            )
            .await
            .unwrap();
    }
    drop(writer);
    tailer.poll(&hub).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), sub.next())
            .await
            .is_err()
    );
    assert_eq!(hub.stats()["subscribers"][0]["nextHeight"], 5);
    let mut writer = Store::open(dir.path()).unwrap();
    current = next(&current);
    writer.append(&current).unwrap();
    tailer.poll(&hub).await.unwrap();
    tailer.poll(&hub).await.unwrap();
    let message = sub.next().await.unwrap().unwrap();
    assert_eq!(message["block"]["height"], 5);
    client
        .request::<(), _>(
            "firehose_ack",
            rpc_params![message["subscriberId"].as_u64().unwrap(), 5],
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), sub.next())
            .await
            .is_err()
    );
    server.stop().unwrap();
    drop(sub);
    drop(client);
    server.stopped().await;
}

#[tokio::test]
async fn stale_tip_survives_reader_restart_and_new_records_restore_health() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Store::open(dir.path()).unwrap();
    let first = fixture();
    writer.append(&first).unwrap();
    let path = dir.path().join(format!("{:020}.xdr", 1));
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_mins(2))
        .unwrap();
    let mut tailer = Tailer::new(dir.path().to_owned()).unwrap();
    let metrics = Metrics::new().unwrap();
    tailer.update_health(&metrics, Duration::from_mins(1));
    assert!(!metrics.is_healthy());
    assert!(metrics.tip_age.get() >= 120.);
    assert!(
        String::from_utf8(metrics.render().unwrap())
            .unwrap()
            .contains("firehose_healthy 0")
    );
    let hub = Hub::new(dir.path().to_owned(), tailer.tip(), config());
    writer.append(&next(&first)).unwrap();
    tailer.poll(&hub).await.unwrap();
    tailer.update_health(&metrics, Duration::from_mins(1));
    assert!(metrics.is_healthy());
    assert!(metrics.tip_age.get() < 60.);
}

#[tokio::test]
async fn corrupt_or_misnamed_records_never_advance_the_tip() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Store::open(dir.path()).unwrap();
    writer.append(&fixture()).unwrap();
    let mut tailer = Tailer::new(dir.path().to_owned()).unwrap();
    let hub = Hub::new(dir.path().to_owned(), tailer.tip(), config());
    let path = dir.path().join(format!("{:020}.xdr", 2));
    let mut corrupt = next(&fixture());
    corrupt.block.identity.consensus_hash = fixture().block.identity.consensus_hash;
    fs::write(&path, xdrs::txs::encode(&corrupt).unwrap()).unwrap();
    assert!(tailer.poll(&hub).await.is_err());
    assert_eq!(tailer.tip(), 1);
    fs::write(&path, xdrs::txs::encode(&fixture()).unwrap()).unwrap();
    assert!(tailer.poll(&hub).await.is_err());
    assert_eq!(tailer.tip(), 1);
}

async fn health_status(address: std::net::SocketAddr) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).await.unwrap();
    response.lines().next().unwrap().to_owned()
}

#[tokio::test]
async fn serve_http_health_is_starting_then_healthy_then_stale_without_writer() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("archive");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut process = tokio::process::Command::new(env!("CARGO_BIN_EXE_hog-firehose-serve"))
        .args([
            "--store",
            archive.to_str().unwrap(),
            "--listen",
            &address.to_string(),
            "--ws-listen",
            "127.0.0.1:0",
            "--poll-interval-ms",
            "10",
            "--tip-stale-seconds",
            "1",
        ])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            assert!(process.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(health_status(address).await.contains("200"));
    assert!(
        !archive.exists(),
        "serve must not create its archive or LOCK"
    );
    let mut writer = Store::open(&archive).unwrap();
    let mut first = fixture();
    first.origin.first_height = 0.into();
    first
        .block
        .payload
        .consensus_value
        .proposal_header
        .block_height = 0.into();
    common::rehash(&mut first);
    writer.append(&first).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(health_status(address).await.contains("200"));
    drop(writer);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(health_status(address).await.contains("503"));
    assert!(process.try_wait().unwrap().is_none());
    let mut writer = Store::open(&archive).unwrap();
    writer.append(&next(&first)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(health_status(address).await.contains("200"));
    process.kill().await.unwrap();
    process.wait().await.unwrap();
}
