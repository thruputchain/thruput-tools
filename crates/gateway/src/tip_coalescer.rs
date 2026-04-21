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

use std::sync::Arc;

use async_trait::async_trait;
use groundhog_rpc_api_types::block::ChainTip;
use groundhog_rpc_api_types::hex::Hex0x;
use groundhog_scheduler::sched_coord::{
    SchedulerCoord_SchedulerCoord1_Client, SchedulerGetTipResult,
};
use tokio::sync::{RwLock, mpsc, oneshot};

use crate::config::TipConfig;
use crate::error::GatewayError;

/// Backend for chain-tip lookups.  In production this wraps the
/// scheduler client; tests substitute a mock.
#[async_trait]
pub trait TipSink: Send + Sync + 'static {
    async fn get_tip(&self) -> Result<ChainTip, GatewayError>;
}

/// Production sink: forwards to `schedcoord_get_tip` and maps the
/// outcome to a `ChainTip` (or a gateway error).
pub struct SchedulerTipSink {
    scheduler: Arc<RwLock<Option<SchedulerCoord_SchedulerCoord1_Client>>>,
}

impl SchedulerTipSink {
    pub fn new(scheduler: Arc<RwLock<Option<SchedulerCoord_SchedulerCoord1_Client>>>) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl TipSink for SchedulerTipSink {
    async fn get_tip(&self) -> Result<ChainTip, GatewayError> {
        let client = self.scheduler.read().await.clone();
        let client = client
            .ok_or_else(|| GatewayError::BackendUnavailable("scheduler not connected".into()))?;
        let result = client
            .schedcoord_get_tip()
            .await
            .map_err(|e| GatewayError::BackendUnavailable(e.to_string()))?;
        match result {
            SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => Ok(ChainTip {
                height: db_tip.height,
                block_hash: Hex0x(db_tip.block_hash.into_inner()),
            }),
            _ => Err(GatewayError::BackendUnavailable(
                "no committed blocks yet".into(),
            )),
        }
    }
}

/// Internal message: a waiter's oneshot reply channel.
type Waiter = oneshot::Sender<Result<ChainTip, GatewayError>>;

struct Coalescer {
    ingress: mpsc::Receiver<Waiter>,
    completion_rx: mpsc::UnboundedReceiver<()>,
    completion_tx: mpsc::UnboundedSender<()>,
    inflight: u32,
    max_inflight: u32,
    sink: Arc<dyn TipSink>,
}

impl Coalescer {
    async fn run(mut self) {
        loop {
            tokio::select! {
                biased;

                Some(()) = self.completion_rx.recv() => {
                    self.inflight = self.inflight.saturating_sub(1);
                }

                maybe_waiter = self.ingress.recv(), if self.inflight < self.max_inflight => {
                    let Some(first) = maybe_waiter else { break; };
                    // One RPC will satisfy every waiter currently queued, so
                    // drain the entire channel before firing.  Unlike the
                    // submitter there is no `max_size` — a bigger fanout is
                    // strictly better (more coalescing per RPC).
                    let mut waiters = vec![first];
                    while let Ok(w) = self.ingress.try_recv() {
                        waiters.push(w);
                    }
                    self.inflight += 1;
                    let sink = self.sink.clone();
                    let done = self.completion_tx.clone();
                    tokio::spawn(fetch_tip_once(sink, waiters, done));
                }
            }
        }
    }
}

async fn fetch_tip_once(
    sink: Arc<dyn TipSink>,
    waiters: Vec<Waiter>,
    done: mpsc::UnboundedSender<()>,
) {
    let result = sink.get_tip().await;
    for w in waiters {
        let _ = w.send(result.clone());
    }
    let _ = done.send(());
}

/// Handle returned to the rest of the gateway.  Clone-friendly.
#[derive(Clone)]
pub struct TipCoalescer {
    sender: mpsc::Sender<Waiter>,
}

impl TipCoalescer {
    pub fn spawn(config: &TipConfig, sink: &Arc<dyn TipSink>) -> Self {
        let (sender, ingress) = mpsc::channel(config.ingress_queue_depth.max(1));
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        let coalescer = Coalescer {
            ingress,
            completion_tx,
            completion_rx,
            inflight: 0,
            max_inflight: config.max_inflight_tips.max(1),
            sink: sink.clone(),
        };
        tokio::spawn(coalescer.run());
        Self { sender }
    }

