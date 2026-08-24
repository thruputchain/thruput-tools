//! Request coalescing for `chainTip`.
//!
//! See `TipConfig` for the design rationale.  Structurally this is
//! the same shape as `submitter` — bounded in-flight, fanout on
//! completion — but the amortization mechanism differs: `chainTip`
//! has no per-request payload, so many waiters can share a single
//! backend RPC's result.  Linearizable: every waiter receives a
//! `ChainTip` from an RPC initiated strictly after they arrived.
//!
//! Backpressure mirrors the submitter: a bounded ingress channel
//! with `try_send` at the handler; full → `Overloaded`.

use std::{sync::Arc, time::Duration, time::Instant};

use async_trait::async_trait;
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::NetworkHash;
use groundhog_json_rpc_types::block::{ChainTip, consensus_value_hash};
use groundhog_json_rpc_types::network_info::NetworkInfo;
use groundhog_scheduler::sched_coord::SchedulerGetTipResult;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::task::{AbortHandle, JoinSet};

use crate::adaptive_batch::deadline_cohorts;
use crate::config::{HealthBackend, TipConfig};
use crate::context::{AbortTaskOnDrop, SchedulerClient, clear_scheduler_if_current};
use crate::error::GatewayError;
use crate::metrics::{BackendRequestOutcome, GatewayMetrics, REASON_CLOSED, REASON_INGRESS_FULL};

const TIP_COALESCER_COMPONENT: &str = "tip_coalescer";

/// A scheduler tip together with the network identity it was observed on.
///
/// The scheduler returns both in one call, so the gateway reads the network
/// hash off the tip it already fetches rather than asking for it separately.
#[derive(Debug, Clone)]
pub struct TipSnapshot {
    /// Chain state that advances with each commit.
    pub tip: ChainTip,
    /// Network identity, fixed for the life of the chain.
    pub network_hash: NetworkHash,
}

/// Backend for chain-tip lookups.  In production this wraps the
/// scheduler client; tests substitute a mock.
#[async_trait]
pub trait TipSink: Send + Sync + 'static {
    async fn get_tip(&self, deadline: Instant) -> Result<TipSnapshot, GatewayError>;
}

/// Production sink: forwards to `schedcoord_get_tip` and maps the
/// outcome to a `ChainTip` (or a gateway error).
pub struct SchedulerTipSink {
    scheduler: Arc<RwLock<Option<SchedulerClient>>>,
    metrics: GatewayMetrics,
    timeout: Duration,
}

impl SchedulerTipSink {
    pub(crate) fn new(
        scheduler: Arc<RwLock<Option<SchedulerClient>>>,
        metrics: GatewayMetrics,
        timeout: Duration,
    ) -> Self {
        Self {
            scheduler,
            metrics,
            timeout,
        }
    }
}

