mod transaction;

use std::sync::Arc;
use std::time::Duration;

use crate::tip_cache::{AbortOnDrop, TipCache, TipRead, spawn_tip_refresh};

use crate::{ChainPlane, RpcRequestContext, RpcServerConfig};

/// Native transaction lifecycle over independently-typed submit, chain, and
/// read backend planes: `sendTransaction`, `getTransactionStatus`, and
/// `getTransactionReceipt` behind [`crate::native::service::NativeService`].
///
/// Holds a cached reading of the db tip (height plus the resolved network
/// config) that every submission pre-check consults. The background task
/// started by [`Self::new`] keeps it current, so a submit reads what that
/// task published; a bound re-check still fetches for itself.
#[derive(Debug)]
pub struct GroundhogRpcServer<S, C, R> {
    submit: S,
    chain: Arc<C>,
    read: R,
    config: RpcServerConfig,
    tip_cache: TipCache,
    /// Aborts the background refresh when the server is dropped, which
    /// for a served one is when jsonrpsee drops the module holding it.
    _tip_refresh: Option<AbortOnDrop>,
}

impl<S, C, R> GroundhogRpcServer<S, C, R> {
    fn request_context(&self) -> RpcRequestContext {
        RpcRequestContext::new(self.config.request_timeout_ms.map(Duration::from_millis))
    }
}

impl<S, C, R> GroundhogRpcServer<S, C, R>
where
    C: ChainPlane,
{
    /// Create a server wrapper around independent backend planes.
    ///
    /// When called inside a Tokio runtime this also starts the background
    /// task that refreshes the cached db-tip reading every
    /// `tip_refresh_interval_ms`; it stops when the server is dropped. An
    /// interval of zero starts no task and makes every pre-check fetch its
    /// own reading.
    pub fn new(submit: S, chain: C, read: R, config: RpcServerConfig) -> Self {
        let chain = Arc::new(chain);
        let tip_cache = TipCache::default();
        let tip_refresh = spawn_tip_refresh(chain.clone(), tip_cache.clone(), &config);
        Self {
            submit,
            chain,
            read,
            config,
            tip_cache,
            _tip_refresh: tip_refresh,
        }
    }

    /// Db-tip reading served from the shared rate-limited cache, for the
    /// submit pre-checks. Its state is `None` when no
    /// reading is available (probe failed or timed out with no prior
    /// success); a submit then runs only the checks that need no chain
    /// reading.
    async fn cached_tip_state(&self, context: RpcRequestContext) -> TipRead {
        self.tip_state_within(self.tip_reuse_window(), context)
            .await
    }

    /// One refresh interval plus the longest a probe may take to publish:
    /// older than that means the task is not keeping up, so the caller
    /// fetches its own. Zero when refreshing is disabled.
    fn tip_reuse_window(&self) -> Duration {
        if self.config.tip_refresh_interval_ms == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(
            self.config
                .tip_refresh_interval_ms
                .saturating_add(self.config.tip_probe_timeout_ms),
        )
    }

    /// Db-tip reading fetched now regardless of the cache window, for a
    /// decision that must not rest on a reading that may lag the chain.
    async fn fresh_tip_state(&self, context: RpcRequestContext) -> TipRead {
        self.tip_state_within(Duration::ZERO, context).await
    }

    async fn tip_state_within(&self, window: Duration, context: RpcRequestContext) -> TipRead {
        self.tip_cache
            .reading(
                window,
                Duration::from_millis(self.config.tip_probe_timeout_ms),
                context,
                |probe_context| self.chain.chain_tip(probe_context),
            )
            .await
    }
}

#[cfg(test)]
mod tests;
