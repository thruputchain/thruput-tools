use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use thruput_json_rpc_types::{
    BlockHeight,
    airdrop::ClientNonce,
    base58::Base58,
    block::ChainTip,
    hex::HexBytes,
    receipt::TransactionReceipt,
    transaction::{
        ConflictCode, ConflictDiagnostic, SchedulerTransactionErrorSource, TransactionStatus,
    },
};

use super::*;

struct CountingBuilder {
    constructions: AtomicUsize,
    bytes: Vec<u8>,
}

impl AirdropTransactionBuilder for CountingBuilder {
    fn build(
        &self,
        _recipient: ContractAddress,
        _block_expiry_height: i64,
        _raw_amount: u128,
    ) -> Result<SignedAirdrop, RpcServerError> {
        self.constructions.fetch_add(1, Ordering::SeqCst);
        Ok(SignedAirdrop {
            bytes: self.bytes.clone(),
            tx_hash: test_tx_hash(),
        })
    }
}

struct TestBackend {
    submit_results: Mutex<VecDeque<Result<(), RpcServerError>>>,
    status_results: Mutex<VecDeque<Result<TransactionStatus, RpcServerError>>>,
    submissions: Mutex<Vec<Vec<u8>>>,
    receipt: Mutex<Option<TransactionReceipt>>,
    submit_delay: Duration,
    submit_gate: Option<Arc<Semaphore>>,
    contexts: Mutex<Vec<RpcRequestContext>>,
}

impl TestBackend {
    fn new(submit_results: Vec<Result<(), RpcServerError>>) -> Self {
        Self {
            submit_results: Mutex::new(submit_results.into()),
            status_results: Mutex::new(VecDeque::new()),
            submissions: Mutex::new(Vec::new()),
            receipt: Mutex::new(None),
            submit_delay: Duration::ZERO,
            submit_gate: None,
            contexts: Mutex::new(Vec::new()),
        }
    }

    fn with_submit_delay(mut self, delay: Duration) -> Self {
        self.submit_delay = delay;
        self
    }

    fn with_status_results(
        mut self,
        status_results: Vec<Result<TransactionStatus, RpcServerError>>,
    ) -> Self {
        self.status_results = Mutex::new(status_results.into());
        self
    }
}

#[async_trait]
impl AirdropBackend for TestBackend {
    async fn chain_tip(&self, context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
        self.contexts.lock().unwrap().push(context);
        Ok(ChainTip {
            height: BlockHeight::from(7),
            block_hash: Base58(thruput_xdr::types::SHA256Hash::from([0x77; 32])),
        })
    }

    async fn submit(
        &self,
        _context: RpcRequestContext,
        signed: &SignedAirdrop,
    ) -> Result<(), RpcServerError> {
        self.submissions
            .lock()
            .expect("bug: submissions lock should be available")
            .push(signed.bytes.clone());
        if let Some(gate) = &self.submit_gate {
            gate.acquire().await.unwrap().forget();
        }
        if !self.submit_delay.is_zero() {
            tokio::time::sleep(self.submit_delay).await;
        }
        self.submit_results
            .lock()
            .expect("bug: submit results lock should be available")
            .pop_front()
            .unwrap_or(Ok(()))
    }

    async fn receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Ok(self.receipt.lock().unwrap().clone())
    }

    async fn status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        self.status_results
            .lock()
            .expect("bug: status results lock should be available")
            .pop_front()
            .unwrap_or(Ok(TransactionStatus::Pending))
    }
}

#[test]
fn native_builder_binds_signatures_to_network_hash() {
    let build = |network_byte| {
        NativeAirdropBuilder {
            network_hash: thruput_xdr::types::NetworkHash::from(
                thruput_xdr::types::SHA256Hash::from([network_byte; 32]),
            ),
            signer: Arc::new(LocalSigner::from_secret_key([0x33; 32])),
            next_nonce: AtomicU64::new(7),
        }
        .build(
            ContractAddress::from(thruput_xdr::types::ContractAddress::from([0x44; 32])),
            1_000,
            100_000_000,
        )
        .expect("bug: local airdrop should build")
    };

    let first = build(0x55);
    let second = build(0x66);

    assert_eq!(first.tx_hash, second.tx_hash);
    assert_ne!(first.bytes, second.bytes);
}