#[async_trait]
impl TipSink for SchedulerTipSink {
    async fn get_tip(&self, deadline: Instant) -> Result<TipSnapshot, GatewayError> {
        let client = self.scheduler.read().await.clone();
        let client = client
            .ok_or_else(|| GatewayError::BackendUnavailable("scheduler not connected".into()))?;
        let generation = client.generation();
        let started_at = Instant::now();
        let timeout = self
            .timeout
            .min(deadline.saturating_duration_since(started_at));
        if timeout.is_zero() {
            return Err(GatewayError::BackendTimeout);
        }
        let mut call = tokio::spawn(async move { client.schedcoord_get_tip().await });
        let _abort_call_on_drop = AbortTaskOnDrop::new(&call);
        let rpc_result = if let Ok(result) = tokio::time::timeout(timeout, &mut call).await {
            Ok(result.unwrap_or(Err(rpc_driver::RpcError::SystemError)))
        } else {
            clear_scheduler_if_current(&self.scheduler, generation).await;
            call.abort();
            let _ = call.await;
            Err(())
        };
        let outcome = match &rpc_result {
            Ok(Ok(SchedulerGetTipResult::SchedulerGetTipOK { .. })) => BackendRequestOutcome::Ok,
            Ok(Ok(_)) => BackendRequestOutcome::Rejected,
            Ok(Err(_)) => BackendRequestOutcome::Error,
            Err(()) => BackendRequestOutcome::Timeout,
        };
        self.metrics.observe_backend_request(
            HealthBackend::Scheduler,
            "schedcoord_get_tip",
            outcome,
            started_at.elapsed(),
        );
        if matches!(rpc_result, Ok(Err(_))) {
            clear_scheduler_if_current(&self.scheduler, generation).await;
        }
        let result = rpc_result
            .map_err(|()| GatewayError::BackendTimeout)?
            .map_err(|e| GatewayError::BackendUnavailable(e.to_string()))?;
        match result {
            SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => {
                let block_hash = consensus_value_hash(&db_tip.value).map_err(|error| {
                    GatewayError::BackendUnavailable(format!(
                        "scheduler returned an unencodable consensus value: {error}"
                    ))
                })?;
                Ok(TipSnapshot {
                    network_hash: NetworkHash::from(
                        db_tip
                            .value
                            .proposal_header
                            .network_hash
                            .clone()
                            .into_inner(),
                    ),
                    tip: ChainTip {
                        height: db_tip.value.proposal_header.block_height.clone(),
                        block_hash,
                    },
                })
            }
            SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => Err(
                GatewayError::BackendUnavailable("no committed blocks yet".into()),
            ),
            other => Err(GatewayError::BackendUnavailable(format!(
                "unexpected scheduler tip result: {other:?}"
            ))),
        }
    }
}

struct Waiter {
    reply: oneshot::Sender<Result<TipSnapshot, GatewayError>>,
    backend_deadline: Instant,
}

struct Coalescer {
    ingress: mpsc::Receiver<Waiter>,
    max_inflight: u32,
    sink: Arc<dyn TipSink>,
    metrics: GatewayMetrics,
    max_deadline_skew: Duration,
}

impl Coalescer {
    async fn run(mut self) {
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                biased;

                Some(result) = workers.join_next(), if !workers.is_empty() => {
                    if let Err(error) = result
                        && !error.is_cancelled()
                    {
                        tracing::error!(%error, "tip coalescer worker failed");
                    }
                }

                maybe_waiter = self.ingress.recv(), if workers.len() < self.max_inflight as usize => {
                    let Some(first) = maybe_waiter else { break; };
                    // Drain the channel, then split materially different
                    // deadlines so an old waiter cannot poison newer ones.
                    let mut waiters = vec![first];
                    while let Ok(w) = self.ingress.try_recv() {
                        waiters.push(w);
                    }
                    let now = Instant::now();
                    let mut live = Vec::with_capacity(waiters.len());
                    for waiter in waiters {
                        if waiter.reply.is_closed() {
                            continue;
                        }
                        if waiter.backend_deadline <= now {
                            let _ = waiter.reply.send(Err(GatewayError::BackendTimeout));
                        } else {
                            live.push(waiter);
                        }
                    }
                    let cohorts = deadline_cohorts(live, self.max_deadline_skew, |waiter| {
                        waiter.backend_deadline
                    });
                    if cohorts.is_empty() {
                        continue;
                    }
                    let in_flight = self
                        .metrics
                        .internal_batch_in_flight_guard(TIP_COALESCER_COMPONENT);
                    let sink = self.sink.clone();
                    let metrics = self.metrics.clone();
                    workers.spawn(async move {
                        let _in_flight = in_flight;
                        fetch_tip_cohorts(sink, cohorts, metrics).await;
                    });
                }
            }
        }
        workers.shutdown().await;
    }
}

