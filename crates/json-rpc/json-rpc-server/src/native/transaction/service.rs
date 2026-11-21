//! Native transaction lifecycle and the db-tip cache behind its submission
//! pre-checks.

mod lifecycle;

use std::sync::Arc;
use std::time::Duration;

use crate::tip_cache::{AbortOnDrop, TipCache, TipState, spawn_tip_refresh};

use crate::{ChainPlane, RpcRequestContext, RpcServerConfig};

/// Native transaction lifecycle over independently-typed submit, chain, and
/// read backend planes: `sendTransaction`, `getTransactionStatus`, and
/// `getTransactionReceipt` behind [`crate::native::service::NativeService`].
///
/// Holds a cached reading of the db tip (height plus the resolved network
/// config) that every submission pre-check consults. The background task
/// started by [`Self::new`] keeps it current, so a submit reads what that
/// task published.
#[derive(Debug)]
pub struct NativeTransactionService<S, C, R> {
    submit: S,
    chain: Arc<C>,
    read: R,
    config: RpcServerConfig,
    tip_cache: TipCache,
    /// Aborts the background refresh when the service is dropped, which
    /// for a served one is when jsonrpsee drops the module holding it.
    _tip_refresh: Option<AbortOnDrop>,
}

impl<S, C, R> NativeTransactionService<S, C, R> {
    fn request_context(&self) -> RpcRequestContext {
        RpcRequestContext::new(self.config.request_timeout_ms.map(Duration::from_millis))
    }
}

impl<S, C, R> NativeTransactionService<S, C, R>
where
    C: ChainPlane,
{
    /// Create the service over independent backend planes.
    ///
    /// When called inside a Tokio runtime this also starts the background
    /// task that refreshes the cached db-tip reading every
    /// `tip_refresh_interval_ms`; it stops when the service is dropped. An
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
    /// submit pre-checks. Returns the last successful reading, if any.
    async fn cached_tip_state(&self, context: RpcRequestContext) -> Option<TipState> {
        self.tip_cache
            .reading(
                self.tip_reuse_window(),
                Duration::from_millis(self.config.tip_probe_timeout_ms),
                context,
                |probe_context| self.chain.chain_tip(probe_context),
            )
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
}

#[cfg(test)]
mod tests;