#[tokio::test]
async fn registered_http_small_array_waits_without_raising_execution_limit() {
    use thruput_json_rpc_server::batch::{ConcurrentBatchService, RpcBatchConcurrency};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    for concurrency in [1, RpcBatchConcurrency::default().get()] {
        let gate = Arc::new(Semaphore::new(0));
        let mut backend = TestBackend::new(vec![]);
        backend.submit_gate = Some(gate.clone());
        let backend = Arc::new(backend);
        let builder = Arc::new(CountingBuilder {
            constructions: AtomicUsize::new(0),
            bytes: vec![1, 2, 3],
        });
        let rpc = test_rpc(backend.clone(), builder.clone(), 64, 16);
        let middleware =
            jsonrpsee::core::middleware::RpcServiceBuilder::new().layer_fn(move |inner| {
                ConcurrentBatchService::new(inner, 100_000, concurrency.try_into().unwrap())
            });
        let server = jsonrpsee::server::Server::builder()
            .set_rpc_middleware(middleware)
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let address = server.local_addr().unwrap();
        let mut module = jsonrpsee::RpcModule::new(rpc.clone());
        module
            .register_async_method("faucet_requestAirdrop", |params, funding, _| async move {
                let request = thruput_json_rpc_server::params::parse_named(&params)?;
                thruput_json_rpc_server::native::funding::NativeFundingPlane::request_airdrop(
                    &*funding, request,
                )
                .await
            })
            .unwrap();
        let handle = server.start(module);
        let body = serde_json::to_string(
            &(1..=20)
                .map(|id| {
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id, "method": "faucet_requestAirdrop",
                        "params": test_request(id, id)
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let response = tokio::spawn(async move {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            socket.write_all(format!(
                    "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
                ).as_bytes()).await.unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            response
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while backend.submissions.lock().unwrap().len() < concurrency.min(16)
                || rpc.admission.available_permits() != 256 - concurrency.min(20)
            {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                backend.submissions.lock().unwrap().len(),
                concurrency.min(16)
            );
            assert_eq!(rpc.admission.available_permits(), 256 - concurrency.min(20));
            gate.add_permits(20);
            let response = response.await.unwrap();
            let responses: Vec<serde_json::Value> =
                serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(responses.len(), 20);
            for response in responses {
                assert!(response.get("error").is_none(), "{response}");
                assert_eq!(response["result"]["attemptStatus"], "pending");
            }
        })
        .await
        .unwrap();
        assert_eq!(builder.constructions.load(Ordering::SeqCst), 20);
        assert_eq!(backend.submissions.lock().unwrap().len(), 20);
        assert_eq!(rpc.concurrency.available_permits(), 16);
        assert_eq!(rpc.admission.available_permits(), 256);
        handle.stop().unwrap();
        handle.stopped().await;
    }
}

#[tokio::test]
async fn admission_bounds_same_key_waiters_and_recovers_after_cancellation() {
    let backend = Arc::new(TestBackend::new(vec![]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1],
    });
    let rpc = test_rpc(backend.clone(), builder, 64, 1);
    let execution = rpc.concurrency.acquire().await.unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let rpc = rpc.clone();
        tasks.spawn(async move { rpc.request_airdrop(test_request(1, 1)).await });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        // Admission precedes journal registration on a blocking worker.
        // Wait for both before checking the retained attempt or cancelling callers.
        while rpc.admission.available_permits() != 0 || rpc.attempts.lock().unwrap().len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let error = rpc.request_airdrop(test_request(2, 2)).await.unwrap_err();
    assert_eq!(
        error.code(),
        thruput_json_rpc_types::error::codes::OVERLOADED
    );
    assert_eq!(rpc.attempts.lock().unwrap().len(), 1);
    assert!(backend.submissions.lock().unwrap().is_empty());
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    assert_eq!(rpc.admission.available_permits(), 16);
    drop(execution);
    rpc.request_airdrop(test_request(1, 1)).await.unwrap();
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn deadline_covers_queue_and_submission_and_retains_signed_bytes() {
    let backend = Arc::new(TestBackend::new(vec![]).with_submit_delay(Duration::from_millis(50)));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![9, 8, 7],
    });
    let mut rpc = test_rpc(backend.clone(), builder.clone(), 2, 1);
    rpc.timeout = Duration::from_millis(10);
    let execution = rpc.concurrency.acquire().await.unwrap();
    assert!(matches!(
        rpc.request(test_request(1, 1)).await,
        Err(RpcServerError::BackendTimeout)
    ));
    assert_eq!(rpc.admission.available_permits(), 16);
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 0);
    drop(execution);
    // Both registered requests remain retained after timeout.
    assert!(matches!(
        rpc.request(test_request(2, 2)).await,
        Err(RpcServerError::BackendTimeout)
    ));
    assert_eq!(rpc.concurrency.available_permits(), 1);
    rpc.timeout = Duration::from_secs(1);
    rpc.request_airdrop(test_request(2, 2)).await.unwrap();
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(*backend.submissions.lock().unwrap(), vec![vec![9, 8, 7]; 2]);
}

#[tokio::test]
async fn execution_receives_the_deadline_started_before_queueing() {
    let backend = Arc::new(TestBackend::new(vec![]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1],
    });
    let rpc = test_rpc(backend.clone(), builder, 1, 1);
    let execution = rpc.concurrency.acquire().await.unwrap();
    let task = tokio::spawn({
        let rpc = rpc.clone();
        async move { rpc.request_airdrop(test_request(1, 1)).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while rpc.admission.available_permits() == 16 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let released_at = std::time::Instant::now();
    drop(execution);
    task.await.unwrap().unwrap();
    let contexts = backend.contexts.lock().unwrap();
    assert_eq!(contexts.len(), 1);
    assert!(contexts[0].deadline().unwrap() <= released_at + rpc.timeout);
    assert!(contexts[0].started_at() <= released_at);
}

#[test]
fn local_context_rejects_wrong_faucet_role() {
    let config = NodeSupervisorConfig::default();
    let mut manifest = runtime_manifest(&config);
    manifest.bootstrap_accounts[0].role = "local_user".into();

    let error = LocalAirdropContext::from_validated_runtime(
        &manifest,
        &config,
        fixture_signer(),
        fixture_journal(),
        None,
    )
    .err()
    .expect("wrong dev-0 role must fail validation");

    assert!(error.contains("missing its local faucet"));
}

#[test]
fn local_context_rejects_multiple_faucets() {
    let config = NodeSupervisorConfig::default();
    let mut manifest = runtime_manifest(&config);
    manifest
        .bootstrap_accounts
        .push(manifest.bootstrap_accounts[0].clone());
    let error = LocalAirdropContext::from_validated_runtime(
        &manifest,
        &config,
        fixture_signer(),
        fixture_journal(),
        None,
    )
    .err()
    .unwrap();
    assert!(error.contains("multiple local faucets"));
}

#[test]
fn local_context_rejects_wrong_faucet_public_key() {
    let config = NodeSupervisorConfig::default();
    let mut manifest = runtime_manifest(&config);
    manifest.bootstrap_accounts[0].public_key =
        Base58(thruput_xdr::types::Ed25519PublicKey::from([0x99; 32]));

    let error = LocalAirdropContext::from_validated_runtime(
        &manifest,
        &config,
        fixture_signer(),
        fixture_journal(),
        None,
    )
    .err()
    .expect("wrong faucet public key must fail validation");

    assert!(error.contains("public key does not match"));
}

#[test]
fn local_context_rejects_non_native_faucet_address() {
    let config = NodeSupervisorConfig::default();
    let mut manifest = runtime_manifest(&config);
    manifest.bootstrap_accounts[0].wallet_address = Base58([0; 32].into());

    let error = LocalAirdropContext::from_validated_runtime(
        &manifest,
        &config,
        fixture_signer(),
        fixture_journal(),
        None,
    )
    .err()
    .expect("non-native faucet address must fail validation");

    assert!(error.contains("address is not its nonce-zero native account"));
}

#[tokio::test]
async fn concurrent_same_key_constructs_and_submits_once() {
    let backend =
        Arc::new(TestBackend::new(vec![Ok(())]).with_submit_delay(Duration::from_millis(25)));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1, 2, 3],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(1, 1);

    let (a, b, c, d) = tokio::join!(
        rpc.request(request.clone()),
        rpc.request(request.clone()),
        rpc.request(request.clone()),
        rpc.request(request),
    );

    for result in [a, b, c, d] {
        assert!(matches!(result, Ok(AirdropAttemptResponse::Pending { .. })));
    }
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available")
            .len(),
        1
    );
}

