use std::{collections::VecDeque, future::pending, sync::Mutex};

use jsonrpsee::types::ErrorObjectOwned;
use serde_json::{Value, json};
use thruput_json_rpc_types::{base58::Base58, responses::ReceiptExecution};

use super::*;

type ReceiptRead = Result<DecodedResponse<ReceiptResult>, NativeClientError>;
type StatusRead = Result<DecodedResponse<TransactionStatus>, NativeClientError>;

#[derive(Default)]
struct Source {
    receipts: Mutex<VecDeque<ReceiptRead>>,
    statuses: Mutex<VecDeque<StatusRead>>,
    requested: Mutex<Vec<TxHash>>,
    hang_receipt: bool,
    hang_status: bool,
}

impl ReceiptSource for Source {
    async fn transaction_receipt(&self, hash: TxHash) -> ReceiptRead {
        self.requested.lock().unwrap().push(hash);
        if self.hang_receipt {
            return pending().await;
        }
        self.receipts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(DecodedResponse::Known(None)))
    }

    async fn transaction_status(&self, hash: TxHash) -> StatusRead {
        self.requested.lock().unwrap().push(hash);
        if self.hang_status {
            return pending().await;
        }
        self.statuses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(DecodedResponse::Known(TransactionStatus::Unknown {})))
    }
}

fn network() -> NetworkHash {
    Base58([3; 32].into())
}
fn hash() -> TxHash {
    TxHash(Base58([4; 32].into()))
}
fn known() -> KnownCommitment {
    KnownCommitment {
        network_hash: network(),
        transaction_hash: hash(),
        block_height: 7.into(),
    }
}
fn config() -> ReceiptPollConfig {
    ReceiptPollConfig {
        timeout: Duration::from_secs(1),
        interval: Duration::from_millis(10),
    }
}
fn receipt_raw(execution: Value) -> Value {
    let mut raw = json!({"transactionHash": hash(), "blockHeight": "7", "invokedAddresses": [], "events": [], "gasUsed": "9"});
    raw["execution"] = execution;
    raw
}
fn receipt(execution: Value) -> DecodedResponse<ReceiptResult> {
    serde_json::from_value(receipt_raw(execution)).unwrap()
}
fn status(value: Value) -> DecodedResponse<TransactionStatus> {
    serde_json::from_value(value).unwrap()
}
fn make_source(receipts: Vec<ReceiptRead>, statuses: Vec<StatusRead>) -> Source {
    Source {
        receipts: Mutex::new(receipts.into()),
        statuses: Mutex::new(statuses.into()),
        ..Source::default()
    }
}
fn application(data: Value, code: i32) -> NativeClientError {
    let source = ErrorObjectOwned::owned(code, "diagnostic", Some(data));
    NativeClientError::Application {
        data: Box::new(DecodedError::decode(
            "getTransactionReceipt",
            serde_json::to_value(&source).unwrap(),
        )),
        source,
    }
}
fn corrupt_observation(transaction_hash: TxHash, height: &str) -> NativeClientError {
    application(
        json!({"kind": "backendDataInvalid", "reason": "inconsistentResult", "observation": {
            "transactionHash": transaction_hash, "blockHeight": height
        }}),
        -32054,
    )
}
async fn wait(
    source: &Source,
    known: Option<KnownCommitment>,
) -> Result<TransactionReceipt, ReceiptPollError> {
    poll_receipt(source, network(), hash(), known, config(), pending()).await
}

#[tokio::test(start_paused = true)]
async fn returns_success_and_failed_execution_without_submission() {
    for execution in [
        json!({"result": "return", "returnData": "0x0102"}),
        json!({"result": "abort", "abortData": "0x"}),
        json!({"result": "fault"}),
    ] {
        let source = make_source(vec![Ok(receipt(execution.clone()))], vec![]);
        let receipt = wait(&source, None).await.unwrap();
        assert_eq!(serde_json::to_value(receipt.execution).unwrap(), execution);
        assert_eq!(*source.requested.lock().unwrap(), [hash()]);
    }
}

