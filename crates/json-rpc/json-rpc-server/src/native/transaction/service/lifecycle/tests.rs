use super::*;
use crate::{RpcServerConfig, TipReading, native::transaction::NativeReadError};
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use thruput_json_rpc_types::{
    base58::Base58,
    block::ChainTip,
    error::native::{BackendDataInvalidReason, ReceiptObservation, codes},
    hex::HexBytes,
};
use thruput_xdr::network_config::{NetworkConfig, NetworkConfigV0};
use xdr_common::VecM;

#[derive(Clone, Copy, Default)]
enum Mode {
    #[default]
    Accept,
    Queued,
    Dispatched,
}
#[derive(Clone, Copy, Default)]
enum Precheck {
    #[default]
    Ready,
    Pending,
    Slow,
}
#[derive(Clone, Default)]
struct Backend {
    mode: Mode,
    calls: Arc<AtomicUsize>,
    attempt: Arc<Mutex<Option<SubmissionAttempt>>>,
    submitted: Arc<Mutex<Option<Vec<u8>>>>,
    corrupt_receipt: bool,
    precheck: Precheck,
    observation: Option<ReceiptObservation>,
    read_error: bool,
    receipt_height: Option<i64>,
    status: Option<out::TransactionStatus>,
}
#[async_trait]
impl ChainPlane for Backend {
    async fn chain_tip(&self, _: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        if matches!(self.precheck, Precheck::Slow) {
            // Model non-yielding preparation that runs past the request budget.
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if matches!(self.precheck, Precheck::Pending) {
            std::future::pending::<()>().await;
        }
        Ok(TipReading::new(
            ChainTip {
                height: 42.into(),
                block_hash: Base58([8; 32].into()),
            },
            NetworkConfig::_0 {
                v0: NetworkConfigV0 {
                    params: VecM(vec![]),
                },
            },
        ))
    }
    async fn pending_transaction_count(&self, _: RpcRequestContext) -> Result<u64, RpcServerError> {
        panic!("not used")
    }
}
#[async_trait]
impl NativeSubmitPlane for Backend {
    async fn native_submit(
        &self,
        _: RpcRequestContext,
        tx: ValidatedTransaction,
        attempt: SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.submitted.lock().unwrap() = Some(tx.bytes);
        *self.attempt.lock().unwrap() = Some(attempt.clone());
        if matches!(self.mode, Mode::Accept | Mode::Dispatched) {
            assert!(attempt.begin_dispatch());
        }
        if !matches!(self.mode, Mode::Accept) {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}
#[async_trait]
impl NativeTransactionReadPlane for Backend {
    async fn native_input(
        &self,
        _: RpcRequestContext,
        _: TxHash,
    ) -> Result<out::TransactionInputResult, NativeReadError> {
        Ok(out::TransactionInputResult::Unavailable {})
    }
    async fn native_receipt(
        &self,
        _: RpcRequestContext,
        hash: TxHash,
    ) -> Result<Option<out::TransactionReceipt>, NativeReadError> {
        if self.read_error {
            return Err(NativeReadError {
                error: RpcServerError::invalid_data(
                    BackendDataInvalidReason::InvalidEncoding,
                    "broken event",
                ),
                observation: self.observation.clone().map(Box::new),
            });
        }
        Ok(Some(out::TransactionReceipt {
            invoked_addresses: vec![],
            transaction_hash: if self.corrupt_receipt {
                TxHash(Base58([9; 32].into()))
            } else {
                hash
            },
            block_height: self.receipt_height.unwrap_or(42).into(),
            execution: out::ReceiptExecution::Return {
                return_data: HexBytes(vec![]),
            },
            events: vec![],
            gas_used: 10,
        }))
    }
    async fn native_status(
        &self,
        _: RpcRequestContext,
        _: TxHash,
    ) -> Result<out::TransactionStatus, NativeReadError> {
        Ok(self
            .status
            .clone()
            .unwrap_or(out::TransactionStatus::Committed {
                block_height: 42.into(),
            }))
    }
}
fn server(backend: Backend) -> NativeTransactionService<Backend, Backend, Backend> {
    NativeTransactionService::new(
        backend.clone(),
        backend.clone(),
        backend,
        RpcServerConfig {
            request_timeout_ms: Some(25),
            tip_refresh_interval_ms: 0,
            ..RpcServerConfig::default()
        },
    )
}
fn request() -> input::SendTransactionRequest {
    input::SendTransactionRequest {
        signed_transaction: HexBytes(
            xdrs::txs::encode(&crate::tx::test_fixtures::canonical_signed_transaction()).unwrap(),
        ),
    }
}
fn hash() -> TxHash {
    let bytes: [u8; 32] = hex::decode(crate::tx::test_fixtures::CANONICAL_HASH)
        .unwrap()
        .try_into()
        .unwrap();
    TxHash(Base58(bytes.into()))
}
fn data(error: &jsonrpsee::types::ErrorObjectOwned) -> serde_json::Value {
    serde_json::from_str(error.data().unwrap().get()).unwrap()
}

#[tokio::test]
async fn invalid_encoding_and_expiry_are_not_attempted() {
    let backend = Backend::default();
    let server = server(backend.clone());
    let error = server
        .send_transaction(input::SendTransactionRequest {
            signed_transaction: HexBytes(vec![1, 2, 3]),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::TRANSACTION_INVALID);
    assert_eq!(data(&error)["admission"], "notAttempted");
    assert!(data(&error).get("transactionHash").is_none());
    let mut tx = crate::tx::test_fixtures::canonical_signed_transaction();
    tx.tx.uniquifier.block_expiry_height = 42.into();
    let error = server
        .send_transaction(input::SendTransactionRequest {
            signed_transaction: HexBytes(xdrs::txs::encode(&tx).unwrap()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::TRANSACTION_REJECTED);
    assert_eq!(data(&error)["admission"], "notAttempted");
    assert_eq!(data(&error)["reason"], "expired");
    assert!(data(&error)["transactionHash"].is_string());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn accepted_submission_returns_the_canonical_body_hash() {
    let backend = Backend::default();
    let result = server(backend.clone())
        .send_transaction(request())
        .await
        .unwrap();
    assert_eq!(result.transaction_hash, hash());
    assert!(matches!(result.admission, out::Admission::Accepted));
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        backend.submitted.lock().unwrap().as_deref(),
        Some(request().signed_transaction.0.as_slice())
    );
}
#[tokio::test]
async fn timeout_before_dispatch_closes_the_gate_but_dispatched_timeout_is_unknown() {
    for (mode, expected) in [
        (Mode::Queued, "notAttempted"),
        (Mode::Dispatched, "unknown"),
    ] {
        let backend = Backend {
            mode,
            ..Default::default()
        };
        let error = server(backend.clone())
            .send_transaction(request())
            .await
            .unwrap_err();
        assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
        assert_eq!(data(&error)["admission"], expected);
        assert_eq!(
            data(&error)["transactionHash"],
            serde_json::to_value(hash()).unwrap()
        );
        let attempt = backend.attempt.lock().unwrap().clone().unwrap();
        assert!(
            !attempt.begin_dispatch(),
            "timed-out queued work must never dispatch later"
        );
    }
}
#[tokio::test]
async fn caller_cancellation_also_closes_the_queued_dispatch_gate() {
    let backend = Backend {
        mode: Mode::Queued,
        ..Default::default()
    };
    let server = server(backend.clone());
    let mut future = Box::pin(server.send_transaction(request()));
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(future.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let attempt = backend.attempt.lock().unwrap().clone().unwrap();
    drop(future);
    assert!(!attempt.begin_dispatch());
}
#[tokio::test]
async fn receipt_identity_failure_omits_observation_and_valid_evidence_survives() {
    let query = || input::TransactionQuery {
        transaction_hash: hash(),
    };
    let error = server(Backend {
        corrupt_receipt: true,
        ..Default::default()
    })
    .get_transaction_receipt(query())
    .await
    .unwrap_err();
    assert_eq!(error.code(), codes::BACKEND_DATA_INVALID);
    assert!(data(&error).get("observation").is_none());
    for (observed, height, kept) in [
        (hash(), 42, true),
        (hash(), 0, true),
        (hash(), -1, false),
        (TxHash(Base58([9; 32].into())), 42, false),
    ] {
        let error = server(Backend {
            read_error: true,
            observation: Some(ReceiptObservation {
                transaction_hash: observed,
                block_height: height.into(),
            }),
            ..Default::default()
        })
        .get_transaction_receipt(query())
        .await
        .unwrap_err();
        assert_eq!(data(&error).get("observation").is_some(), kept);
        assert!(data(&error).get("admission").is_none());
        if kept {
            assert_eq!(
                data(&error)["observation"]["blockHeight"],
                height.to_string()
            );
        }
    }
}

#[tokio::test]
async fn receipt_height_must_be_nonnegative() {
    let query = || input::TransactionQuery {
        transaction_hash: hash(),
    };
    let receipt = server(Backend {
        receipt_height: Some(0),
        ..Default::default()
    })
    .get_transaction_receipt(query())
    .await
    .unwrap();
    assert_eq!(receipt.map(|receipt| *receipt.block_height), Some(0));
    let error = server(Backend {
        receipt_height: Some(-1),
        ..Default::default()
    })
    .get_transaction_receipt(query())
    .await
    .unwrap_err();
    assert_eq!(error.code(), codes::BACKEND_DATA_INVALID);
    assert_eq!(data(&error)["reason"], "inconsistentResult");
}

#[tokio::test]
async fn status_heights_must_be_nonnegative() {
    let committed = |height: i64| out::TransactionStatus::Committed {
        block_height: height.into(),
    };
    let failed = |height: i64| out::TransactionStatus::RecentFailure {
        attempt_block_height: height.into(),
        cause: out::FailureCause::ContractRejected {
            contract_index: 0,
            code: out::ContractRejectCode::Duplicate,
        },
    };
    for (status, valid) in [
        (committed(0), true),
        (committed(-1), false),
        (failed(0), true),
        (failed(7), true),
        (failed(-1), false),
    ] {
        let result = server(Backend {
            status: Some(status.clone()),
            ..Default::default()
        })
        .get_transaction_status(input::TransactionQuery {
            transaction_hash: hash(),
        })
        .await;
        match result {
            Ok(observed) => {
                assert!(valid, "{status:?}");
                assert_eq!(
                    serde_json::to_value(observed).unwrap(),
                    serde_json::to_value(status).unwrap()
                );
            }
            Err(error) => {
                assert!(!valid, "{status:?}");
                assert_eq!(error.code(), codes::BACKEND_DATA_INVALID);
                assert_eq!(data(&error)["reason"], "inconsistentResult");
                assert!(data(&error).get("admission").is_none());
            }
        }
    }
}

#[tokio::test]
async fn precheck_timeout_has_no_hash_and_cannot_submit() {
    let backend = Backend {
        precheck: Precheck::Pending,
        ..Default::default()
    };
    let error = server(backend.clone())
        .send_transaction(request())
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
    assert_eq!(data(&error)["admission"], "notAttempted");
    assert!(data(&error).get("transactionHash").is_none());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert!(backend.attempt.lock().unwrap().is_none());
}

#[tokio::test]
async fn synchronous_preparation_overrun_cannot_start_submission() {
    let backend = Backend {
        precheck: Precheck::Slow,
        ..Default::default()
    };
    let error = server(backend.clone())
        .send_transaction(request())
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
    assert_eq!(data(&error)["admission"], "notAttempted");
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert!(backend.attempt.lock().unwrap().is_none());
}