#[tokio::test]
async fn ambiguous_retry_resubmits_exact_signed_bytes_without_reconstruction() {
    let backend = Arc::new(
        TestBackend::new(vec![
            Err(RpcServerError::BackendUnavailable("response lost".into())),
            Ok(()),
        ])
        .with_status_results(vec![Err(RpcServerError::BackendUnavailable(
            "status unavailable".into(),
        ))]),
    );
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![9, 8, 7, 6],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(2, 2);

    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request).await;

    assert!(matches!(first, Err(RpcServerError::BackendUnavailable(_))));
    assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        *backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available"),
        vec![vec![9, 8, 7, 6], vec![9, 8, 7, 6]]
    );
}

#[tokio::test]
async fn deterministic_submit_rejection_is_terminal_without_status_reconciliation() {
    let backend = Arc::new(
        TestBackend::new(vec![Err(RpcServerError::TxRejected("expired".into()))])
            .with_status_results(vec![Err(RpcServerError::BackendUnavailable(
                "status unavailable".into(),
            ))]),
    );
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![2, 4, 6],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(12, 12);

    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request).await;

    assert!(matches!(first, Err(RpcServerError::TxRejected(_))));
    assert!(matches!(second, Err(RpcServerError::TxRejected(_))));
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available")
            .len(),
        1
    );
}

#[tokio::test]
async fn accepted_pending_retry_polls_without_resubmitting() {
    let backend = Arc::new(TestBackend::new(vec![Ok(())]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![3, 2, 1],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(7, 7);

    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request).await;

    assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
    assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available")
            .len(),
        1
    );
}

#[tokio::test]
async fn accepted_unknown_retry_resubmits_exact_signed_bytes_without_reconstruction() {
    let backend = Arc::new(
        TestBackend::new(vec![Ok(()), Ok(())]).with_status_results(vec![
            Ok(TransactionStatus::Unknown),
            Ok(TransactionStatus::Pending),
        ]),
    );
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![4, 3, 2, 1],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(13, 13);

    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request).await;

    assert!(matches!(
        first,
        Ok(AirdropAttemptResponse::Pending {
            tx_hash: Some(hash),
            ..
        }) if hash == test_tx_hash()
    ));
    assert!(matches!(
        second,
        Ok(AirdropAttemptResponse::Pending {
            tx_hash: Some(hash),
            ..
        }) if hash == test_tx_hash()
    ));
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        *backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available"),
        vec![vec![4, 3, 2, 1], vec![4, 3, 2, 1]]
    );
}

#[tokio::test]
async fn unknown_retry_with_ambiguous_submit_error_can_retry_again() {
    let backend = Arc::new(
        TestBackend::new(vec![
            Ok(()),
            Err(RpcServerError::BackendUnavailable("response lost".into())),
            Ok(()),
        ])
        .with_status_results(vec![
            Ok(TransactionStatus::Unknown),
            Ok(TransactionStatus::Unknown),
            Ok(TransactionStatus::Pending),
        ]),
    );
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![6, 7, 8, 9],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(14, 14);

    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request.clone()).await;
    let third = rpc.request(request).await;

    assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
    assert!(matches!(second, Err(RpcServerError::BackendUnavailable(_))));
    assert!(matches!(third, Ok(AirdropAttemptResponse::Pending { .. })));
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        *backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available"),
        vec![vec![6, 7, 8, 9]; 3]
    );
}

#[tokio::test]
async fn recent_failure_retry_resubmits_exact_signed_bytes() {
    let backend = Arc::new(
        TestBackend::new(vec![Ok(()), Ok(())]).with_status_results(vec![Ok(
            TransactionStatus::RecentFailure {
                attempt_height: BlockHeight::from(8),
                cause: thruput_json_rpc_types::transaction::TransactionFailureCause::DbConflict {
                    conflict: ConflictDiagnostic {
                        address: Base58(thruput_xdr::types::ContractAddress::from([0x44; 32])),
                        key: HexBytes(vec![0x01]),
                        code: ConflictCode::BoundsExceeded,
                    },
                },
            },
        )]),
    );
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![5, 4, 3],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(10, 10);

    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request).await;

    assert!(matches!(first, Err(RpcServerError::BackendUnavailable(_))));
    assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        *backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available"),
        vec![vec![5, 4, 3], vec![5, 4, 3]]
    );
}

fn scheduler_failure() -> SchedulerTransactionError {
    SchedulerTransactionError::new(SchedulerTransactionErrorSource::Executor, -7)
}

#[tokio::test]
async fn retry_outcome_resubmits_same_airdrop() {
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(
        TransactionStatus::Retry {
            error: scheduler_failure(),
        },
    )]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![5, 4, 3],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(10, 10);
    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request).await;

    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
    assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
    assert_eq!(*backend.submissions.lock().unwrap(), vec![vec![5, 4, 3]; 2]);
}

#[tokio::test]
async fn dropped_airdrop_is_terminal() {
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(
        TransactionStatus::Dropped {
            error: scheduler_failure(),
        },
    )]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![5, 4, 3],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(10, 10);
    let first = rpc.request(request.clone()).await;
    let second = rpc.request(request).await;

    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert!(
        matches!(first, Err(RpcServerError::TxRejected(ref reason)) if reason.contains("Executor code -7"))
    );
    assert_eq!(
        first.unwrap_err().to_string(),
        second.unwrap_err().to_string()
    );
    assert_eq!(*backend.submissions.lock().unwrap(), vec![vec![5, 4, 3]]);
}