#[tokio::test(start_paused = true)]
async fn commitment_survives_scheduler_statuses_unknown_and_errors() {
    let source = make_source(
        vec![],
        vec![
            Ok(status(json!({"status": "committed", "blockHeight": "7"}))),
            Ok(status(
                json!({"status": "retry", "error": {"source": "scheduler", "code": 4}}),
            )),
            Ok(status(
                json!({"status": "dropped", "error": {"source": "auth", "code": 2}, "recentFailure": {"attemptBlockHeight":"6", "cause":{"kind":"contractRejected", "contractIndex":0, "code":"tooMany"}}}),
            )),
            Ok(status(json!({"status": "unknown"}))),
            Err(NativeClientError::Transport(TransportError::RequestTimeout)),
        ],
    );
    let error = wait(&source, None).await.unwrap_err();
    assert!(matches!(error.cause, ReceiptPollCause::Timeout));
    assert_eq!(error.evidence.commitment, Some(known()));
    assert!(matches!(
        error.evidence.last_status,
        Some(TransactionStatus::Committed { ref block_height }) if **block_height == 7
    ));
    assert!(error.evidence.last_lookup_error.is_some());
    assert!(source.statuses.lock().unwrap().is_empty());
    assert!(
        source
            .requested
            .lock()
            .unwrap()
            .iter()
            .all(|requested| *requested == hash())
    );
}

#[tokio::test(start_paused = true)]
async fn retry_and_dropped_do_not_preempt_later_receipt_without_commitment() {
    let source = make_source(
        vec![
            Ok(DecodedResponse::Known(None)),
            Ok(DecodedResponse::Known(None)),
            Ok(receipt(json!({"result": "fault"}))),
        ],
        vec![
            Ok(status(
                json!({"status": "retry", "error": {"source": "scheduler", "code": 1}, "recentFailure": {"attemptBlockHeight":"6", "cause":{"kind":"contractRejected", "contractIndex":0, "code":"tooMany"}}}),
            )),
            Ok(status(
                json!({"status": "dropped", "error": {"source": "auth", "code": 2}, "recentFailure": {"attemptBlockHeight":"6", "cause":{"kind":"contractRejected", "contractIndex":0, "code":"tooMany"}}}),
            )),
        ],
    );
    assert!(matches!(
        wait(&source, None).await.unwrap().execution,
        ReceiptExecution::Fault {}
    ));
}

