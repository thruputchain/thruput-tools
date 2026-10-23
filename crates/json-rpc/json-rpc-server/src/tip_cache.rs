//! Shared db-tip reading: what the `sendTransaction` pre-checks read instead
//! of fetching a tip each time, and the background task that keeps it current.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::time::Instant;

use thruput_tx_validation::NetworkConfigParams;

use crate::backend::{ChainPlane, RpcRequestContext, TipReading};
use crate::config::RpcServerConfig;
use crate::error::RpcServerError;

/// What one db-tip fetch resolved, so each later access is a field read.
#[derive(Debug, Clone)]
pub(crate) struct TipState {
    pub(crate) height: i64,
    /// `None` when the tip's network config is a version this build cannot
    /// interpret: chain-configured pre-checks are then skipped.
    pub(crate) params: Option<NetworkConfigParams>,
}

impl TipState {
    pub(crate) fn from_reading(reading: &TipReading) -> Self {
        let params = match NetworkConfigParams::from_network_config(&reading.network_config) {
            Ok(params) => Some(params),
            Err(error) => {
                tracing::warn!(%error, "tip network config unreadable, chain-configured pre-checks skipped");
                None
            }
        };
        Self {
            height: *reading.tip.height,
            params,
        }
    }
}

/// Rate-limited, best-effort db-tip reading shared across this server.
///
/// One caller per window claims the refresh and the rest reuse the last
/// reading without waiting, so a slow probe never queues submits behind
/// it. A failed probe keeps the previous reading and overlapping fetches
/// keep the higher tip, both safe because the tip only grows and the
/// config changes only at upgrade blocks. The exception is a cold cache,
/// where callers do wait on the in-flight probe rather than answer empty.
/// Those allowances suit an advisory pre-check.
#[derive(Debug, Clone)]
pub(crate) struct TipCache {
    inner: Arc<TipCacheInner>,
}

#[derive(Debug)]
struct TipCacheInner {
    // Every submission reads this cache. Critical sections only inspect or
    // replace a small snapshot and never await. A synchronous lock avoids
    // handing each contended read through Tokio's FIFO task wakeup queue.
    state: Mutex<TipCacheState>,
    /// Signalled when a probe finishes, whatever its outcome, so cold-cache
    /// waiters can re-read the state.
    probe_done: tokio::sync::watch::Sender<()>,
}

impl Default for TipCache {
    fn default() -> Self {
        let (probe_done, _) = tokio::sync::watch::channel(());
        Self {
            inner: Arc::new(TipCacheInner {
                state: Mutex::default(),
                probe_done,
            }),
        }
    }
}

#[derive(Debug, Default)]
struct TipCacheState {
    /// When a probe was last claimed, whatever became of it. This is the
    /// rate limiter: it decides who probes, not how old the reading is.
    last_attempt: Option<Instant>,
    reading: Option<TipState>,
    /// A probe claimed the current window and has not reported back. Stays
    /// set if that probe's request is cancelled mid-flight; cold-cache
    /// waiters therefore give up after the probe timeout rather than trust
    /// it, and the next claimed window resets it.
    in_flight: bool,
}

impl TipCacheState {
    /// Keep `reading` unless a newer one is already stored.
    fn record(&mut self, reading: TipState) {
        if self
            .reading
            .as_ref()
            .is_none_or(|current| reading.height >= current.height)
        {
            self.reading = Some(reading);
        }
    }
}

/// What a cache read returned, and whether this call went to the backend
/// for it (successfully or not) rather than serving the cache, so a caller
/// wanting a fresher reading knows a second probe would only repeat the
/// first.
#[derive(Debug, Clone)]
pub(crate) struct TipRead {
    /// `None` only when no reading has ever succeeded.
    pub(crate) state: Option<TipState>,
    pub(crate) probed: bool,
}

