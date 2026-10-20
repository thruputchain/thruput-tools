use thruput_firehose::store::Store;
use thruput_indexer::IndexerProgress;

mod common;
use common::{fixture, next};

#[test]
fn floor_only_archive_recovers_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let first = fixture();
    let second = next(&first);
    let mut progress = None;
    Store::skip_bootstrap(&mut progress, &first).unwrap();
    Store::skip_bootstrap(&mut progress, &second).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let progress = progress.unwrap();
    store.establish_tip(progress.clone()).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    assert_eq!(store.height(), 2);
    assert_eq!(store.floor_height(), Some(3));
    assert_eq!(store.blocks, 0);
    let bytes = store.bytes;
    let allocated = store.allocated_bytes;
    assert!(bytes > 0);
    assert!(allocated > 0);
    store.clean_shutdown().unwrap();
    let mut recovered = Store::open(dir.path()).unwrap();
    assert_eq!(recovered.bytes, bytes);
    assert_eq!(recovered.allocated_bytes, allocated);
    assert_eq!(recovered.blocks, 0);
    assert_eq!(
        recovered.cursor(false),
        IndexerProgress::IndexerProgressInitialized {
            initialized: progress,
        }
    );
    recovered.append(&next(&second)).unwrap();
    assert_eq!(recovered.height(), 3);
    assert_eq!(recovered.blocks, 1);
}

#[test]
fn establish_tip_refuses_an_adopted_or_populated_archive() {
    for populated in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let first = fixture();
        let mut progress = None;
        Store::skip_bootstrap(&mut progress, &first).unwrap();
        store.adopt(&first.origin).unwrap();
        if populated {
            store.append(&first).unwrap();
        }
        let before = store.cursor(false);
        assert!(store.establish_tip(progress.unwrap()).is_err());
        assert_eq!(store.cursor(false), before);
        assert!(!dir.path().join("floor.xdr").exists());
    }
}