#[tokio::test(start_paused = true)]
async fn recent_failure_cannot_replace_commitment_at_timeout() {
    let source = make_source(
        vec![],
        vec![
            Ok(status(
                json!({"status": "recentFailure", "attemptBlockHeight": "6", "cause": {"kind": "contractRejected", "contractIndex": 0, "code": "tooMany"}}),
            )),
            Ok(status(json!({"status": "committed", "blockHeight": "7"}))),
        ],
    );
    let error = wait(&source, None).await.unwrap_err();
    assert!(matches!(error.cause, ReceiptPollCause::Timeout));
    assert_eq!(error.evidence.commitment, Some(known()));
    assert!(matches!(
        error.evidence.last_status,
        Some(TransactionStatus::Committed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn supplied_commitment_checks_network_hash_and_height_before_reading() {
    let invalid = [
        KnownCommitment {
            network_hash: Base58([9; 32].into()),
            ..known()
        },
        KnownCommitment {
            transaction_hash: TxHash(Base58([8; 32].into())),
            ..known()
        },
        KnownCommitment {
            block_height: (-1).into(),
            ..known()
        },
    ];
    for commitment in invalid {
        let source = Source::default();
        let error = wait(&source, Some(commitment)).await.unwrap_err();
        assert!(matches!(error.cause, ReceiptPollCause::Invalid { .. }));
        assert_eq!(error.evidence.transaction_hash, hash());
        assert!(error.evidence.commitment.is_none());
        assert!(source.requested.lock().unwrap().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn receipt_hash_and_height_mismatches_preserve_prior_identity() {
    for field in ["transactionHash", "blockHeight"] {
        let mut raw = receipt_raw(json!({"result": "return", "returnData": "0x"}));
        raw[field] = if field == "transactionHash" {
            json!(TxHash(Base58([8; 32].into())))
        } else {
            json!("8")
        };
        let source = make_source(
            vec![Ok(serde_json::from_value(raw.clone()).unwrap())],
            vec![],
        );
        let error = wait(&source, Some(known())).await.unwrap_err();
        assert_eq!(error.evidence.commitment, Some(known()));
        assert_eq!(error.evidence.transaction_hash, hash());
        assert!(
            matches!(error.cause, ReceiptPollCause::Invalid { raw: Some(observed), .. } if observed == raw)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn conflicting_status_height_keeps_original_commitment() {
    let source = make_source(
        vec![],
        vec![Ok(status(
            json!({"status": "committed", "blockHeight": "8"}),
        ))],
    );
    let error = wait(&source, Some(known())).await.unwrap_err();
    assert!(matches!(error.cause, ReceiptPollCause::Invalid { .. }));
    assert_eq!(error.evidence.commitment, Some(known()));
}

#[tokio::test(start_paused = true)]
async fn unknown_execution_retains_raw_and_validated_commitment() {
    let raw = receipt_raw(json!({"result": "futureOutcome", "nested": {"payload": [1, 2]}}));
    let source = make_source(
        vec![Ok(serde_json::from_value(raw.clone()).unwrap())],
        vec![],
    );
    let error = wait(&source, None).await.unwrap_err();
    assert_eq!(error.evidence.commitment, Some(known()));
    assert!(
        matches!(error.cause, ReceiptPollCause::Unsupported { method: "getTransactionReceipt", raw: retained } if retained == raw)
    );
}

#[tokio::test(start_paused = true)]
async fn unknown_status_retains_raw_and_prior_commitment() {
    let raw = json!({"status": "futureStatus", "nested": {"payload": [1, 2]}});
    let source = make_source(vec![], vec![Ok(status(raw.clone()))]);
    let error = wait(&source, Some(known())).await.unwrap_err();
    assert_eq!(error.evidence.commitment, Some(known()));
    assert!(
        matches!(error.cause, ReceiptPollCause::Unsupported { method: "getTransactionStatus", raw: retained } if retained == raw)
    );
}

#[tokio::test(start_paused = true)]
async fn timeout_without_commitment_retains_the_recent_attempt_diagnostic() {
    let raw = json!({
        "status":"retry", "error":{"source":"executor", "code":3},
        "recentFailure":{"attemptBlockHeight":"6", "cause":{
            "kind":"contractRejected", "contractIndex":0, "code":"tooMany"
        }}
    });
    let source = make_source(vec![], vec![Ok(status(raw.clone()))]);
    let error = poll_receipt(
        &source,
        network(),
        hash(),
        None,
        ReceiptPollConfig {
            timeout: Duration::from_millis(10),
            interval: Duration::from_secs(1),
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error.cause, ReceiptPollCause::Timeout));
    assert!(error.evidence.commitment.is_none());
    assert_eq!(
        serde_json::to_value(error.evidence.last_status.unwrap()).unwrap(),
        raw
    );
    assert_eq!(*source.requested.lock().unwrap(), [hash(), hash()]);
}

#[tokio::test(start_paused = true)]
async fn unknown_attempt_diagnostic_retains_complete_status_and_prior_commitment() {
    let raw = json!({
        "status":"retry", "error":{"source":"executor", "code":3},
        "recentFailure":{"attemptBlockHeight":"6", "cause":{"kind":"future", "payload":[1,2]}}
    });
    let source = make_source(vec![], vec![Ok(status(raw.clone()))]);
    let error = wait(&source, Some(known())).await.unwrap_err();
    assert_eq!(error.evidence.commitment, Some(known()));
    assert!(matches!(error.cause,
        ReceiptPollCause::Unsupported { method: "getTransactionStatus", raw: retained } if retained == raw));
    assert!(source.statuses.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn corrupt_receipt_retains_only_matching_valid_observation() {
    let source = make_source(vec![Err(corrupt_observation(hash(), "7"))], vec![]);
    let error = wait(&source, None).await.unwrap_err();
    assert_eq!(error.evidence.commitment, Some(known()));
    assert!(matches!(
        error.cause,
        ReceiptPollCause::Lookup(NativeClientError::Application { .. })
    ));

    for (observed_hash, height) in [(TxHash(Base58([8; 32].into())), "7"), (hash(), "-1")] {
        let source = make_source(
            vec![Err(corrupt_observation(observed_hash, height))],
            vec![],
        );
        let error = wait(&source, None).await.unwrap_err();
        assert!(error.evidence.commitment.is_none());
        assert_eq!(error.evidence.transaction_hash, hash());
    }
}

#[tokio::test(start_paused = true)]
async fn lookup_failure_keeps_commitment_and_transport_cause() {
    let source = make_source(
        vec![Err(NativeClientError::Transport(TransportError::Custom(
            "original cause".into(),
        )))],
        vec![],
    );
    let error = wait(&source, Some(known())).await.unwrap_err();
    assert_eq!(error.evidence.commitment, Some(known()));
    assert!(
        matches!(error.cause, ReceiptPollCause::Lookup(NativeClientError::Transport(TransportError::Custom(message))) if message == "original cause")
    );
}

#[tokio::test(start_paused = true)]
async fn transient_receipt_reads_retry_and_deadlines_bound_each_lookup() {
    let source = make_source(
        vec![
            Err(application(json!({"kind": "backendUnavailable"}), -32050)),
            Err(NativeClientError::Transport(TransportError::RequestTimeout)),
            Ok(receipt(json!({"result": "return", "returnData": "0x"}))),
        ],
        vec![],
    );
    wait(&source, Some(known())).await.unwrap();
    assert_eq!(source.requested.lock().unwrap().len(), 3);

    for hang_status in [false, true] {
        let source = Source {
            hang_receipt: !hang_status,
            hang_status,
            ..Source::default()
        };
        let start = Instant::now();
        let error = wait(&source, Some(known())).await.unwrap_err();
        assert!(matches!(error.cause, ReceiptPollCause::Timeout));
        assert_eq!(Instant::now() - start, config().timeout);
        assert_eq!(error.evidence.commitment, Some(known()));
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_bounds_each_read_and_sleep_preserving_evidence() {
    for (hang_receipt, hang_status) in [(true, false), (false, true), (false, false)] {
        let source = Source {
            hang_receipt,
            hang_status,
            ..Source::default()
        };
        let start = Instant::now();
        let error = poll_receipt(&source, network(), hash(), Some(known()), config(), async {
            tokio::time::sleep(Duration::from_millis(5)).await;
        })
        .await
        .unwrap_err();
        assert!(matches!(error.cause, ReceiptPollCause::Cancelled));
        assert_eq!(Instant::now() - start, Duration::from_millis(5));
        assert_eq!(error.evidence.commitment, Some(known()));
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_preserves_last_transient_error() {
    let source = make_source(
        vec![Err(NativeClientError::Transport(
            TransportError::RequestTimeout,
        ))],
        vec![],
    );
    let error = wait(&source, Some(known())).await.unwrap_err();
    assert!(matches!(error.cause, ReceiptPollCause::Timeout));
    assert!(matches!(
        error.evidence.last_lookup_error,
        Some(NativeClientError::Transport(TransportError::RequestTimeout))
    ));
    assert_eq!(error.evidence.commitment, Some(known()));
}

#[test]
fn defaults_and_backoff_are_bounded() {
    assert_eq!(
        ReceiptPollConfig::default().timeout,
        Duration::from_secs(30)
    );
    assert_eq!(
        ReceiptPollConfig::default().interval,
        Duration::from_millis(250)
    );
    let interval = ReceiptPollConfig::default().interval;
    assert_eq!(retry_pause(interval, 0), interval);
    for failures in 1..100 {
        let pause = retry_pause(interval, failures);
        assert!(pause >= interval);
        assert!(pause <= Duration::from_secs(2));
    }
}

#[tokio::test(start_paused = true)]
async fn invalid_timing_fails_before_reads_and_long_interval_cannot_overrun_deadline() {
    for invalid in [
        ReceiptPollConfig {
            interval: Duration::ZERO,
            ..config()
        },
        ReceiptPollConfig {
            timeout: Duration::MAX,
            ..config()
        },
    ] {
        let source = Source::default();
        let error = poll_receipt(
            &source,
            network(),
            hash(),
            Some(known()),
            invalid,
            pending(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error.cause, ReceiptPollCause::Invalid { .. }));
        assert!(source.requested.lock().unwrap().is_empty());
        assert_eq!(error.evidence.commitment, Some(known()));
    }
    let source = make_source(
        vec![Err(NativeClientError::Transport(
            TransportError::RequestTimeout,
        ))],
        vec![],
    );
    let start = Instant::now();
    let error = poll_receipt(
        &source,
        network(),
        hash(),
        Some(known()),
        ReceiptPollConfig {
            interval: Duration::MAX,
            ..config()
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error.cause, ReceiptPollCause::Timeout));
    assert_eq!(Instant::now() - start, config().timeout);
}
