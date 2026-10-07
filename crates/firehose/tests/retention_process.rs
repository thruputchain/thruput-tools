//! A spawned child holds a copy of every open descriptor until its exec closes
//! them, and with it the `flock` on any archive `LOCK` a sibling test has open.
//! A sibling that drops a `Store` and reopens it in that window gets `WouldBlock`,
//! so this process test runs in its own binary.

#![cfg(unix)]

use std::time::{Duration, Instant};

use groundhog_firehose::store::Store;

mod common;
use common::{fixture, next};

fn archive(path: &std::path::Path, count: usize) -> Store {
    let mut store = Store::open(path).unwrap();
    let mut block = fixture();
    store.adopt(&block.origin).unwrap();
    for _ in 0..count {
        store.append(&block).unwrap();
        block = next(&block);
    }
    store
}

#[test]
fn process_sigterm_checkpoints_and_sigkill_forces_full_recovery() {
    use std::process::{Command, Stdio};
    for clean in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let store = archive(dir.path(), 3);
        store.clean_shutdown().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_hog-firehose-store"))
            .args([
                "run",
                "--event-endpoint",
                "127.0.0.1:1",
                "--listen",
                "127.0.0.1:0",
                "--store",
            ])
            .arg(dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while dir.path().join("verified.xdr").exists() {
            assert!(start.elapsed() < Duration::from_secs(10));
            assert!(child.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        // Allow recovery to finish and the signal future to install its handlers.
        std::thread::sleep(Duration::from_millis(200));
        if clean {
            assert!(
                Command::new("kill")
                    .args(["-TERM", &child.id().to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
        } else {
            child.kill().unwrap();
        }
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > Duration::from_secs(10) {
                child.kill().unwrap();
                panic!("store did not exit after signal");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(dir.path().join("verified.xdr").exists(), clean);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.verified_blocks, if clean { 0 } else { 3 });
    }
}