async fn fetch_tip_cohorts(
    sink: Arc<dyn TipSink>,
    cohorts: Vec<Vec<Waiter>>,
    metrics: GatewayMetrics,
) {
    for mut waiters in cohorts.into_iter().rev() {
        let now = Instant::now();
        waiters.retain(|waiter| !waiter.reply.is_closed());
        if waiters.is_empty() {
            continue;
        }
        let deadline = waiters[0].backend_deadline;
        if deadline <= now {
            for waiter in waiters {
                let _ = waiter.reply.send(Err(GatewayError::BackendTimeout));
            }
            continue;
        }
        metrics.observe_coalesced_waiters(TIP_COALESCER_COMPONENT, waiters.len());
        let result = sink.get_tip(deadline).await;
        for waiter in waiters {
            let _ = waiter.reply.send(result.clone());
        }
    }
}

/// Handle returned to the rest of the gateway.  Clone-friendly.
#[derive(Clone)]
pub struct TipCoalescer {
    inner: Arc<TipCoalescerInner>,
    metrics: GatewayMetrics,
    request_timeout: Duration,
    backend_timeout: Duration,
}

struct TipCoalescerInner {
    sender: mpsc::Sender<Waiter>,
    task: AbortHandle,
}

impl Drop for TipCoalescerInner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TipCoalescer {
    #[cfg(test)]
    pub fn spawn(config: &TipConfig, sink: &Arc<dyn TipSink>, metrics: GatewayMetrics) -> Self {
        Self::spawn_with_deadlines(
            config,
            sink,
            metrics,
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
    }

    pub(crate) fn spawn_with_deadlines(
        config: &TipConfig,
        sink: &Arc<dyn TipSink>,
        metrics: GatewayMetrics,
        request_timeout: Duration,
        backend_timeout: Duration,
        max_deadline_skew: Duration,
    ) -> Self {
        let (sender, ingress) = mpsc::channel(config.ingress_queue_depth.max(1));
        let coalescer = Coalescer {
            ingress,
            max_inflight: config.max_inflight_tips.max(1),
            sink: sink.clone(),
            metrics: metrics.clone(),
            max_deadline_skew,
        };
        let task = tokio::spawn(coalescer.run());
        Self {
            inner: Arc::new(TipCoalescerInner {
                sender,
                task: task.abort_handle(),
            }),
            metrics,
            request_timeout,
            backend_timeout,
        }
    }

    /// Abort the coalescer runner and all in-flight workers. Safe to call repeatedly.
    pub(crate) fn abort(&self) {
        self.inner.task.abort();
    }

    /// Ask for the current chain tip.  Returns `Overloaded` if the
    /// coalescer's ingress is full; otherwise awaits the fanout from
    /// the next completed backend RPC.
    #[tracing::instrument(skip_all)]
    async fn tip_snapshot(&self, context: RpcRequestContext) -> Result<TipSnapshot, GatewayError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request_deadline = context
            .capped_from_start(self.request_timeout)
            .deadline()
            .expect("operation timeout should bound chain-tip lookup");
        let backend_deadline = context
            .capped_from_start(self.backend_timeout)
            .deadline()
            .expect("backend timeout should bound chain-tip lookup");
        if backend_deadline <= Instant::now() {
            return Err(GatewayError::BackendTimeout);
        }
        self.inner
            .sender
            .try_send(Waiter {
                reply: reply_tx,
                backend_deadline,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => {
                    self.metrics.observe_internal_batch_rejected(
                        TIP_COALESCER_COMPONENT,
                        REASON_INGRESS_FULL,
                    );
                    GatewayError::Overloaded("chainTip ingress queue full".into())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    self.metrics
                        .observe_internal_batch_rejected(TIP_COALESCER_COMPONENT, REASON_CLOSED);
                    GatewayError::BackendUnavailable("tip coalescer shut down".into())
                }
            })?;
        tokio::time::timeout_at(request_deadline.into(), reply_rx)
            .await
            .map_err(|_| GatewayError::BackendTimeout)?
            .map_err(|_| GatewayError::BackendUnavailable("tip coalescer dropped reply".into()))?
    }

    /// Ask for the current chain tip.
    #[tracing::instrument(skip_all)]
    pub async fn chain_tip(&self, context: RpcRequestContext) -> Result<ChainTip, GatewayError> {
        Ok(self.tip_snapshot(context).await?.tip)
    }

