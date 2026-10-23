use std::{collections::VecDeque, sync::Mutex, time::Duration};
use thruput_json_rpc_client::receipt::{
    ReceiptPollConfig, ReceiptPollError, ReceiptSource, poll_receipt,
};
use thruput_json_rpc_types::{
    BlockHeight,
    base58::Base58,
    receipt::{ExecStatus, TransactionReceipt},
    transaction::{TransactionStatus, TxHash},
};

struct Source {
    statuses: Mutex<VecDeque<Option<TransactionStatus>>>,
    receipt_calls: Mutex<usize>,
    receipt_after: Option<usize>,
}

impl ReceiptSource for Source {
    type Error = std::io::Error;

    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, Self::Error> {
        let mut calls = self.receipt_calls.lock().unwrap();
        let receipt = self
            .receipt_after
            .filter(|after| *calls >= *after)
            .map(|_| TransactionReceipt {
                tx_hash,
                height: BlockHeight::from(42),
                status: ExecStatus::Ok,
                events: vec![],
                return_data: None,
                abort_data: None,
                gas_used: 1_500,
            });
        *calls += 1;
        Ok(receipt)
    }

    async fn transaction_status(
        &self,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionStatus>, Self::Error> {
        self.statuses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap()
            .map(Some)
            .ok_or_else(|| std::io::Error::other("status unavailable"))
    }
}

fn polled_hash() -> TxHash {
    TxHash(Base58(thruput_xdr::types::SHA256Hash::from([0x33; 32])))
}

async fn poll(
    statuses: Vec<Option<TransactionStatus>>,
    receipt_after: Option<usize>,
) -> Result<TransactionReceipt, ReceiptPollError<std::io::Error>> {
    let tx_hash = polled_hash();
    let attempts = statuses.len();
    let source = Source {
        statuses: Mutex::new(VecDeque::from(statuses)),
        receipt_calls: Mutex::new(0),
        receipt_after,
    };
    poll_receipt(
        &source,
        tx_hash,
        ReceiptPollConfig::attempts(attempts, Duration::from_millis(1)),
    )
    .await
}

#[tokio::test]
async fn timeout_reports_latest_unknown_after_pending() {
    let error = poll(
        vec![
            Some(TransactionStatus::Pending),
            Some(TransactionStatus::Unknown),
        ],
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        ReceiptPollError::Timeout {
            attempts: 2,
            last_status: Some(TransactionStatus::Unknown),
            ..
        }
    ));
}

#[tokio::test]
async fn status_errors_retain_last_successful_observation() {
    let error = poll(vec![Some(TransactionStatus::Pending), None], None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ReceiptPollError::Timeout {
            last_status: Some(TransactionStatus::Pending),
            ..
        }
    ));
}

#[tokio::test]
async fn unsupported_status_times_out_without_observation() {
    let error = poll(vec![None, None], None).await.unwrap_err();
    assert!(matches!(
        error,
        ReceiptPollError::Timeout {
            last_status: None,
            ..
        }
    ));
}

#[tokio::test]
async fn receipt_after_unknown_succeeds() {
    let receipt = poll(vec![Some(TransactionStatus::Unknown), None], Some(1))
        .await
        .unwrap();
    assert!(matches!(receipt.status, ExecStatus::Ok));
}

#[tokio::test]
async fn invalid_still_stops_polling() {
    let error = poll(
        vec![
            Some(TransactionStatus::Invalid {
                reason: "expired".into(),
            }),
            None,
        ],
        Some(1),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        ReceiptPollError::Status {
            status: TransactionStatus::Invalid { .. },
            ..
        }
    ));
}

#[tokio::test]
async fn scheduler_processing_failures_stop_polling_before_a_later_receipt() {
    use thruput_json_rpc_types::transaction::{
        SchedulerTransactionError, SchedulerTransactionErrorSource,
    };
    for status in [
        TransactionStatus::Retry {
            error: SchedulerTransactionError::new(SchedulerTransactionErrorSource::Scheduler, 1),
        },
        TransactionStatus::Dropped {
            error: SchedulerTransactionError::new(SchedulerTransactionErrorSource::Auth, 2),
        },
    ] {
        let expected = serde_json::to_value(&status).unwrap();
        let error = poll(vec![Some(status), None], Some(1)).await.unwrap_err();
        let ReceiptPollError::Status { status, .. } = error else {
            panic!("unexpected error: {error:?}")
        };
        assert_eq!(serde_json::to_value(status).unwrap(), expected);
    }
}

fn recent_failure() -> TransactionStatus {
    use thruput_json_rpc_types::{
        hex::HexBytes,
        transaction::{ConflictCode, ConflictDiagnostic, TransactionFailureCause},
    };
    TransactionStatus::RecentFailure {
        attempt_height: BlockHeight::from(42),
        cause: TransactionFailureCause::DbConflict {
            conflict: ConflictDiagnostic {
                address: Base58(thruput_xdr::types::ContractAddress::from([0x0D; 32])),
                key: HexBytes(vec![0, 0, 0, 1]),
                code: ConflictCode::ExclusiveLock,
            },
        },
    }
}

#[tokio::test]
async fn exhausted_polling_surfaces_a_recent_failure() {
    let error = poll(vec![Some(recent_failure()); 3], None)
        .await
        .unwrap_err();
    let ReceiptPollError::Status { tx_hash, status } = error else {
        panic!("expected a recent failure status, got {error:?}")
    };
    assert_eq!(tx_hash, polled_hash());
    assert_eq!(
        serde_json::to_value(status).unwrap(),
        serde_json::to_value(recent_failure()).unwrap()
    );
}

#[tokio::test]
async fn a_later_receipt_wins_over_a_recent_failure() {
    let receipt = poll(vec![Some(recent_failure()); 3], Some(1))
        .await
        .unwrap();
    assert_eq!(receipt.tx_hash, polled_hash());
    assert_eq!(*receipt.height, 42);
    assert!(matches!(receipt.status, ExecStatus::Ok));
}