#[tokio::test]
async fn registered_unsigned_attempt_consumes_capacity_and_can_resume() {
    let backend = Arc::new(TestBackend::new(vec![Ok(())]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1, 2, 3],
    });
    let rpc = test_rpc(backend, builder, 1, 1);
    let first = test_request(1, 1);
    rpc.find_or_admit(FundAccountRequest {
        recipient: first.recipient,
        client_nonce: first.client_nonce.clone(),
        raw_amount: rpc.raw_amount.to_string(),
        faucet: rpc.faucet,
    })
    .await
    .unwrap();
    assert!(matches!(
        rpc.request(test_request(2, 2)).await,
        Err(RpcServerError::Overloaded(_))
    ));
    assert!(rpc.request(first).await.is_ok());
}

#[tokio::test]
async fn terminal_attempt_remains_retained_at_capacity() {
    let backend = Arc::new(TestBackend::new(vec![Err(RpcServerError::TxRejected(
        "rejected".into(),
    ))]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1, 2, 3],
    });
    let rpc = test_rpc(backend.clone(), builder, 1, 1);
    let first = test_request(1, 1);
    assert!(matches!(
        rpc.request(first.clone()).await,
        Err(RpcServerError::TxRejected(_))
    ));
    assert!(matches!(
        rpc.request(test_request(2, 2)).await,
        Err(RpcServerError::Overloaded(_))
    ));
    assert!(matches!(
        rpc.request(first).await,
        Err(RpcServerError::TxRejected(_))
    ));
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ambiguous_submission_confirmed_pending_wakes_block_driver() {
    let backend = Arc::new(
        TestBackend::new(vec![Err(RpcServerError::BackendUnavailable(
            "response lost".into(),
        ))])
        .with_status_results(vec![Ok(TransactionStatus::Pending)]),
    );
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![4, 3, 2, 1],
    });
    let notify = Arc::new(tokio::sync::Notify::new());
    let rpc = test_rpc_with_trigger(
        backend.clone(),
        builder,
        8,
        4,
        BlockDriverTrigger::new(notify.clone()),
    );

    let result = rpc.request(test_request(9, 9)).await;

    assert!(matches!(result, Ok(AirdropAttemptResponse::Pending { .. })));
    tokio::time::timeout(Duration::from_millis(10), notify.notified())
        .await
        .expect("pending reconciliation should wake instant block production");
    assert_eq!(
        backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available")
            .len(),
        1
    );
}

#[tokio::test]
async fn cancelled_submission_retry_reuses_exact_signed_bytes() {
    let backend =
        Arc::new(TestBackend::new(vec![Ok(())]).with_submit_delay(Duration::from_millis(100)));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![6, 5, 4],
    });
    let rpc = test_rpc(backend.clone(), builder.clone(), 8, 4);
    let request = test_request(8, 8);
    let task = tokio::spawn({
        let rpc = rpc.clone();
        let request = request.clone();
        async move { rpc.request(request).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let _ = task.await;

    let retry = rpc.request(request).await;

    assert!(matches!(retry, Ok(AirdropAttemptResponse::Pending { .. })));
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 1);
    assert_eq!(
        *backend
            .submissions
            .lock()
            .expect("bug: submissions lock should be available"),
        vec![vec![6, 5, 4], vec![6, 5, 4]]
    );
}

#[tokio::test]
async fn unresolved_attempt_prevents_capacity_eviction() {
    let backend = Arc::new(TestBackend::new(vec![Err(
        RpcServerError::BackendUnavailable("response lost".into()),
    )]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![4, 5, 6],
    });
    let rpc = test_rpc(backend, builder, 1, 1);

    let first = rpc.request(test_request(3, 3)).await;
    let second = rpc.request(test_request(4, 4)).await;

    assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
    assert!(matches!(second, Err(RpcServerError::Overloaded(_))));
}

fn funding_request(raw_amount: &str) -> FundAccountRequest {
    let request = test_request(0x44, 9);
    FundAccountRequest {
        recipient: request.recipient,
        client_nonce: request.client_nonce,
        raw_amount: raw_amount.into(),
        faucet: Base58(
            PubkeyAccount::from_public_key_bytes(fixture_signer().verifying_key_bytes(), 0)
                .address(),
        ),
    }
}

fn durable_rpc(dir: &std::path::Path, fresh: bool, backend: Arc<TestBackend>) -> LocalAirdropRpc {
    let signer = fixture_signer();
    let faucet = funding_request("42").faucet;
    let journal = FundingJournal::open(dir, None, faucet, fresh, &signer).unwrap();
    let mut rpc = LocalAirdropRpc::with_parts(
        backend,
        Arc::new(NativeAirdropBuilder {
            network_hash: thruput_json_rpc_types::NetworkHashText::from([0; 32]).to_network_hash(),
            signer,
            next_nonce: AtomicU64::new(42),
        }),
        LocalAirdropConfig {
            attempt_capacity: 1,
            ..LocalAirdropConfig::default()
        },
        BlockDriverTrigger::noop(),
    );
    rpc.journal = Arc::new(std::sync::Mutex::new(journal));
    rpc.faucet = faucet;
    rpc
}

#[tokio::test]
async fn local_funding_admits_explicit_amount_before_production_and_retains_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::new(vec![Ok(())]));
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    let request = funding_request("123456");
    let response = rpc.fund(request.clone()).await.unwrap();
    assert_eq!(response.raw_amount, "123456");
    assert_eq!(
        backend.submissions.lock().unwrap().as_slice(),
        std::slice::from_ref(&response.signed_transaction.0)
    );
    // No receipt exists yet: admission preserves submit -> produce -> receipt.
    drop(rpc);
    let restarted_backend = Arc::new(TestBackend::new(vec![]));
    let restarted = durable_rpc(dir.path(), false, restarted_backend.clone());
    assert_eq!(restarted.fund(request.clone()).await.unwrap(), response);
    assert!(restarted_backend.submissions.lock().unwrap().is_empty());
    let mut conflict = request.clone();
    conflict.raw_amount = "123457".into();
    assert!(matches!(
        restarted.fund(conflict).await,
        Err(RpcServerError::InvalidParam(_))
    ));
    let mut wrong_chain = request.clone();
    wrong_chain.faucet = Base58([0; 32].into());
    assert!(matches!(
        restarted.fund(wrong_chain).await,
        Err(RpcServerError::InvalidParam(_))
    ));
    let mut another = request;
    another.client_nonce = ClientNonce::new(vec![10]).unwrap();
    assert!(matches!(
        restarted.fund(another).await,
        Err(RpcServerError::Overloaded(_))
    ));
}

