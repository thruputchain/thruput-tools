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
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(health_status(address).await.contains("200"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(health_status(address).await.contains("503"));
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
