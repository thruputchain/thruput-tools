use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use groundhog_json_rpc_types::{
    BlockHeight,
    airdrop::ClientNonce,
    block::ChainTip,
    hex::{Hex0x, HexBytes},
    transaction::{ConflictCode, ConflictDiagnostic, TransactionStatus},
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
            block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x77; 32])),
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
    ) -> Result<Option<groundhog_json_rpc_types::receipt::TransactionReceipt>, RpcServerError> {
        Ok(None)
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
            network_hash: groundhog_xdr::types::NetworkHash::from(
                groundhog_xdr::types::SHA256Hash::from([network_byte; 32]),
            ),
            signer: Arc::new(LocalSigner::from_secret_key([0x33; 32])),
            next_nonce: AtomicU64::new(7),
            raw_amount: 100_000_000,
        }
        .build(
            ContractAddress::from(groundhog_xdr::types::ContractAddress::from([0x44; 32])),
            1_000,
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
    use groundhog_json_rpc_server::batch::{ConcurrentBatchService, RpcBatchConcurrency};
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
        let handle = server.start(rpc.clone().into_rpc());
        let body = serde_json::to_string(
            &(1..=20)
                .map(|id| {
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id, "method": "groundhog_requestAirdrop",
                        "params": [test_request(id, id)]
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
        while rpc.admission.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let error = rpc.request_airdrop(test_request(2, 2)).await.unwrap_err();
    assert_eq!(
        error.code(),
        groundhog_json_rpc_types::error::codes::OVERLOADED
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
    let mut rpc = test_rpc(backend.clone(), builder.clone(), 1, 1);
    rpc.timeout = Duration::from_millis(10);
    let execution = rpc.concurrency.acquire().await.unwrap();
    assert!(matches!(
        rpc.request(test_request(1, 1)).await,
        Err(RpcServerError::BackendTimeout)
    ));
    assert_eq!(rpc.admission.available_permits(), 16);
    assert_eq!(builder.constructions.load(Ordering::SeqCst), 0);
    drop(execution);
    // A pristine expired waiter can be evicted, but signed ambiguous work cannot.
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

    let error = LocalAirdropContext::from_validated_runtime(&manifest, &config)
        .err()
        .expect("wrong dev-0 role must fail validation");

    assert!(error.contains("derivation does not match"));
}

#[test]
fn local_context_rejects_wrong_derivation_seed() {
    let config = NodeSupervisorConfig::default();
    let mut manifest = runtime_manifest(&config);
    manifest.bootstrap_account_derivation.seed = "wrong-seed".into();

    let error = LocalAirdropContext::from_validated_runtime(&manifest, &config)
        .err()
        .expect("wrong derivation seed must fail validation");

    assert!(error.contains("derivation does not match"));
}

#[test]
fn local_context_rejects_wrong_faucet_public_key() {
    let config = NodeSupervisorConfig::default();
    let mut manifest = runtime_manifest(&config);
    manifest.bootstrap_accounts[0].public_key =
        Hex0x(groundhog_xdr::types::Ed25519PublicKey::from([0x99; 32]));

    let error = LocalAirdropContext::from_validated_runtime(&manifest, &config)
        .err()
        .expect("wrong faucet public key must fail validation");

    assert!(error.contains("public key does not match"));
}

#[test]
fn local_context_rejects_non_native_faucet_address() {
    let config = NodeSupervisorConfig::default();
    let mut manifest = runtime_manifest(&config);
    manifest.bootstrap_accounts[0].wallet_address = Hex0x([0; 32].into());

    let error = LocalAirdropContext::from_validated_runtime(&manifest, &config)
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
                cause: groundhog_json_rpc_types::transaction::TransactionFailureCause::DbConflict {
                    conflict: ConflictDiagnostic {
                        address: Hex0x(groundhog_xdr::types::ContractAddress::from([0x44; 32])),
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

#[test]
fn attempt_is_evictable_when_pristine_or_terminal_with_an_outcome() {
    let attempt = Attempt::new();
    assert!(attempt.is_evictable());
    attempt
        .store_signed(SignedAirdrop {
            bytes: vec![1, 2, 3],
            tx_hash: test_tx_hash(),
        })
        .expect("bug: signed transaction should be stored");
    attempt
        .set_terminal(true)
        .expect("bug: terminal state should update");

    assert!(!attempt.is_evictable());

    attempt.record_result(Err(RpcServerError::TxRejected("failed".into())));
    assert!(attempt.is_evictable());
}

#[tokio::test]
async fn abandoned_pristine_attempt_does_not_consume_capacity() {
    let backend = Arc::new(TestBackend::new(vec![Ok(())]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1, 2, 3],
    });
    let rpc = test_rpc(backend, builder, 1, 1);
    let abandoned = test_request(1, 1);
    rpc.attempts
        .lock()
        .expect("bug: attempt store should be available")
        .insert(AttemptKey::new(&abandoned), Arc::new(Attempt::new()));

    let result = rpc.request(test_request(2, 2)).await;

    assert!(matches!(result, Ok(AirdropAttemptResponse::Pending { .. })));
}

#[tokio::test]
async fn borrowed_terminal_attempt_is_not_evicted() {
    let backend = Arc::new(TestBackend::new(vec![
        Err(RpcServerError::TxRejected("first".into())),
        Err(RpcServerError::TxRejected("second".into())),
    ]));
    let builder = Arc::new(CountingBuilder {
        constructions: AtomicUsize::new(0),
        bytes: vec![1, 2, 3],
    });
    let rpc = test_rpc(backend, builder, 1, 1);
    let first_request = test_request(1, 1);
    let first_key = AttemptKey::new(&first_request);
    assert!(matches!(
        rpc.request(first_request).await,
        Err(RpcServerError::TxRejected(_))
    ));
    let borrowed = rpc
        .attempts
        .lock()
        .expect("bug: attempt store should be available")
        .get(&first_key)
        .expect("bug: terminal attempt should be retained")
        .clone();

    assert!(matches!(
        rpc.request(test_request(2, 2)).await,
        Err(RpcServerError::Overloaded(_))
    ));
    drop(borrowed);
    assert!(matches!(
        rpc.request(test_request(2, 2)).await,
        Err(RpcServerError::TxRejected(_))
    ));
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
        recipient: Hex0x(groundhog_xdr::types::ContractAddress::from([recipient; 32])),
        client_nonce: ClientNonce::new(vec![nonce]).expect("bug: one-byte nonce should validate"),
    }
}

fn test_tx_hash() -> TxHash {
    TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0xaa; 32])))
}

fn runtime_manifest(config: &NodeSupervisorConfig) -> RuntimeManifest {
    let signer = derive_dev_account_signer(&config.hogd_dev.dev_account_seed, 0);
    serde_json::from_value(serde_json::json!({
            "schemaVersion": 3,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test00000000000",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "producer": { "binary": "hogd-dev", "version": "0.1.0" },
            "endpoints": { "scheduler": "127.0.0.1:1000" },
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": config.hogd_dev.dev_account_seed,
            },
            "bootstrapAccounts": [{
                "index": 0,
                "role": "faucet",
                "publicKey": format!("0x{}", hex::encode(signer.verifying_key_bytes())),
                "walletAddress": format!("0x{}", hex::encode(PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address().as_ref())),
                "initialStorageTokens": "100000000",
            }],
            "bootstrap": {
                "storageCostModel": {
                    "hash": format!("0x{}", "53".repeat(32)),
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