#[tokio::test]
async fn lost_admission_response_reuses_exact_retained_bytes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(
        TestBackend::new(vec![Err(RpcServerError::BackendUnavailable(
            "lost response".into(),
        ))])
        .with_status_results(vec![Ok(TransactionStatus::Unknown)]),
    );
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    let request = funding_request("42");
    assert!(matches!(
        rpc.fund(request.clone()).await,
        Err(RpcServerError::LocalFundingUnresolved { .. })
    ));
    let retained = backend.submissions.lock().unwrap()[0].clone();
    drop(rpc);
    let restarted_backend = Arc::new(
        TestBackend::new(vec![Ok(())]).with_status_results(vec![Ok(TransactionStatus::Unknown)]),
    );
    let restarted = durable_rpc(dir.path(), false, restarted_backend.clone());
    let response = restarted.fund(request).await.unwrap();
    assert_eq!(response.signed_transaction.0, retained);
    assert_eq!(restarted_backend.submissions.lock().unwrap()[0], retained);
}

#[tokio::test]
async fn retained_funding_retry_reuses_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let rpc = durable_rpc(dir.path(), true, Arc::new(TestBackend::new(vec![])));
    let request = funding_request("42");
    let original = rpc.fund(request.clone()).await.unwrap();
    drop(rpc);
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(
        TransactionStatus::Retry {
            error: scheduler_failure(),
        },
    )]));
    let restarted = durable_rpc(dir.path(), false, backend.clone());
    let result = restarted.fund(request).await;

    assert_eq!(result.unwrap(), original);
    assert_eq!(
        *backend.submissions.lock().unwrap(),
        vec![original.signed_transaction.0]
    );
}

#[tokio::test]
async fn retained_dropped_funding_never_resubmits() {
    let dir = tempfile::tempdir().unwrap();
    let rpc = durable_rpc(dir.path(), true, Arc::new(TestBackend::new(vec![])));
    let request = funding_request("42");
    let original = rpc.fund(request.clone()).await.unwrap();
    drop(rpc);
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(
        TransactionStatus::Dropped {
            error: scheduler_failure(),
        },
    )]));
    let restarted = durable_rpc(dir.path(), false, backend.clone());
    let error = restarted.fund(request).await.unwrap_err();

    assert!(error.to_string().contains("Executor code -7"));
    let object: jsonrpsee::types::ErrorObjectOwned = error.into();
    let evidence: LocalFundingErrorData =
        serde_json::from_str(object.data().unwrap().get()).unwrap();
    assert_eq!(evidence.tx_hash, original.tx_hash);
    assert_eq!(evidence.signed_transaction, original.signed_transaction);
    assert!(backend.submissions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn restored_airdrop_retry_reuses_same_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let original_backend = Arc::new(TestBackend::new(vec![]));
    let rpc = durable_rpc(dir.path(), true, original_backend.clone());
    let request = test_request(0x44, 9);
    assert!(matches!(
        rpc.request(request.clone()).await,
        Ok(AirdropAttemptResponse::Pending { .. })
    ));
    let bytes = original_backend.submissions.lock().unwrap()[0].clone();
    drop(rpc);
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(
        TransactionStatus::Retry {
            error: scheduler_failure(),
        },
    )]));
    let restarted = durable_rpc(dir.path(), false, backend.clone());
    for _ in 0..2 {
        let result = restarted.request(request.clone()).await;
        assert!(matches!(result, Ok(AirdropAttemptResponse::Pending { .. })));
    }
    assert_eq!(*backend.submissions.lock().unwrap(), vec![bytes]);
}

#[tokio::test]
async fn restored_dropped_airdrop_is_terminal_without_resubmitting() {
    let dir = tempfile::tempdir().unwrap();
    let rpc = durable_rpc(dir.path(), true, Arc::new(TestBackend::new(vec![])));
    let request = test_request(0x44, 9);
    assert!(matches!(
        rpc.request(request.clone()).await,
        Ok(AirdropAttemptResponse::Pending { .. })
    ));
    drop(rpc);
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(
        TransactionStatus::Dropped {
            error: scheduler_failure(),
        },
    )]));
    let restarted = durable_rpc(dir.path(), false, backend.clone());
    for _ in 0..2 {
        let result = restarted.request(request.clone()).await;
        assert!(
            matches!(result, Err(RpcServerError::TxRejected(ref reason)) if reason.contains("Executor code -7"))
        );
    }
    assert!(backend.submissions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn restored_airdrop_repeated_retry_resubmits_after_accepted_replay() {
    let dir = tempfile::tempdir().unwrap();
    let original_backend = Arc::new(TestBackend::new(vec![]));
    let rpc = durable_rpc(dir.path(), true, original_backend.clone());
    let request = test_request(0x44, 9);
    assert!(matches!(
        rpc.request(request.clone()).await,
        Ok(AirdropAttemptResponse::Pending { .. })
    ));
    let bytes = original_backend.submissions.lock().unwrap()[0].clone();
    drop(rpc);
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![
        Ok(TransactionStatus::Retry {
            error: scheduler_failure(),
        }),
        Ok(TransactionStatus::Pending),
        Ok(TransactionStatus::Retry {
            error: scheduler_failure(),
        }),
        Ok(TransactionStatus::Retry {
            error: scheduler_failure(),
        }),
        Ok(TransactionStatus::Retry {
            error: scheduler_failure(),
        }),
        Ok(TransactionStatus::Pending),
    ]));
    let restarted = durable_rpc(dir.path(), false, backend.clone());
    let first = restarted.request(request.clone()).await;
    assert!(matches!(first, Ok(AirdropAttemptResponse::Pending { .. })));
    let attempt = restarted
        .attempts
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(attempt.restored);
    assert!(attempt.is_submitted().unwrap());
    assert_eq!(*backend.submissions.lock().unwrap(), vec![bytes.clone()]);

    let second = restarted.request(request.clone()).await;
    assert!(matches!(second, Ok(AirdropAttemptResponse::Pending { .. })));
    assert!(!attempt.is_submitted().unwrap());
    assert_eq!(*backend.submissions.lock().unwrap(), vec![bytes.clone()]);

    let third = restarted.request(request).await;
    assert!(matches!(third, Ok(AirdropAttemptResponse::Pending { .. })));
    assert!(attempt.is_submitted().unwrap());
    assert_eq!(*backend.submissions.lock().unwrap(), vec![bytes; 2]);
    assert!(backend.status_results.lock().unwrap().is_empty());
}

