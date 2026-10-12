use std::{
    fs,
    time::{Duration, Instant},
};

use thruput_firehose::store::{Retention, Store};
use thruput_indexer::IndexerInitializedProgress;

mod common;
use common::{fixture, next};

fn budget(min_blocks: u64) -> Retention {
    Retention {
        duration: Duration::MAX,
        bytes: 0,
        min_blocks,
    }
}

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
fn prune_restart_and_continue_preserve_cursor_bytes_and_base() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = archive(dir.path(), 5);
    let cursor = xdrs::txs::encode(&store.cursor(false)).unwrap();
    let parked = xdrs::txs::encode(&store.cursor(true)).unwrap();
    store.prune(budget(2)).unwrap();
    assert_eq!(store.blocks, 2);
    assert_eq!(store.floor_height(), Some(4));
    assert_eq!(xdrs::txs::encode(&store.cursor(false)).unwrap(), cursor);
    assert_eq!(xdrs::txs::encode(&store.cursor(true)).unwrap(), parked);
    store.clean_shutdown().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    assert_eq!(store.floor_height(), Some(4));
    assert_eq!(store.verified_blocks, 0);
    let bytes = fs::read(dir.path().join("00000000000000000005.xdr")).unwrap();
    let last = xdrs::txs::decode(&bytes).unwrap();
    store.append(&next(&last)).unwrap();
    assert_eq!(store.height(), 6);
    store.prune(budget(1)).unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.floor_height(), Some(6));
    assert_eq!(store.verified_blocks, 1);
}

#[test]
fn recovery_discards_leftovers_in_both_prune_crash_windows() {
    for unlinked in [0, 1] {
        let dir = tempfile::tempdir().unwrap();
        let store = archive(dir.path(), 5);
        let cursor = store.cursor(false);
        drop(store);
        let bytes = fs::read(dir.path().join("00000000000000000004.xdr")).unwrap();
        let first: thruput_indexer::EventIndexerPulledBlock = xdrs::txs::decode(&bytes).unwrap();
        let floor = IndexerInitializedProgress {
            origin: first.origin,
            contiguous_height: 3.into(),
            last_consensus_hash: first.block.payload.consensus_value.prev_hash,
        };
        // Model a durable floor publication followed by zero or one unlink.
        fs::write(
            dir.path().join("floor.xdr"),
            xdrs::txs::encode(&floor).unwrap(),
        )
        .unwrap();
        for height in 1..=unlinked {
            fs::remove_file(dir.path().join(format!("{height:020}.xdr"))).unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.cursor(false), cursor);
        assert_eq!(store.blocks, 2);
        assert_eq!(store.floor_height(), Some(4));
        for height in 1..4 {
            assert!(!dir.path().join(format!("{height:020}.xdr")).exists());
        }
    }
}

#[test]
fn watermark_is_single_use_and_unclean_drop_requires_full_verification() {
    let dir = tempfile::tempdir().unwrap();
    let store = archive(dir.path(), 3);
    store.clean_shutdown().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.verified_blocks, 0);
    assert!(!dir.path().join("verified.xdr").exists());
    drop(store); // No clean shutdown, exactly as after a kill.
    assert_eq!(Store::open(dir.path()).unwrap().verified_blocks, 3);
}

#[test]
fn duration_and_allocation_each_bind_but_minimum_never_removes_everything() {
    for duration in [Duration::ZERO, Duration::MAX] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = archive(dir.path(), 4);
        let before = store.allocated_bytes;
        store
            .prune(Retention {
                duration,
                bytes: if duration == Duration::ZERO {
                    u64::MAX
                } else {
                    before - 1
                },
                min_blocks: 2,
            })
            .unwrap();
        assert!((2..4).contains(&store.blocks));
        assert!(store.allocated_bytes < before);
        store.prune(budget(0)).unwrap();
        assert_eq!(store.blocks, 1);
    }
}

#[test]
fn explicit_audit_verifies_below_a_warm_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let store = archive(dir.path(), 2);
    store.clean_shutdown().unwrap();
    let mut block = fixture();
    block.block.payload.event_logs.0[0].events.0.clear();
    fs::write(
        dir.path().join("00000000000000000001.xdr"),
        xdrs::txs::encode(&block).unwrap(),
    )
    .unwrap();
    assert!(Store::open_verified(dir.path()).is_err());
}

#[test]
#[ignore = "manual restart measurement on a 100-transaction archive"]
fn measure_restart_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let hex: String = include_str!("fixtures/hundred-transactions.hex")
        .split_whitespace()
        .collect();
    let mut block: thruput_indexer::EventIndexerPulledBlock =
        xdrs::txs::decode(&hex::decode(hex).unwrap()).unwrap();
    block.origin.first_height = block
        .block
        .payload
        .consensus_value
        .proposal_header
        .block_height
        .clone();
    block.origin.first_prev_hash = block.block.payload.consensus_value.prev_hash.clone();
    let mut store = Store::open(dir.path()).unwrap();
    store.adopt(&block.origin).unwrap();
    for _ in 0..10_000 {
        store.append(&block).unwrap();
        block = next(&block);
    }
    drop(store);
    let start = Instant::now();
    let store = Store::open(dir.path()).unwrap();
    let cold = start.elapsed();
    assert_eq!(store.verified_blocks, 10_000);
    store.clean_shutdown().unwrap();
    let start = Instant::now();
    let store = Store::open(dir.path()).unwrap();
    let warm = start.elapsed();
    assert_eq!(store.verified_blocks, 0);
    println!(
        "blocks={} bytes={} full={cold:?} warm={warm:?}",
        store.blocks, store.bytes
    );
}

#[test]
fn warm_checkpoint_verifies_new_tail_and_preserves_floor_epoch_origin() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = archive(dir.path(), 3);
    let mut fourth = next(&next(&next(&fixture())));
    fourth.origin.epoch_sequence += 1;
    fourth.origin.first_height = 4.into();
    fourth.origin.first_prev_hash = fourth.block.payload.consensus_value.prev_hash.clone();
    store.append(&fourth).unwrap();
    store.prune(budget(1)).unwrap();
    let cursor = store.cursor(false);
    store.clean_shutdown().unwrap();
    // A tail above the checkpoint must still be verified on warm recovery.
    let mut fifth = next(&fourth);
    fifth.block.payload.event_logs.0[0].events.0.clear();
    fs::write(
        dir.path().join("00000000000000000005.xdr"),
        xdrs::txs::encode(&fifth).unwrap(),
    )
    .unwrap();
    assert!(Store::open(dir.path()).is_err());
    fs::remove_file(dir.path().join("00000000000000000005.xdr")).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    assert_eq!(store.cursor(false), cursor);
    assert_eq!(store.verified_blocks, 1);
    store.append(&next(&fourth)).unwrap();
}

#[test]
fn checkpoint_ignores_caught_up_epoch_without_a_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = archive(dir.path(), 1);
    let expected = store.cursor(false);
    let mut new_origin = fixture().origin;
    new_origin.epoch_sequence += 1;
    new_origin.first_height = 2.into();
    new_origin.first_prev_hash = fixture().block.identity.consensus_hash;
    store.adopt(&new_origin).unwrap();
    store.clean_shutdown().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.cursor(false), expected);
    assert_eq!(store.verified_blocks, 0);
}

#[test]
fn watermark_cannot_outlive_its_tip_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = archive(dir.path(), 3);
    store.prune(budget(1)).unwrap();
    store.clean_shutdown().unwrap();
    fs::remove_file(dir.path().join("00000000000000000003.xdr")).unwrap();
    assert!(Store::open(dir.path()).is_err());
}
