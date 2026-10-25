//! A spawned child holds a copy of every open descriptor until its exec closes
//! them, and with it the `flock` on any archive `LOCK` a sibling test has open.
//! A sibling that drops a `Store` and reopens it in that window gets `WouldBlock`,
//! so this process test runs in its own binary.

use std::time::Duration;

use thruput_firehose::store::Store;

mod common;
use common::{fixture, next};

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

/// The `--tip-stale-seconds` the server runs with.
const STALE_AFTER: Duration = Duration::from_secs(1);
/// Scheduling slack on top of an expected transition. The stale window is
/// wall-clock, so a fixed sleep either wastes time or fails on a slow runner.
const SLACK: Duration = Duration::from_secs(3);

/// Poll until health reports `expected`, failing if that takes until
/// `deadline`. Deadlines are anchored before the event that starts the clock,
/// so a stale window longer than `STALE_AFTER + SLACK` fails.
async fn wait_for_health(
    address: std::net::SocketAddr,
    expected: &str,
    deadline: tokio::time::Instant,
) {
    tokio::time::timeout_at(deadline, async {
        loop {
            if health_status(address).await.contains(expected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("health did not report {expected} by its deadline"));
}

#[tokio::test]
async fn serve_http_health_is_starting_then_healthy_then_stale_without_writer() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("archive");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let spawned = tokio::time::Instant::now();
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
            &STALE_AFTER.as_secs().to_string(),
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
    wait_for_health(address, "200", tokio::time::Instant::now() + SLACK).await;
    wait_for_health(address, "503", spawned + STALE_AFTER + SLACK).await;
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
    let appended = tokio::time::Instant::now();
    writer.append(&first).unwrap();
    wait_for_health(address, "200", appended + SLACK).await;
    drop(writer);
    wait_for_health(address, "503", appended + STALE_AFTER + SLACK).await;
    assert!(process.try_wait().unwrap().is_none());
    let mut writer = Store::open(&archive).unwrap();
    let appended = tokio::time::Instant::now();
    writer.append(&next(&first)).unwrap();
    wait_for_health(address, "200", appended + SLACK).await;
    process.kill().await.unwrap();
    process.wait().await.unwrap();
}