#[tokio::test]
async fn committed_receipt_wins_over_retained_dropped_funding_status() {
    let dir = tempfile::tempdir().unwrap();
    let rpc = durable_rpc(dir.path(), true, Arc::new(TestBackend::new(vec![])));
    let request = funding_request(&rpc.raw_amount.to_string());
    let original = rpc.fund(request.clone()).await.unwrap();
    drop(rpc);
    let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(
        TransactionStatus::Dropped {
            error: scheduler_failure(),
        },
    )]));
    *backend.receipt.lock().unwrap() = Some(TransactionReceipt {
        tx_hash: original.tx_hash,
        height: BlockHeight::from(9),
        status: ExecStatus::Ok,
        events: vec![],
        return_data: Some(HexBytes(vec![])),
        abort_data: None,
        gas_used: 1,
    });
    let restarted = durable_rpc(dir.path(), false, backend.clone());
    assert_eq!(restarted.fund(request.clone()).await.unwrap(), original);
    let resumed_request = AirdropRequest {
        recipient: request.recipient,
        client_nonce: request.client_nonce,
    };
    let result = restarted.request(resumed_request).await;
    assert!(matches!(result, Ok(AirdropAttemptResponse::Succeeded { height, .. }) if *height == 9));
    assert!(backend.submissions.lock().unwrap().is_empty());
    assert_eq!(backend.status_results.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn expired_retained_funding_without_outcome_never_creates_another_transfer() {
    for status in [
        TransactionStatus::Unknown,
        TransactionStatus::Retry {
            error: scheduler_failure(),
        },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let rpc = durable_rpc(dir.path(), true, Arc::new(TestBackend::new(vec![])));
        let request = funding_request("42");
        let signed = NativeAirdropBuilder {
            network_hash: thruput_json_rpc_types::NetworkHashText::from([0; 32]).to_network_hash(),
            signer: fixture_signer(),
            next_nonce: AtomicU64::new(7),
        }
        .build(request.recipient, 6, 42)
        .unwrap();
        {
            let mut journal = rpc.journal.lock().unwrap();
            journal.register(request.clone()).unwrap();
            journal
                .retain_signed(
                    &request,
                    RetainedFundingTransaction {
                        tx_hash: signed.tx_hash,
                        signed_transaction: thruput_json_rpc_types::hex::HexBytes(signed.bytes),
                    },
                )
                .unwrap();
        }
        drop(rpc);
        let backend = Arc::new(TestBackend::new(vec![]).with_status_results(vec![Ok(status)]));
        let restarted = durable_rpc(dir.path(), false, backend.clone());
        let error = restarted.fund(request).await.unwrap_err();
        assert!(error.to_string().contains("expired"));
        let object: jsonrpsee::types::ErrorObjectOwned = error.into();
        assert_eq!(
            object.code(),
            thruput_json_rpc_types::error::codes::LOCAL_FUNDING_UNRESOLVED
        );
        let evidence: LocalFundingErrorData =
            serde_json::from_str(object.data().unwrap().get()).unwrap();
        assert_eq!(evidence.tx_hash, signed.tx_hash);
        let original = FundingJournal::open(
            dir.path(),
            None,
            funding_request("42").faucet,
            false,
            &fixture_signer(),
        )
        .unwrap();
        assert_eq!(
            evidence.signed_transaction,
            original.records()[0]
                .signed
                .as_ref()
                .unwrap()
                .signed_transaction
        );
        assert!(backend.submissions.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn corrupt_retained_intent_fails_startup_and_failed_persistence_never_broadcasts() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::new(vec![]));
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    let request = funding_request("42");
    rpc.fund(request.clone()).await.unwrap();
    drop(rpc);
    let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["records"][0]["request"]["rawAmount"] = serde_json::json!("43");
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(
        FundingJournal::open(dir.path(), None, request.faucet, false, &fixture_signer()).is_err()
    );
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::new(vec![]));
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    // A directory at the journal path makes its atomic replacement fail.
    let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(rpc.fund(request).await.is_err());
    assert!(backend.submissions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_signed_persistence_does_not_expose_restart_unstable_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::new(vec![]));
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    let request = funding_request("42");
    rpc.find_or_admit(request.clone()).await.unwrap();
    let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let error = rpc.fund(request.clone()).await.unwrap_err();
    assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
    let journal = rpc.journal.lock().unwrap();
    assert!(journal.records()[0].signed.is_some());
    assert!(journal.durable_signed(&request).is_none());
    assert!(backend.submissions.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_persistence_respects_deadline_without_broadcast_or_runtime_stall() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::new(vec![]));
    let mut rpc = durable_rpc(dir.path(), true, backend.clone());
    rpc.timeout = Duration::from_millis(20);
    let journal = rpc.journal.clone();
    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let holder = tokio::task::spawn_blocking(move || {
        let _held = journal.lock().unwrap();
        ready_sender.send(()).unwrap();
        release_receiver.recv().unwrap();
    });
    ready_receiver.await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), rpc.fund(funding_request("42")))
        .await
        .unwrap();
    assert!(matches!(response, Err(RpcServerError::BackendTimeout)));
    assert!(backend.submissions.lock().unwrap().is_empty());
    release_sender.send(()).unwrap();
    holder.await.unwrap();
    // Cancellation leaves only registered intent; retry can advance it safely.
    rpc.timeout = Duration::from_secs(1);
    assert!(rpc.fund(funding_request("42")).await.is_ok());
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unchanged_durable_retry_does_not_require_another_journal_write() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::new(vec![]));
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    let request = funding_request("42");
    let admitted = rpc.fund(request.clone()).await.unwrap();
    let path = dir.path().join(crate::LOCAL_FUNDING_ATTEMPTS_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(rpc.fund(request).await.unwrap(), admitted);
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_persistence_holds_chain_lock_and_bounds_workers_until_completion() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::HogdDevConfig::default().with_data_dir(dir.path());
    let state = crate::local_state::LocalChainState::prepare(&config).unwrap();
    let intent = config.to_bootstrap_manifest([0; 32].into(), state.public_key());
    std::fs::write(dir.path().join("hog-devnode-data.json"), serde_json::to_vec(&serde_json::json!({
            "schemaVersion":5, "kind":"hog-devnode-data", "bootstrapConfig": {
                "bootstrapFaucet": intent.bootstrap_faucet, "bootstrapAccounts": intent.bootstrap_accounts
            }
        })).unwrap()).unwrap();
    let backend = Arc::new(TestBackend::new(vec![]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1],
    });
    let mut rpc = test_rpc(backend.clone(), builder, 8, 1);
    rpc.journal = state.journal.clone();
    rpc.lock_lease = state.lock.clone();
    rpc.faucet = Base58(
        PubkeyAccount::from_public_key_bytes(state.signer.verifying_key_bytes(), 0).address(),
    );
    rpc.timeout = Duration::from_millis(20);
    let request = FundAccountRequest {
        faucet: rpc.faucet,
        ..funding_request("42")
    };
    let journal = rpc.journal.clone();
    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let holder = tokio::task::spawn_blocking(move || {
        let _held = journal.lock().unwrap();
        ready_sender.send(()).unwrap();
        release_receiver.recv().unwrap();
    });
    ready_receiver.await.unwrap();
    for _ in 0..3 {
        assert!(matches!(
            rpc.fund(request.clone()).await,
            Err(RpcServerError::BackendTimeout)
        ));
        assert_eq!(
            rpc.persistence.available_permits(),
            0,
            "only one detached worker is admitted"
        );
    }
    assert!(backend.submissions.lock().unwrap().is_empty());
    // Simulate shutdown dropping both owners while the cancelled worker remains.
    drop(state);
    drop(rpc);
    assert!(crate::local_state::LocalChainState::prepare(&config).is_err());
    release_sender.send(()).unwrap();
    holder.await.unwrap();
    let recovered = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(state) = crate::local_state::LocalChainState::prepare(&config) {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(recovered.journal.lock().unwrap().records().len(), 1);
}