    /// Ask for the identity of the network the scheduler is serving.
    ///
    /// Read off the same tip the scheduler already returns, so this costs no
    /// extra backend call.
    #[tracing::instrument(skip_all)]
    pub async fn network_info(
        &self,
        context: RpcRequestContext,
    ) -> Result<NetworkInfo, GatewayError> {
        Ok(NetworkInfo::new(
            self.tip_snapshot(context).await?.network_hash,
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_json_rpc_types::BlockHeight;
    use groundhog_json_rpc_types::hex::Hex0x;
    use std::collections::VecDeque;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio::sync::Notify;

    /// Scripted mock sink.  Each `get_tip()` call pops the next gated
    /// response from a FIFO; if empty, returns a fixed default.
    struct MockSink {
        seen: Mutex<u32>,
        deadlines: Mutex<Vec<Instant>>,
        responses: Mutex<VecDeque<MockResponse>>,
        default_height: i64,
    }

    enum MockResponse {
        Gate(oneshot::Receiver<Result<TipSnapshot, GatewayError>>),
        Immediate(Result<TipSnapshot, GatewayError>),
    }

    impl MockSink {
        fn new(default_height: i64) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(0),
                deadlines: Mutex::new(Vec::new()),
                responses: Mutex::new(VecDeque::new()),
                default_height,
            })
        }

        fn queue_gate(&self) -> oneshot::Sender<Result<TipSnapshot, GatewayError>> {
            let (tx, rx) = oneshot::channel();
            self.responses
                .lock()
                .unwrap()
                .push_back(MockResponse::Gate(rx));
            tx
        }

        fn queue_response(&self, r: Result<TipSnapshot, GatewayError>) {
            self.responses
                .lock()
                .unwrap()
                .push_back(MockResponse::Immediate(r));
        }

        fn seen(&self) -> u32 {
            *self.seen.lock().unwrap()
        }

        fn deadlines(&self) -> Vec<Instant> {
            self.deadlines.lock().unwrap().clone()
        }
    }

