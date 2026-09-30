use groundhog_firehose::{
    client::{Outcome, outcome},
    store::Store,
};
use groundhog_indexer::{EventIndexerPullResult, IndexerProgress};

mod common;
use common::{fixture, next, rehash};

#[test]
fn every_pull_outcome_is_distinct() {
    let block = fixture();
    assert_eq!(
        outcome(&EventIndexerPullResult::EventIndexerPullCaughtUp {
            origin: block.origin.clone()
        }),
        Outcome::CaughtUp
    );
    assert_eq!(
        outcome(&EventIndexerPullResult::EventIndexerPullBlock {
            pulled_block: block
        }),
        Outcome::Block
    );
    for (code, expected) in [
        (2, Outcome::NotReady),
        (3, Outcome::MismatchedProtocol),
        (4, Outcome::ProgressConflict),
        (5, Outcome::InternalFailure),
        (99, Outcome::Unknown),
    ] {
        assert_eq!(
            outcome(&EventIndexerPullResult::Default {
                outcome: code,
                diagnostic: "test".try_into().unwrap()
            }),
            expected
        );
    }
}

#[test]
fn live_node_fixture_recovers_cursor_and_replay_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let block = fixture();
    let mut store = Store::open(dir.path()).unwrap();
    assert!(store.append(&block).unwrap());
    assert!(!store.append(&block).unwrap());
    assert!(Store::open(dir.path()).is_err(), "exclusive writer lock");
    let cursor = store.cursor(false);
    let bytes = store.bytes;
    drop(store);
    let mut recovered = Store::open(dir.path()).unwrap();
    assert_eq!(recovered.cursor(false), cursor);
    assert_eq!(recovered.bytes, bytes);
    assert_eq!(recovered.blocks, 1);
    assert!(recovered.append(&next(&block)).unwrap());
    assert_eq!(recovered.height(), 2);
}

#[test]
fn recovery_rejects_corrupt_events_transactions_and_consensus() {
    for field in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let mut block = fixture();
        let mut store = Store::open(dir.path()).unwrap();
        store.append(&block).unwrap();
        drop(store);
        match field {
            0 => {
                block.block.payload.event_logs.0[0].events.0.clear();
            }
            1 => {
                block.block.payload.tx_bodies.0.clear();
            }
            2 => {
                block.block.payload.consensus_value.proposal_header.txs_hash =
                    groundhog_xdr::types::TxsHash::new([0; 32].into());
                rehash(&mut block);
            }
            _ => {
                block.block.identity.consensus_hash =
                    groundhog_xdr::types::ConsensusValueHash::new([0; 32].into());
            }
        }
        std::fs::write(
            dir.path().join("00000000000000000001.xdr"),
            xdrs::txs::encode(&block).unwrap(),
        )
        .unwrap();
        assert!(Store::open(dir.path()).is_err());
    }
}

#[test]
fn rejects_gaps_rewrites_wrong_parent_and_invalid_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let block = fixture();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(&block).unwrap();
    let mut gap = next(&next(&block));
    assert!(store.append(&gap).is_err());
    gap = next(&block);
    gap.block.payload.consensus_value.prev_hash = block.origin.first_prev_hash.clone();
    rehash(&mut gap);
    assert!(store.append(&gap).is_err());
    gap = next(&block);
    gap.origin.epoch_sequence += 1;
    assert!(store.append(&gap).is_err());
    let mut rewrite = block.clone();
    rewrite.origin.epoch_sequence += 1;
    assert!(store.append(&rewrite).is_err());
    assert_eq!(store.blocks, 1);
}

#[test]
fn epoch_boundary_and_empty_origin_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let block = fixture();
    let mut store = Store::open(dir.path()).unwrap();
    store.adopt(&block.origin).unwrap();
    drop(store);
    let mut store = Store::open(dir.path()).unwrap();
    assert!(matches!(
        store.cursor(false),
        IndexerProgress::IndexerProgressInitialized { .. }
    ));
    store.append(&block).unwrap();
    let mut second = next(&block);
    second.origin.epoch_sequence += 1;
    second.origin.first_height = second
        .block
        .payload
        .consensus_value
        .proposal_header
        .block_height
        .clone();
    second.origin.first_prev_hash = block.block.identity.consensus_hash.clone();
    store.adopt(&second.origin).unwrap();
    store.append(&second).unwrap();
    let expected = store.cursor(false);
    drop(store);
    assert_eq!(Store::open(dir.path()).unwrap().cursor(false), expected);
}

#[test]
fn interrupted_unpublished_write_is_ignored_but_published_truncation_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let block = fixture();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(&block).unwrap();
    drop(store);
    std::fs::write(dir.path().join("pending"), [1, 2, 3]).unwrap();
    assert_eq!(Store::open(dir.path()).unwrap().blocks, 1);
    std::fs::write(dir.path().join("00000000000000000001.xdr"), [1, 2, 3]).unwrap();
    assert!(Store::open(dir.path()).is_err());
}

#[test]
fn crash_after_publication_never_truncates_the_committed_inode() {
    let dir = tempfile::tempdir().unwrap();
    let block = fixture();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(&block).unwrap();
    drop(store);
    let published = dir.path().join("00000000000000000001.xdr");
    let original = std::fs::read(&published).unwrap();
    std::fs::hard_link(&published, dir.path().join("pending")).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(&next(&block)).unwrap();
    drop(store);
    assert_eq!(std::fs::read(published).unwrap(), original);
    assert_eq!(Store::open(dir.path()).unwrap().blocks, 2);
}