fn test_rpc(
    backend: Arc<TestBackend>,
    builder: Arc<CountingBuilder>,
    capacity: usize,
    concurrency: usize,
) -> LocalAirdropRpc {
    test_rpc_with_trigger(
        backend,
        builder,
        capacity,
        concurrency,
        BlockDriverTrigger::noop(),
    )
}

fn test_rpc_with_trigger(
    backend: Arc<TestBackend>,
    builder: Arc<CountingBuilder>,
    capacity: usize,
    concurrency: usize,
    block_driver: BlockDriverTrigger,
) -> LocalAirdropRpc {
    LocalAirdropRpc::with_parts(
        backend,
        builder,
        LocalAirdropConfig {
            raw_amount: crate::config::DEFAULT_LOCAL_AIRDROP_RAW_AMOUNT,
            attempt_capacity: capacity,
            max_concurrency: concurrency,
            request_timeout: Duration::from_secs(30),
        },
        block_driver,
    )
}

fn test_request(recipient: u8, nonce: u8) -> AirdropRequest {
    AirdropRequest {
        recipient: Base58(thruput_xdr::types::ContractAddress::from([recipient; 32])),
        client_nonce: ClientNonce::new(vec![nonce]).expect("bug: one-byte nonce should validate"),
    }
}

fn test_tx_hash() -> TxHash {
    TxHash(Base58(thruput_xdr::types::SHA256Hash::from([0xaa; 32])))
}

fn runtime_manifest(_config: &NodeSupervisorConfig) -> RuntimeManifest {
    let signer = fixture_signer();
    serde_json::from_value(serde_json::json!({
            "schemaVersion": 5,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "1".repeat(32),
            "protocol": "hogp/test00000000000",
            "genesisHash": thruput_json_rpc_types::NetworkHashText::from([0x42; 32]),
            "producer": { "binary": "hogd-dev", "version": "0.1.0" },
            "endpoints": { "scheduler": "127.0.0.1:1000" },
            "bootstrapAccounts": [{
                "role": "faucet",
                "publicKey": Base58(thruput_xdr::types::Ed25519PublicKey::from(signer.verifying_key_bytes())),
                "walletAddress": Base58(PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address()),
                "initialStorageTokens": "100000000",
            }],
            "bootstrap": {
                "storageCostModel": {
                    "hash": thruput_json_rpc_types::NetworkHashText::from([0x53; 32]),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234",
                },
            },
        }))
        .expect("bug: runtime manifest fixture should deserialize")
}
fn fixture_signer() -> Arc<LocalSigner> {
    Arc::new(LocalSigner::from_secret_key([0x33; 32]))
}
fn fixture_journal() -> Arc<std::sync::Mutex<FundingJournal>> {
    Arc::new(std::sync::Mutex::new(FundingJournal::memory(
        [0; 32].into(),
        Base58(
            PubkeyAccount::from_public_key_bytes(fixture_signer().verifying_key_bytes(), 0)
                .address(),
        ),
    )))
}

fn assert_known_native_funding_error(method: &str, error: &jsonrpsee::types::ErrorObjectOwned) {
    assert!(
        matches!(
            thruput_json_rpc_types::error::native::DecodedError::decode(
                method,
                serde_json::to_value(error).unwrap()
            ),
            thruput_json_rpc_types::error::native::DecodedError::Known { .. }
        ),
        "unclassified {method} error: {error}"
    );
}

