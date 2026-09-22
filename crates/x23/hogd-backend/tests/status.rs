use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use groundhog_hogd_backend::status::{StatusProbe, transaction_status};
use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::{
    hex::Hex0x,
    transaction::{TransactionStatus, TxHash},
};
use groundhog_trace::{DbConflictCode, DbConflictDiagnostic, TxFailureDiagnostic};

type IndexerAnswer = Result<Option<TransactionStatus>, RpcServerError>;
type MempoolAnswer = Result<bool, RpcServerError>;

/// A probe that answers from a script and records which backend it was
/// asked, in order. Asking past the script is a sequencing bug and panics.
struct Scripted {
    indexer: Mutex<VecDeque<IndexerAnswer>>,
    mempool: Mutex<VecDeque<MempoolAnswer>>,
    asked: Arc<Mutex<Vec<&'static str>>>,
}

impl Scripted {
    fn new(indexer: Vec<IndexerAnswer>, mempool: Vec<MempoolAnswer>) -> Self {
        Self {
            indexer: Mutex::new(indexer.into()),
            mempool: Mutex::new(mempool.into()),
            asked: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn asked(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().clone()
    }
}

impl StatusProbe for Scripted {
    async fn mempool_holds(&self, _tx_hash: TxHash) -> MempoolAnswer {
        self.asked.lock().unwrap().push("mempool");
        self.mempool
            .lock()
            .unwrap()
            .pop_front()
            .expect("bug: mempool asked more often than scripted")
    }

    async fn indexed_status(&self, _tx_hash: TxHash) -> IndexerAnswer {
        self.asked.lock().unwrap().push("indexer");
        self.indexer
            .lock()
            .unwrap()
            .pop_front()
            .expect("bug: indexer asked more often than scripted")
    }
}

fn hash() -> TxHash {
    TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x11; 32])))
}

fn committed() -> TransactionStatus {
    TransactionStatus::Committed { height: 42.into() }
}

fn recent_failure() -> TransactionStatus {
    TransactionStatus::from(&TxFailureDiagnostic {
        attempt_height: 43.into(),
        conflict: DbConflictDiagnostic {
            addrkey: groundhog_xdr::types::AddressAndKey {
                address: [0; 32].into(),
                key: groundhog_xdr::types::StorageKey::FreeNativeTokenBalance,
            },
            code: DbConflictCode::kDbConflictExclusiveLock,
        },
    })
}

fn indexer_down() -> RpcServerError {
    RpcServerError::BackendUnavailable("indexer: down".into())
}

fn mempool_down() -> RpcServerError {
    RpcServerError::BackendUnavailable("scheduler: down".into())
}

#[tokio::test]
async fn a_committed_record_answers_without_asking_the_mempool() {
    let probe = Scripted::new(vec![Ok(Some(committed()))], vec![]);

    let status = transaction_status(&probe, hash()).await.unwrap();

    assert!(matches!(status, TransactionStatus::Committed { height } if height == 42.into()));
    assert_eq!(probe.asked(), ["indexer"]);
}

#[tokio::test]
async fn pending_wins_over_a_miss_a_diagnostic_and_an_indexer_failure() {
    for first in [Ok(None), Ok(Some(recent_failure())), Err(indexer_down())] {
        let probe = Scripted::new(vec![first], vec![Ok(true)]);

        let status = transaction_status(&probe, hash()).await.unwrap();

        assert!(matches!(status, TransactionStatus::Pending));
        assert_eq!(probe.asked(), ["indexer", "mempool"]);
    }
}

#[tokio::test]
async fn a_mempool_miss_returns_the_indexer_diagnostic_or_failure() {
    let probe = Scripted::new(vec![Ok(Some(recent_failure()))], vec![Ok(false)]);
    let status = transaction_status(&probe, hash()).await.unwrap();
    assert!(matches!(status, TransactionStatus::RecentFailure { .. }));
    assert_eq!(probe.asked(), ["indexer", "mempool"]);

    let probe = Scripted::new(vec![Err(indexer_down())], vec![Ok(false)]);
    let error = transaction_status(&probe, hash()).await.unwrap_err();
    assert_eq!(error, indexer_down());
    assert_eq!(probe.asked(), ["indexer", "mempool"]);
}

#[tokio::test]
async fn unknown_requires_both_backends_to_have_missed() {
    let probe = Scripted::new(vec![Ok(None)], vec![Ok(false)]);
    let status = transaction_status(&probe, hash()).await.unwrap();
    assert!(matches!(status, TransactionStatus::Unknown));
    assert_eq!(probe.asked(), ["indexer", "mempool"]);

    let probe = Scripted::new(vec![Ok(None)], vec![Err(mempool_down())]);
    let error = transaction_status(&probe, hash()).await.unwrap_err();
    let RpcServerError::BackendUnavailable(message) = error else {
        panic!("expected BackendUnavailable, got {error:?}");
    };
    assert!(message.starts_with("scheduler: down"), "{message}");
    assert!(message.contains("may still be pending"), "{message}");
    assert_eq!(probe.asked(), ["indexer", "mempool"]);

    let probe = Scripted::new(vec![Ok(None)], vec![Err(RpcServerError::BackendTimeout)]);
    let error = transaction_status(&probe, hash()).await.unwrap_err();
    assert_eq!(error, RpcServerError::BackendTimeout);
}
