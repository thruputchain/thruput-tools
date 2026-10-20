use super::*;
use crate::{BuildInfo, RpcServerConfig, TipReading, native::transaction::NativeReadError};
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use thruput_json_rpc_types::{
    BlockHeight,
    base58::Base58,
    block::{BlockInfo, ChainTip},
    error::native::{BackendDataInvalidReason, ReceiptObservation, codes},
    hex::HexBytes,
    network_info::NetworkInfo,
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
    corrupt_receipt: bool,
    precheck: Precheck,
    observation: Option<ReceiptObservation>,
    read_error: bool,
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
    async fn get_block_by_number(
        &self,
        _: RpcRequestContext,
        _: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        panic!("not used")
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
        _: ValidatedTransaction,
        attempt: SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
            transaction_hash: if self.corrupt_receipt {
                TxHash(Base58([9; 32].into()))
            } else {
                hash
            },
            block_height: 42.into(),
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
        Ok(out::TransactionStatus::Committed {
            block_height: 42.into(),
        })
    }
}
fn server(backend: Backend) -> GroundhogRpcServer<Backend, Backend, Backend, ()> {
    GroundhogRpcServer::new(
        backend.clone(),
        backend.clone(),
        backend,
        (),
        NetworkInfo::new(Base58([7; 32].into())),
        RpcServerConfig {
            request_timeout_ms: Some(25),
            tip_refresh_interval_ms: 0,
            ..RpcServerConfig::default()
        },
        BuildInfo::new("test", None),
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
        .native_send_transaction(input::SendTransactionRequest {
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
        .native_send_transaction(input::SendTransactionRequest {
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
        .native_send_transaction(request())
        .await
        .unwrap();
    assert_eq!(result.transaction_hash, hash());
    assert!(matches!(result.admission, out::Admission::Accepted));
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
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
            .native_send_transaction(request())
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
    let mut future = Box::pin(server.native_send_transaction(request()));
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
    .native_transaction_receipt(query())
    .await
    .unwrap_err();
    assert_eq!(error.code(), codes::BACKEND_DATA_INVALID);
    assert!(data(&error).get("observation").is_none());
    for (observed, kept) in [(hash(), true), (TxHash(Base58([9; 32].into())), false)] {
        let error = server(Backend {
            read_error: true,
            observation: Some(ReceiptObservation {
                transaction_hash: observed,
                block_height: 42.into(),
            }),
            ..Default::default()
        })
        .native_transaction_receipt(query())
        .await
        .unwrap_err();
        assert_eq!(data(&error).get("observation").is_some(), kept);
        assert!(data(&error).get("admission").is_none());
        if kept {
            assert_eq!(data(&error)["observation"]["blockHeight"], "42");
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
        .native_send_transaction(request())
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
        .native_send_transaction(request())
        .await
        .unwrap_err();
    assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
    assert_eq!(data(&error)["admission"], "notAttempted");
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert!(backend.attempt.lock().unwrap().is_none());
}