#[tokio::test]
async fn native_faucet_returns_pending_hash_after_one_submission() {
    use thruput_json_rpc_server::native::funding::NativeFundingPlane;
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::new(vec![]));
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    let request = test_request(0x44, 9);
    let result = NativeFundingPlane::request_airdrop(
        &rpc,
        thruput_json_rpc_types::requests::AirdropRequest {
            recipient: request.recipient,
            client_nonce: request.client_nonce,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        thruput_json_rpc_types::responses::AirdropAttempt::Pending {
            transaction_hash: Some(_),
            ..
        }
    ));
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_local_unresolved_retains_signed_bytes_and_stopped_cause() {
    use thruput_json_rpc_server::native::funding::NativeFundingPlane;
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(
        TestBackend::new(vec![Err(RpcServerError::TxRejected("expired".into()))])
            .with_status_results(vec![Ok(TransactionStatus::Unknown)]),
    );
    let rpc = durable_rpc(dir.path(), true, backend.clone());
    let request = funding_request("7");
    let error = NativeFundingPlane::fund_account(
        &rpc,
        thruput_json_rpc_types::requests::FundAccountRequest {
            recipient: request.recipient,
            raw_amount: 7,
            client_nonce: request.client_nonce,
            faucet: request.faucet,
        },
    )
    .await
    .unwrap_err();
    assert_known_native_funding_error("dev_fundAccount", &error);
    let data: serde_json::Value = serde_json::from_str(error.data().unwrap().get()).unwrap();
    assert_eq!(data["kind"], "localFundingUnresolved");
    assert_eq!(data["cause"]["kind"], "fundingAttemptStopped");
    assert_eq!(data["transactionHash"], data["cause"]["transactionHash"]);
    assert_eq!(
        data["signedTransaction"],
        data["cause"]["signedTransaction"]
    );
    assert_eq!(backend.submissions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_local_success_requires_matching_receipt_and_preserves_output() {
    use thruput_json_rpc_server::native::funding::NativeFundingPlane;
    for wrong_hash in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request(&rpc.raw_amount.to_string());
        let original = rpc.fund(request.clone()).await.unwrap();
        *backend.receipt.lock().unwrap() = Some(TransactionReceipt {
            tx_hash: if wrong_hash {
                test_tx_hash()
            } else {
                original.tx_hash
            },
            height: 9.into(),
            status: ExecStatus::Ok,
            events: vec![],
            return_data: Some(HexBytes(vec![1, 2])),
            abort_data: None,
            gas_used: 1,
        });
        let result = NativeFundingPlane::request_airdrop(
            &rpc,
            thruput_json_rpc_types::requests::AirdropRequest {
                recipient: request.recipient,
                client_nonce: request.client_nonce,
            },
        )
        .await;
        if wrong_hash {
            let error = result.unwrap_err();
            assert_known_native_funding_error("faucet_requestAirdrop", &error);
            assert_eq!(
                error.code(),
                thruput_json_rpc_types::error::native::codes::BACKEND_DATA_INVALID
            );
        } else {
            let result = serde_json::to_value(result.unwrap()).unwrap();
            assert_eq!(result["execution"]["returnData"], "0x0102");
            assert_eq!(
                result["transactionHash"],
                serde_json::to_value(original.tx_hash).unwrap()
            );
        }
        assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn native_faucet_errors_keep_method_contract_and_omit_lossy_receipts() {
    use thruput_json_rpc_server::native::funding::NativeFundingPlane;
    for failed_receipt in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(
            TestBackend::new(vec![]).with_status_results(vec![Err(RpcServerError::BackendTimeout)]),
        );
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request(&rpc.raw_amount.to_string());
        let original = rpc.fund(request.clone()).await.unwrap();
        if failed_receipt {
            *backend.receipt.lock().unwrap() = Some(TransactionReceipt {
                tx_hash: original.tx_hash,
                height: 9.into(),
                status: ExecStatus::Reverted { message: None },
                events: vec![],
                return_data: None,
                abort_data: Some(HexBytes(vec![3, 4])),
                gas_used: 17,
            });
        }
        let error = NativeFundingPlane::request_airdrop(
            &rpc,
            thruput_json_rpc_types::requests::AirdropRequest {
                recipient: request.recipient,
                client_nonce: request.client_nonce,
            },
        )
        .await
        .unwrap_err();
        assert_known_native_funding_error("faucet_requestAirdrop", &error);
        let data: serde_json::Value = serde_json::from_str(error.data().unwrap().get()).unwrap();
        assert_eq!(
            data["kind"],
            if failed_receipt {
                "fundingAttemptStopped"
            } else {
                "backendTimeout"
            }
        );
        if failed_receipt {
            assert_eq!(
                data["transactionHash"],
                serde_json::to_value(original.tx_hash).unwrap()
            );
            assert_eq!(
                data["signedTransaction"],
                serde_json::to_value(original.signed_transaction).unwrap()
            );
            assert!(
                data.get("receipt").is_none(),
                "legacy events may be incomplete"
            );
        } else {
            assert!(data.get("transactionHash").is_none());
            assert!(data.get("signedTransaction").is_none());
        }
        assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn native_faucet_does_not_cache_a_contradictory_execution_as_terminal() {
    use thruput_json_rpc_server::native::funding::NativeFundingPlane;
    for (status, return_data, abort_data) in [
        (ExecStatus::Ok, None, None),
        (ExecStatus::Reverted { message: None }, None, None),
        (
            ExecStatus::Reverted { message: None },
            Some(HexBytes(vec![1])),
            None,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(TestBackend::new(vec![]));
        let rpc = durable_rpc(dir.path(), true, backend.clone());
        let request = funding_request(&rpc.raw_amount.to_string());
        let original = rpc.fund(request.clone()).await.unwrap();
        *backend.receipt.lock().unwrap() = Some(TransactionReceipt {
            tx_hash: original.tx_hash,
            height: 9.into(),
            status,
            events: vec![],
            return_data,
            abort_data,
            gas_used: 1,
        });
        let request = thruput_json_rpc_types::requests::AirdropRequest {
            recipient: request.recipient,
            client_nonce: request.client_nonce,
        };
        let error = NativeFundingPlane::request_airdrop(&rpc, request.clone())
            .await
            .unwrap_err();
        assert_known_native_funding_error("faucet_requestAirdrop", &error);
        assert_eq!(
            error.code(),
            thruput_json_rpc_types::error::native::codes::BACKEND_DATA_INVALID
        );
        {
            let mut receipt = backend.receipt.lock().unwrap();
            let receipt = receipt.as_mut().unwrap();
            receipt.status = ExecStatus::Ok;
            receipt.return_data = Some(HexBytes(vec![]));
            receipt.abort_data = None;
        }
        let result = NativeFundingPlane::request_airdrop(&rpc, request)
            .await
            .unwrap();
        assert!(matches!(
            result,
            thruput_json_rpc_types::responses::AirdropAttempt::Succeeded { .. }
        ));
        assert_eq!(backend.submissions.lock().unwrap().len(), 1);
    }
}