    fn default_tip(height: i64) -> TipSnapshot {
        let bytes = [0u8; 32];
        TipSnapshot {
            tip: ChainTip {
                height: BlockHeight::new(height),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from(bytes)),
            },
            network_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from(bytes)),
        }
    }

    #[async_trait]
    impl TipSink for MockSink {
        async fn get_tip(&self, deadline: Instant) -> Result<TipSnapshot, GatewayError> {
            {
                let mut n = self.seen.lock().unwrap();
                *n += 1;
            }
            self.deadlines.lock().unwrap().push(deadline);
            let next = self.responses.lock().unwrap().pop_front();
            match next {
                None => Ok(default_tip(self.default_height)),
                Some(MockResponse::Immediate(r)) => r,
                Some(MockResponse::Gate(rx)) => rx.await.unwrap_or_else(|_| {
                    Err(GatewayError::BackendUnavailable("test gate dropped".into()))
                }),
            }
        }
    }

    struct TimeoutInvalidatesTipSink {
        cutoff: Instant,
        valid: AtomicBool,
        deadlines: Mutex<Vec<Instant>>,
    }

    struct PanicOnceTipSink {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl TipSink for PanicOnceTipSink {
        async fn get_tip(&self, _deadline: Instant) -> Result<TipSnapshot, GatewayError> {
            assert_ne!(
                self.calls.fetch_add(1, Ordering::SeqCst),
                0,
                "intentional test panic"
            );
            Ok(default_tip(21))
        }
    }

    struct PendingTipSink {
        started: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl TipSink for PendingTipSink {
        async fn get_tip(&self, _deadline: Instant) -> Result<TipSnapshot, GatewayError> {
            let _marker = DropMarker(self.dropped.clone());
            self.started.notify_one();
            std::future::pending().await
        }
    }

    #[async_trait]
    impl TipSink for TimeoutInvalidatesTipSink {
        async fn get_tip(&self, deadline: Instant) -> Result<TipSnapshot, GatewayError> {
            self.deadlines.lock().unwrap().push(deadline);
            if deadline < self.cutoff {
                self.valid.store(false, Ordering::SeqCst);
                Err(GatewayError::BackendTimeout)
            } else if self.valid.load(Ordering::SeqCst) {
                Ok(default_tip(20))
            } else {
                Err(GatewayError::BackendUnavailable(
                    "scheduler invalidated by older timeout".into(),
                ))
            }
        }
    }

    fn config_for_tests(max_inflight: u32, ingress: usize) -> TipConfig {
        TipConfig {
            max_inflight_tips: max_inflight,
            ingress_queue_depth: ingress,
        }
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    fn request_context() -> RpcRequestContext {
        RpcRequestContext::unbounded()
    }

    #[tokio::test]
    async fn direct_path_at_low_load() {
        let mock = MockSink::new(42);
        let coalescer = TipCoalescer::spawn(
            &config_for_tests(4, 100),
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        let tip = coalescer.chain_tip(request_context()).await.unwrap();
        assert_eq!(tip.height, BlockHeight::new(42));
        assert_eq!(mock.seen(), 1, "one backend call for one client call");
    }

    #[tokio::test]
    async fn request_deadline_reaches_tip_sink() {
        let mock = MockSink::new(42);
        let coalescer = TipCoalescer::spawn(
            &config_for_tests(4, 100),
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );
        let context = RpcRequestContext::new(Some(Duration::from_secs(5)));
        let expected = context.deadline().expect("bounded request deadline");

        coalescer
            .chain_tip(context)
            .await
            .expect("chain-tip lookup should complete");

        assert_eq!(mock.deadlines(), vec![expected]);
    }

    #[tokio::test]
    async fn expired_tip_waiter_is_not_dispatched() {
        let mock = MockSink::new(42);
        let coalescer = TipCoalescer::spawn(
            &config_for_tests(4, 100),
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        let result = coalescer
            .chain_tip(RpcRequestContext::new(Some(Duration::ZERO)))
            .await;

        assert!(matches!(result, Err(GatewayError::BackendTimeout)));
        assert_eq!(mock.seen(), 0);
    }

    #[tokio::test]
    async fn panicking_sink_does_not_wedge_max_inflight() {
        let sink = Arc::new(PanicOnceTipSink {
            calls: AtomicUsize::new(0),
        });
        let coalescer = TipCoalescer::spawn(
            &config_for_tests(1, 10),
            &(sink.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        assert!(matches!(
            coalescer.chain_tip(request_context()).await,
            Err(GatewayError::BackendUnavailable(_))
        ));
        let tip = tokio::time::timeout(
            Duration::from_secs(1),
            coalescer.chain_tip(request_context()),
        )
        .await
        .expect("bug: replacement tip worker should run")
        .expect("bug: replacement tip worker should succeed");
        assert_eq!(tip.height, BlockHeight::new(21));
    }

    #[tokio::test]
    async fn abort_cancels_pending_sink_and_drops_reply() {
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(PendingTipSink {
            started: started.clone(),
            dropped: dropped.clone(),
        });
        let coalescer = TipCoalescer::spawn(
            &config_for_tests(1, 10),
            &(sink as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );
        let waiter = tokio::spawn({
            let coalescer = coalescer.clone();
            async move { coalescer.chain_tip(request_context()).await }
        });
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("bug: pending sink should start");

        coalescer.abort();

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("bug: waiter should finish after abort")
                .expect("bug: waiter task should not panic"),
            Err(GatewayError::BackendUnavailable(_))
        ));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn coalesces_during_inflight() {
        let mock = MockSink::new(7);
        let gate = mock.queue_gate();

        let coalescer = TipCoalescer::spawn(
            &config_for_tests(1, 100), // strict serial
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        // First caller opens a gated RPC.
        let w1 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip(request_context()).await })
        };
        settle().await;
        assert_eq!(mock.seen(), 1, "first RPC started");

        // 10 more arrive while it's in flight.
        let mut more = vec![];
        for _ in 0..10 {
            let c = coalescer.clone();
            more.push(tokio::spawn(
                async move { c.chain_tip(request_context()).await },
            ));
        }
        settle().await;
        assert_eq!(mock.seen(), 1, "pile-up not yet fired");

        // Release the first RPC; the piled-up 10 drain as a single RPC.
        let _ = gate.send(Ok(default_tip(7)));
        let r1 = w1.await.unwrap().unwrap();
        assert_eq!(r1.height, BlockHeight::new(7));
        for w in more {
            let tip = w.await.unwrap().unwrap();
            assert_eq!(tip.height, BlockHeight::new(7));
        }
        assert_eq!(mock.seen(), 2, "11 waiters, 2 backend calls");
    }

    #[tokio::test]
    async fn older_deadline_cohort_does_not_timeout_newer_tip_waiters() {
        let mock = MockSink::new(0);
        let first_gate = mock.queue_gate();
        mock.queue_response(Ok(default_tip(20)));
        mock.queue_response(Err(GatewayError::BackendTimeout));
        let coalescer = TipCoalescer::spawn_with_deadlines(
            &config_for_tests(1, 100),
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_millis(10),
        );

        let first = tokio::spawn({
            let coalescer = coalescer.clone();
            async move { coalescer.chain_tip(request_context()).await }
        });
        settle().await;

        let old = tokio::spawn({
            let coalescer = coalescer.clone();
            async move {
                coalescer
                    .chain_tip(RpcRequestContext::new(Some(Duration::from_secs(2))))
                    .await
            }
        });
        let newer = tokio::spawn({
            let coalescer = coalescer.clone();
            async move {
                coalescer
                    .chain_tip(RpcRequestContext::new(Some(Duration::from_secs(5))))
                    .await
            }
        });
        settle().await;

        first_gate
            .send(Ok(default_tip(10)))
            .expect("first gate should be open");
        first.await.unwrap().unwrap();

        assert!(matches!(
            old.await.unwrap(),
            Err(GatewayError::BackendTimeout)
        ));
        assert_eq!(newer.await.unwrap().unwrap().height, BlockHeight::new(20));
        assert_eq!(mock.seen(), 3);
    }

    #[tokio::test]
    async fn newer_tip_finishes_before_old_timeout_invalidates_scheduler() {
        let now = Instant::now();
        let sink = Arc::new(TimeoutInvalidatesTipSink {
            cutoff: now + Duration::from_secs(3),
            valid: AtomicBool::new(true),
            deadlines: Mutex::new(Vec::new()),
        });
        let (old_reply, old_waiter) = oneshot::channel();
        let (newer_reply, newer_waiter) = oneshot::channel();
        let cohorts = vec![
            vec![Waiter {
                reply: old_reply,
                backend_deadline: now + Duration::from_secs(2),
            }],
            vec![Waiter {
                reply: newer_reply,
                backend_deadline: now + Duration::from_secs(5),
            }],
        ];

        fetch_tip_cohorts(sink.clone(), cohorts, GatewayMetrics::for_tests()).await;

        assert_eq!(
            newer_waiter
                .await
                .expect("newer reply should be sent")
                .expect("newer cohort should finish before invalidation")
                .tip
                .height,
            BlockHeight::new(20)
        );
        assert!(matches!(
            old_waiter.await.expect("old reply should be sent"),
            Err(GatewayError::BackendTimeout)
        ));
        assert_eq!(
            sink.deadlines.lock().unwrap().as_slice(),
            &[now + Duration::from_secs(5), now + Duration::from_secs(2)]
        );
    }

    #[tokio::test]
    async fn max_inflight_is_respected() {
        let mock = MockSink::new(0);
        let gate_a = mock.queue_gate();
        let gate_b = mock.queue_gate();

        let coalescer = TipCoalescer::spawn(
            &config_for_tests(2, 100),
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        let w1 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip(request_context()).await })
        };
        settle().await;
        let w2 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip(request_context()).await })
        };
        settle().await;
        assert_eq!(mock.seen(), 2, "two concurrent backend calls");

        // A third arrival sits queued.
        let w3 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip(request_context()).await })
        };
        settle().await;
        assert_eq!(mock.seen(), 2, "third held until one completes");

        let _ = gate_a.send(Ok(default_tip(1)));
        w1.await.unwrap().unwrap();
        settle().await;
        assert_eq!(mock.seen(), 3, "third fires on completion");

        let _ = gate_b.send(Ok(default_tip(1)));
        w2.await.unwrap().unwrap();
        w3.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn linearizable_across_rpcs() {
        // Setup: first RPC returns height H; a waiter that arrives *after*
        // the first completes must be served by a second RPC returning H+1.
        let mock = MockSink::new(0);
        let gate_first = mock.queue_gate();

        let coalescer = TipCoalescer::spawn(
            &config_for_tests(1, 100),
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        // First caller: its RPC is gated, will get height 10.
        let first = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip(request_context()).await })
        };
        settle().await;

        // Release the first RPC with height 10.
        let _ = gate_first.send(Ok(default_tip(10)));
        let r1 = first.await.unwrap().unwrap();
        assert_eq!(r1.height, BlockHeight::new(10));
        settle().await;

        // Now queue a new response of height 20 and submit a fresh waiter.
        // It must be served by a newly-initiated RPC, not a cached value.
        mock.queue_response(Ok(default_tip(20)));
        let r2 = coalescer.chain_tip(request_context()).await.unwrap();
        assert_eq!(
            r2.height,
            BlockHeight::new(20),
            "post-t1 arrival sees post-t1 RPC"
        );
        assert_eq!(mock.seen(), 2, "two distinct RPCs");
    }

    #[tokio::test]
    async fn ingress_full_returns_overloaded() {
        let mock = MockSink::new(0);
        let gate_holder = mock.queue_gate(); // never fire

        let coalescer = TipCoalescer::spawn(
            &config_for_tests(1, 1), // 1 in flight, 1 queue slot
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        // First pull: taken out of the channel immediately by the coalescer.
        let w1 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip(request_context()).await })
        };
        settle().await;

        // Second pull: channel has 1 free slot, goes there.
        let w2 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip(request_context()).await })
        };
        settle().await;

        // Third pull: channel full → Overloaded.
        let err = coalescer
            .chain_tip(request_context())
            .await
            .expect_err("should be overloaded");
        assert!(
            matches!(err, GatewayError::Overloaded(_)),
            "got {err:?}, expected Overloaded"
        );

        drop(gate_holder);
        let _ = w1.await;
        let _ = w2.await;
    }

    #[tokio::test]
    async fn backend_error_fans_out_to_every_waiter() {
        let mock = MockSink::new(0);
        let err_gate = mock.queue_gate();

        let coalescer = TipCoalescer::spawn(
            &config_for_tests(1, 100),
            &(mock.clone() as Arc<dyn TipSink>),
            GatewayMetrics::for_tests(),
        );

        // Spawn four chain_tip calls back-to-back with no intervening
        // yield.  On `current_thread` (the default `#[tokio::test]`
        // runtime) all four tasks run to their `try_send` + `await
        // reply_rx` yield before the coalescer task gets to poll, so
        // all four waiters land in the coalescer's mpsc.  The coalescer
        // drains them into one fanout on its first ingress pull, which
        // is the RPC gated on `err_gate`.
        let mut waiters = vec![];
        for _ in 0..4 {
            let c = coalescer.clone();
            waiters.push(tokio::spawn(
                async move { c.chain_tip(request_context()).await },
            ));
        }
        settle().await;

        // Fail the single RPC that holds all four waiters.
        let _ = err_gate.send(Err(GatewayError::BackendUnavailable("mock".into())));
        for (i, w) in waiters.into_iter().enumerate() {
            let r = w.await.unwrap();
            assert!(
                matches!(r, Err(GatewayError::BackendUnavailable(_))),
                "waiter {i}: expected BackendUnavailable, got {r:?}"
            );
        }
        assert_eq!(
            mock.seen(),
            1,
            "all four waiters should have shared a single get_tip call"
        );
    }
}