    /// Ask for the current chain tip.  Returns `Overloaded` if the
    /// coalescer's ingress is full; otherwise awaits the fanout from
    /// the next completed backend RPC.
    pub async fn chain_tip(&self) -> Result<ChainTip, GatewayError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender.try_send(reply_tx).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                GatewayError::Overloaded("chainTip ingress queue full".into())
            }
            mpsc::error::TrySendError::Closed(_) => {
                GatewayError::BackendUnavailable("tip coalescer shut down".into())
            }
        })?;
        reply_rx
            .await
            .map_err(|_| GatewayError::BackendUnavailable("tip coalescer dropped reply".into()))?
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use groundhog_rpc_api_types::BlockHeight;
    use groundhog_rpc_api_types::hex::Hex0x;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Scripted mock sink.  Each `get_tip()` call pops the next gated
    /// response from a FIFO; if empty, returns a fixed default.
    struct MockSink {
        seen: Mutex<u32>,
        responses: Mutex<VecDeque<MockResponse>>,
        default_height: i64,
    }

    enum MockResponse {
        Gate(oneshot::Receiver<Result<ChainTip, GatewayError>>),
        Immediate(Result<ChainTip, GatewayError>),
    }

    impl MockSink {
        fn new(default_height: i64) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(0),
                responses: Mutex::new(VecDeque::new()),
                default_height,
            })
        }

        fn queue_gate(&self) -> oneshot::Sender<Result<ChainTip, GatewayError>> {
            let (tx, rx) = oneshot::channel();
            self.responses
                .lock()
                .unwrap()
                .push_back(MockResponse::Gate(rx));
            tx
        }

        fn queue_response(&self, r: Result<ChainTip, GatewayError>) {
            self.responses
                .lock()
                .unwrap()
                .push_back(MockResponse::Immediate(r));
        }

        fn seen(&self) -> u32 {
            *self.seen.lock().unwrap()
        }

        fn default_tip(height: i64) -> ChainTip {
            let bytes = [0u8; 32];
            ChainTip {
                height: BlockHeight::new(height),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from(bytes)),
            }
        }
    }

    #[async_trait]
    impl TipSink for MockSink {
        async fn get_tip(&self) -> Result<ChainTip, GatewayError> {
            {
                let mut n = self.seen.lock().unwrap();
                *n += 1;
            }
            let next = self.responses.lock().unwrap().pop_front();
            match next {
                None => Ok(Self::default_tip(self.default_height)),
                Some(MockResponse::Immediate(r)) => r,
                Some(MockResponse::Gate(rx)) => rx.await.unwrap_or_else(|_| {
                    Err(GatewayError::BackendUnavailable("test gate dropped".into()))
                }),
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

    #[tokio::test]
    async fn direct_path_at_low_load() {
        let mock = MockSink::new(42);
        let coalescer = TipCoalescer::spawn(
            &config_for_tests(4, 100),
            &(mock.clone() as Arc<dyn TipSink>),
        );

        let tip = coalescer.chain_tip().await.unwrap();
        assert_eq!(tip.height, BlockHeight::new(42));
        assert_eq!(mock.seen(), 1, "one backend call for one client call");
    }

    #[tokio::test]
    async fn coalesces_during_inflight() {
        let mock = MockSink::new(7);
        let gate = mock.queue_gate();

        let coalescer = TipCoalescer::spawn(
            &config_for_tests(1, 100), // strict serial
            &(mock.clone() as Arc<dyn TipSink>),
        );

        // First caller opens a gated RPC.
        let w1 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;
        assert_eq!(mock.seen(), 1, "first RPC started");

        // 10 more arrive while it's in flight.
        let mut more = vec![];
        for _ in 0..10 {
            let c = coalescer.clone();
            more.push(tokio::spawn(async move { c.chain_tip().await }));
        }
        settle().await;
        assert_eq!(mock.seen(), 1, "pile-up not yet fired");

        // Release the first RPC; the piled-up 10 drain as a single RPC.
        let _ = gate.send(Ok(MockSink::default_tip(7)));
        let r1 = w1.await.unwrap().unwrap();
        assert_eq!(r1.height, BlockHeight::new(7));
        for w in more {
            let tip = w.await.unwrap().unwrap();
            assert_eq!(tip.height, BlockHeight::new(7));
        }
        assert_eq!(mock.seen(), 2, "11 waiters, 2 backend calls");
    }

    #[tokio::test]
    async fn max_inflight_is_respected() {
        let mock = MockSink::new(0);
        let gate_a = mock.queue_gate();
        let gate_b = mock.queue_gate();

        let coalescer = TipCoalescer::spawn(
            &config_for_tests(2, 100),
            &(mock.clone() as Arc<dyn TipSink>),
        );

        let w1 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;
        let w2 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;
        assert_eq!(mock.seen(), 2, "two concurrent backend calls");

        // A third arrival sits queued.
        let w3 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;
        assert_eq!(mock.seen(), 2, "third held until one completes");

        let _ = gate_a.send(Ok(MockSink::default_tip(1)));
        w1.await.unwrap().unwrap();
        settle().await;
        assert_eq!(mock.seen(), 3, "third fires on completion");

        let _ = gate_b.send(Ok(MockSink::default_tip(1)));
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
        );

        // First caller: its RPC is gated, will get height 10.
        let first = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;

        // Release the first RPC with height 10.
        let _ = gate_first.send(Ok(MockSink::default_tip(10)));
        let r1 = first.await.unwrap().unwrap();
        assert_eq!(r1.height, BlockHeight::new(10));
        settle().await;

        // Now queue a new response of height 20 and submit a fresh waiter.
        // It must be served by a newly-initiated RPC, not a cached value.
        mock.queue_response(Ok(MockSink::default_tip(20)));
        let r2 = coalescer.chain_tip().await.unwrap();
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
        );

        // First pull: taken out of the channel immediately by the coalescer.
        let w1 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;

        // Second pull: channel has 1 free slot, goes there.
        let w2 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;

        // Third pull: channel full → Overloaded.
        let err = coalescer
            .chain_tip()
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
        );

        let w1 = {
            let c = coalescer.clone();
            tokio::spawn(async move { c.chain_tip().await })
        };
        settle().await;

        // Pile up 3 more behind the gated RPC.  These will land in the
        // *second* RPC (no gate), so to exercise fanout-on-error we have
        // to fail the gated one and check the first waiter.
        let mut more = vec![];
        for _ in 0..3 {
            let c = coalescer.clone();
            more.push(tokio::spawn(async move { c.chain_tip().await }));
        }
        settle().await;

        let _ = err_gate.send(Err(GatewayError::BackendUnavailable("mock".into())));
        let r1 = w1.await.unwrap();
        assert!(
            matches!(r1, Err(GatewayError::BackendUnavailable(_))),
            "w1: {r1:?}"
        );

        // The 3 piled-up waiters go through the next RPC (no gate, default Ok).
        for w in more {
            let tip = w.await.unwrap().unwrap();
            assert_eq!(tip.height, BlockHeight::new(0));
        }
    }
}