impl TipCache {
    fn lock_state(&self) -> MutexGuard<'_, TipCacheState> {
        // This is advisory state: retaining the last snapshot after a panic
        // is preferable to failing every subsequent transaction submission.
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Current reading, refreshing it through `fetch` once `window` has
    /// lapsed since the last attempt.
    pub(crate) async fn reading<Fut>(
        &self,
        window: Duration,
        probe_timeout: Duration,
        context: RpcRequestContext,
        fetch: impl FnOnce(RpcRequestContext) -> Fut,
    ) -> TipRead
    where
        Fut: Future<Output = Result<TipReading, RpcServerError>>,
    {
        let now = Instant::now();
        // `Some` when this caller waits on a probe another caller claimed;
        // `None` when it claimed the window itself. Decided under the lock
        // and released before any await, since the probe locks again.
        let waiter = {
            let mut state = self.lock_state();
            match state.last_attempt {
                Some(at) if now.duration_since(at) < window => {
                    if state.reading.is_some() || !state.in_flight {
                        return TipRead {
                            state: state.reading.clone(),
                            probed: false,
                        };
                    }
                    // Subscribed under the lock, so the probe cannot finish
                    // before the wait begins.
                    Some(self.inner.probe_done.subscribe())
                }
                _ => {
                    state.last_attempt = Some(now);
                    state.in_flight = true;
                    None
                }
            }
        };
        let Some(mut probe_done) = waiter else {
            return self.probe(probe_timeout, context, fetch).await;
        };

        // A reading obtained this way is as fresh as one this caller probed
        // for itself, so it counts as probed.
        let _ = tokio::time::timeout(probe_timeout, probe_done.changed()).await;
        TipRead {
            state: self.lock_state().reading.clone(),
            probed: true,
        }
    }

    /// Run the probe this caller claimed the window for, then release any
    /// cold-cache waiters.
    async fn probe<Fut>(
        &self,
        probe_timeout: Duration,
        context: RpcRequestContext,
        fetch: impl FnOnce(RpcRequestContext) -> Fut,
    ) -> TipRead
    where
        Fut: Future<Output = Result<TipReading, RpcServerError>>,
    {
        let probe_context = context.capped(probe_timeout);
        let outcome = match probe_context.remaining() {
            Some(remaining) => Some(tokio::time::timeout(remaining, fetch(probe_context)).await),
            None => None,
        };
        // Conversion and logging can be slow; neither may hold up cache readers.
        let reading = match &outcome {
            Some(Ok(Ok(reading))) => Some(TipState::from_reading(reading)),
            Some(Ok(Err(error))) => {
                tracing::warn!(%error, "chain tip unavailable, pre-checks use last reading");
                None
            }
            Some(Err(_)) => {
                tracing::warn!("chain tip probe timed out, pre-checks use last reading");
                None
            }
            None => None,
        };
        let state = {
            let mut state = self.lock_state();
            if let Some(reading) = reading {
                state.record(reading);
            }
            state.in_flight = false;
            state.reading.clone()
        };
        self.inner.probe_done.send_replace(());
        TipRead {
            state,
            probed: outcome.is_some(),
        }
    }
}

