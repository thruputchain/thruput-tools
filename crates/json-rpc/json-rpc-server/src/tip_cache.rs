//! Shared db-tip reading: what the `sendRawTransaction` pre-checks and
//! `chainConfig` read instead of fetching a tip each time, and the
//! background task that keeps it current.

use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use thruput_json_rpc_types::network_config::ExecutionConfig;
use thruput_tx_validation::NetworkConfigParams;

use crate::backend::{ChainPlane, RpcRequestContext, TipReading};
use crate::config::RpcServerConfig;
use crate::error::RpcServerError;

/// What one db-tip fetch resolved, so each later access is a field read.
#[derive(Debug, Clone)]
pub(crate) struct TipState {
    pub(crate) height: i64,
    /// `None` when the tip's network config is a version this build cannot
    /// interpret: chain-configured pre-checks are then skipped and
    /// `chainConfig` reports the config as unreadable rather than empty.
    pub(crate) params: Option<NetworkConfigParams>,
    /// Effective limits and storage prices read with this tip, `None` when
    /// the backend did not supply them. Shared, so storing a reading and
    /// serving `chainConfig` from it never copy the storage schedule.
    pub(crate) execution: Option<Arc<ExecutionConfig>>,
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
            execution: reading.execution.clone(),
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
///
/// Those allowances suit an advisory pre-check. A caller that must not act
/// on a lapsed reading asks [`Self::fresh`] and reads the tip itself when
/// it answers `None`.
#[derive(Debug, Clone)]
pub(crate) struct TipCache {
    inner: Arc<TipCacheInner>,
}

#[derive(Debug)]
struct TipCacheInner {
    state: tokio::sync::Mutex<TipCacheState>,
    /// Signalled when a probe finishes, whatever its outcome, so cold-cache
    /// waiters can re-read the state.
    probe_done: tokio::sync::watch::Sender<()>,
}

impl Default for TipCache {
    fn default() -> Self {
        let (probe_done, _) = tokio::sync::watch::channel(());
        Self {
            inner: Arc::new(TipCacheInner {
                state: tokio::sync::Mutex::default(),
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
    /// When `reading` was obtained. Distinct from `last_attempt` because a
    /// failed probe moves the attempt forward while the reading it failed
    /// to replace keeps ageing; a caller asking whether the reading is
    /// fresh has to see that age, not the attempt.
    reading_at: Option<Instant>,
    /// A probe claimed the current window and has not reported back. Stays
    /// set if that probe's request is cancelled mid-flight; cold-cache
    /// waiters therefore give up after the probe timeout rather than trust
    /// it, and the next claimed window resets it.
    in_flight: bool,
}

impl TipCacheState {
    /// Keep `reading` unless a newer one is already stored; says whether it
    /// was kept.
    fn record(&mut self, reading: TipState) -> bool {
        let newer = self
            .reading
            .as_ref()
            .is_none_or(|current| reading.height >= current.height);
        if newer {
            self.reading = Some(reading);
            self.reading_at = Some(Instant::now());
        }
        newer
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
    /// Record a reading obtained by any path, restarting the reuse window
    /// when it is the newest.
    pub(crate) async fn store(&self, reading: TipState) {
        let mut state = self.inner.state.lock().await;
        if state.record(reading) {
            state.last_attempt = Some(Instant::now());
        }
    }

    /// Reading from within `window`, without going to the backend.
    ///
    /// `None` when nothing is stored or the stored reading has lapsed, so a
    /// caller that must not answer from a lapsed reading can read the tip
    /// itself and let a failure surface. Unlike [`Self::reading`] this
    /// never falls back to an older reading, and it ages the reading itself
    /// rather than the last attempt, so a backend that has been failing for
    /// an hour cannot present an hour-old price as current.
    pub(crate) async fn fresh(&self, window: Duration) -> Option<TipState> {
        let state = self.inner.state.lock().await;
        let at = state.reading_at?;
        if Instant::now().duration_since(at) >= window {
            return None;
        }
        state.reading.clone()
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
            let mut state = self.inner.state.lock().await;
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
            state: self.inner.state.lock().await.reading.clone(),
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
        let state = {
            let mut state = self.inner.state.lock().await;
            match &outcome {
                Some(Ok(Ok(reading))) => {
                    state.record(TipState::from_reading(reading));
                }
                Some(Ok(Err(error))) => {
                    tracing::warn!(%error, "chain tip unavailable, pre-checks use last reading");
                }
                Some(Err(_)) => {
                    tracing::warn!("chain tip probe timed out, pre-checks use last reading");
                }
                None => {}
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

    #[tokio::test]
    async fn an_overlapping_fetch_that_lands_late_does_not_lower_the_tip() {
        let cache = TipCache::default();
        cache.store(TipState::from_reading(&tip_reading(10))).await;

        let read = cache
            .reading(
                Duration::ZERO,
                Duration::from_secs(1),
                RpcRequestContext::unbounded(),
                |_| async { Ok(tip_reading(9)) },
            )
            .await;

        assert!(read.probed);
        assert_eq!(read.state.map(|tip| tip.height), Some(10));
        cache.store(TipState::from_reading(&tip_reading(9))).await;
        assert_eq!(
            cache.fresh(Duration::from_mins(1)).await.map(|t| t.height),
            Some(10)
        );
    }

    #[tokio::test]
    async fn fresh_declines_a_reading_that_has_lapsed() {
        let cache = TipCache::default();
        cache.store(TipState::from_reading(&tip_reading(4))).await;

        assert!(
            cache.fresh(Duration::from_mins(1)).await.is_some(),
            "a reading inside the window is served without a probe"
        );
        assert!(
            cache.fresh(Duration::ZERO).await.is_none(),
            "a lapsed reading is withheld so the caller reads the tip itself"
        );
    }

    /// A backend that has been failing for a while keeps having probes
    /// claimed against it. Those attempts must not present the reading they
    /// failed to replace as current.
    #[tokio::test(start_paused = true)]
    async fn a_failing_probe_does_not_make_a_stale_reading_look_fresh() {
        let cache = TipCache::default();
        cache.store(TipState::from_reading(&tip_reading(7))).await;

        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(10)).await;
            cache
                .reading(
                    Duration::ZERO,
                    Duration::from_secs(1),
                    RpcRequestContext::unbounded(),
                    |_| async { Err(RpcServerError::BackendUnavailable("down".into())) },
                )
                .await;
        }

        assert_eq!(
            cache
                .reading(
                    Duration::from_mins(1),
                    Duration::from_secs(1),
                    RpcRequestContext::unbounded(),
                    |_| async { Ok(tip_reading(8)) },
                )
                .await
                .state
                .map(|tip| tip.height),
            Some(7),
            "the advisory path still falls back to the last reading"
        );
        assert!(
            cache.fresh(Duration::from_secs(30)).await.is_none(),
            "but the reading is 40s old, and a recent failed attempt is not a recent reading"
        );
    }

    #[tokio::test]
    async fn fresh_is_none_until_a_reading_lands() {
        let cache = TipCache::default();
        assert!(cache.fresh(Duration::from_mins(1)).await.is_none());
    }
}