/// Aborts a background task when dropped.
#[derive(Debug)]
pub(crate) struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start the task that keeps `cache` fresh, on a fixed schedule so a slow
/// probe cannot push later refreshes out. Each tick probes
/// unconditionally. Returns `None` outside a Tokio runtime, or when the
/// interval is zero, which also disables reuse.
pub(crate) fn spawn_tip_refresh<C: ChainPlane>(
    chain: Arc<C>,
    cache: TipCache,
    config: &RpcServerConfig,
) -> Option<AbortOnDrop> {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::debug!("no tokio runtime, chain tip refresh task not started");
        return None;
    };
    if config.tip_refresh_interval_ms == 0 {
        tracing::debug!("tip refresh interval is zero, every pre-check fetches its own tip");
        return None;
    }
    let interval = Duration::from_millis(config.tip_refresh_interval_ms);
    let probe_timeout = Duration::from_millis(config.tip_probe_timeout_ms);
    let task = handle.spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // An overrunning probe resumes on schedule rather than firing the
        // ticks it missed back to back.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `interval` yields its first tick immediately; the task's own
        // first probe belongs one interval in, as `new` documents.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            cache
                .reading(
                    Duration::ZERO,
                    probe_timeout,
                    RpcRequestContext::unbounded(),
                    |context| chain.chain_tip(context),
                )
                .await;
        }
    });
    Some(AbortOnDrop(task.abort_handle()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_types::base58::Base58;
    use thruput_json_rpc_types::block::ChainTip;
    use thruput_xdr::network_config::{NetworkConfig, NetworkConfigV0};
    use xdr_common::VecM;

    /// A reading whose network config states no parameters.
    fn tip_reading(height: i64) -> TipReading {
        TipReading::new(
            ChainTip {
                height: height.into(),
                block_hash: Base58(thruput_xdr::types::SHA256Hash::from([0u8; 32])),
            },
            NetworkConfig::_0 {
                v0: NetworkConfigV0 {
                    params: VecM(vec![]),
                },
            },
        )
    }

    async fn probe(
        cache: &TipCache,
        window: Duration,
        result: Result<TipReading, RpcServerError>,
    ) -> TipRead {
        cache
            .reading(
                window,
                Duration::from_secs(1),
                RpcRequestContext::unbounded(),
                |_| async { result },
            )
            .await
    }

    #[tokio::test]
    async fn an_overlapping_fetch_that_lands_late_does_not_lower_the_tip() {
        let cache = TipCache::default();
        probe(&cache, Duration::ZERO, Ok(tip_reading(10))).await;

        let read = probe(&cache, Duration::ZERO, Ok(tip_reading(9))).await;

        assert!(read.probed);
        assert_eq!(read.state.map(|tip| tip.height), Some(10));
    }

    /// A backend that has been failing for a while keeps having probes
    /// claimed against it; the advisory path still answers from the last
    /// reading it obtained.
    #[tokio::test(start_paused = true)]
    async fn a_failing_probe_keeps_the_last_reading() {
        let cache = TipCache::default();
        probe(&cache, Duration::ZERO, Ok(tip_reading(7))).await;

        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(10)).await;
            let read = probe(
                &cache,
                Duration::ZERO,
                Err(RpcServerError::BackendUnavailable("down".into())),
            )
            .await;
            assert!(read.probed);
            assert_eq!(read.state.map(|tip| tip.height), Some(7));
        }

        let read = probe(&cache, Duration::from_mins(1), Ok(tip_reading(8))).await;
        assert!(
            !read.probed,
            "a recent failed attempt still owns the window"
        );
        assert_eq!(read.state.map(|tip| tip.height), Some(7));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_cold_readers_share_one_probe() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = TipCache::default();
        let probes = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let mut readers = tokio::task::JoinSet::new();
        for _ in 0..128 {
            let cache = cache.clone();
            let probes = probes.clone();
            let release = release.clone();
            readers.spawn(async move {
                cache
                    .reading(
                        Duration::from_mins(1),
                        Duration::from_mins(5),
                        RpcRequestContext::unbounded(),
                        |_| async {
                            probes.fetch_add(1, Ordering::SeqCst);
                            release.notified().await;
                            Ok(tip_reading(42))
                        },
                    )
                    .await
            });
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            // All other readers must be waiting before the probe completes,
            // so this test depends on the notification rather than a warm hit.
            while cache.inner.probe_done.receiver_count() != 127 {
                tokio::task::yield_now().await;
            }
            release.notify_one();
            while let Some(read) = readers.join_next().await {
                assert_eq!(read.unwrap().state.unwrap().height, 42);
            }
        })
        .await
        .expect("cold readers should wake when the shared probe completes");
        assert_eq!(probes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_cold_probe_does_not_stall_later_refreshes() {
        let cache = TipCache::default();
        let probe_cache = cache.clone();
        let (started, observed) = tokio::sync::oneshot::channel();
        let probe = tokio::spawn(async move {
            probe_cache
                .reading(
                    Duration::from_secs(10),
                    Duration::from_secs(2),
                    RpcRequestContext::unbounded(),
                    |_| async {
                        started.send(()).unwrap();
                        std::future::pending().await
                    },
                )
                .await
        });
        observed.await.unwrap();
        probe.abort();
        assert!(probe.await.unwrap_err().is_cancelled());
        let read = cache
            .reading(
                Duration::from_secs(10),
                Duration::from_secs(2),
                RpcRequestContext::unbounded(),
                |_| async { panic!("cancelled probe should still own the reuse window") },
            )
            .await;
        assert!(read.state.is_none());
        tokio::time::advance(Duration::from_secs(10)).await;
        let read = cache
            .reading(
                Duration::from_secs(10),
                Duration::from_secs(2),
                RpcRequestContext::unbounded(),
                |_| async { Ok(tip_reading(43)) },
            )
            .await;
        assert!(read.probed);
        assert_eq!(read.state.unwrap().height, 43);
    }
}
