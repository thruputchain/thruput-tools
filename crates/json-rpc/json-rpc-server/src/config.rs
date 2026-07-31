/// Default gas budget for backend `groundhog_view` execution.
pub const DEFAULT_VIEW_GAS_LIMIT: u64 = 10_000_000;

/// Default page size selected by the indexer when a transaction query uses 0.
pub const DEFAULT_TRANSACTION_PAGE_LIMIT: u32 = 50;

/// Smallest valid configured transaction-page cap.
pub const MIN_TRANSACTION_PAGE_LIMIT: u32 = DEFAULT_TRANSACTION_PAGE_LIMIT;

/// Protocol-wide maximum transaction-page size.
pub const MAX_TRANSACTION_PAGE_LIMIT: u32 = 200;

/// Invalid shared JSON-RPC server configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RpcServerConfigError {
    /// The transaction-page cap cannot represent the indexer default or exceeds
    /// the protocol ceiling.
    #[error(
        "max transactions per page must be between {MIN_TRANSACTION_PAGE_LIMIT} and {MAX_TRANSACTION_PAGE_LIMIT}, got {0}"
    )]
    InvalidTransactionPageLimit(u32),
}

/// Server-wide bounds on JSON-RPC requests.  The shared dispatcher
/// rejects requests that exceed these before they reach a backend.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RpcServerConfig {
    /// Optional end-to-end deadline for one public JSON-RPC method.
    ///
    /// The deadline starts before shared validation and is passed explicitly to
    /// backend planes. `None` preserves the embedding application's own policy.
    pub request_timeout_ms: Option<u64>,

    /// Per-page cap on `getEvents` responses; requests with `limit`
    /// above this are rejected as `InvalidParam`.
    pub max_events_per_page: u32,

    /// Per-page cap on transaction-ref responses; requests with `limit` above
    /// this are rejected as `InvalidParam`.
    pub max_transactions_per_page: u32,

    /// Maximum length of the opaque pagination cursor on paginated
    /// methods; longer cursors are rejected as `InvalidParam`.
    pub max_cursor_bytes: usize,

    /// Maximum `groundhog_view` calldata byte length; larger
    /// requests are rejected as `InvalidParam` before backend dispatch.
    pub max_view_calldata_bytes: usize,

    /// Gas budget assigned to backend `groundhog_view` execution.
    ///
    /// The shared server validates JSON request shape; concrete backends
    /// pass this cap into the hogd view invocation once view execution is
    /// wired. Keep the default bounded so a view-capable backend cannot
    /// accidentally expose an unbounded root invocation.
    pub view_gas_limit: u64,

    /// How long a chain-tip reading is reused by the `sendRawTransaction`
    /// expiry pre-check before the backend is asked again. Bounds the
    /// extra backend load to one tip probe per window regardless of
    /// submission rate. Stale readings are safe: the tip only grows, so
    /// staleness can only under-detect expiry, never falsely reject.
    pub tip_cache_ttl_ms: u64,

    /// Upper bound on a single chain-tip probe issued by the expiry
    /// pre-check. The pre-check is advisory, so a slow or stalled tip
    /// backend must not delay submission past this bound.
    pub tip_probe_timeout_ms: u64,
}

impl Default for RpcServerConfig {
    fn default() -> Self {
        Self {
            request_timeout_ms: None,
            max_events_per_page: 200,
            max_transactions_per_page: 200,
            max_cursor_bytes: 256,
            max_view_calldata_bytes: 64 * 1024,
            view_gas_limit: DEFAULT_VIEW_GAS_LIMIT,
            tip_cache_ttl_ms: 3_000,
            tip_probe_timeout_ms: 2_000,
        }
    }
}

impl RpcServerConfig {
    /// Validate bounds that embedding applications must check before serving.
    pub fn validate(&self) -> Result<(), RpcServerConfigError> {
        if !(MIN_TRANSACTION_PAGE_LIMIT..=MAX_TRANSACTION_PAGE_LIMIT)
            .contains(&self.max_transactions_per_page)
        {
            return Err(RpcServerConfigError::InvalidTransactionPageLimit(
                self.max_transactions_per_page,
            ));
        }
        Ok(())
    }

    /// Set an end-to-end timeout for every public JSON-RPC request.
    #[must_use]
    pub fn with_request_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.request_timeout_ms = Some(timeout_ms.max(1));
        self
    }

    /// Override the per-page event limit.
    #[must_use]
    pub fn with_max_events_per_page(mut self, max: u32) -> Self {
        self.max_events_per_page = max;
        self
    }

    /// Override the per-page transaction-ref limit.
    #[must_use]
    pub fn with_max_transactions_per_page(mut self, max: u32) -> Self {
        self.max_transactions_per_page = max;
        self
    }

    /// Override the cursor byte length cap.
    #[must_use]
    pub fn with_max_cursor_bytes(mut self, max: usize) -> Self {
        self.max_cursor_bytes = max;
        self
    }

    /// Override the view calldata byte length cap.
    #[must_use]
    pub fn with_max_view_calldata_bytes(mut self, max: usize) -> Self {
        self.max_view_calldata_bytes = max;
        self
    }

    /// Override the view execution gas limit.
    #[must_use]
    pub fn with_view_gas_limit(mut self, gas_limit: u64) -> Self {
        self.view_gas_limit = gas_limit;
        self
    }

    /// Override how long the expiry pre-check reuses a chain-tip reading.
    #[must_use]
    pub fn with_tip_cache_ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.tip_cache_ttl_ms = ttl_ms;
        self
    }

    /// Override the expiry pre-check's per-probe timeout.
    #[must_use]
    pub fn with_tip_probe_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.tip_probe_timeout_ms = timeout_ms;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_page_cap_accepts_protocol_range() {
        for limit in [MIN_TRANSACTION_PAGE_LIMIT, MAX_TRANSACTION_PAGE_LIMIT] {
            assert!(
                RpcServerConfig::default()
                    .with_max_transactions_per_page(limit)
                    .validate()
                    .is_ok()
            );
        }
    }

    #[test]
    fn transaction_page_cap_rejects_values_outside_protocol_range() {
        for limit in [
            MIN_TRANSACTION_PAGE_LIMIT - 1,
            MAX_TRANSACTION_PAGE_LIMIT + 1,
        ] {
            assert_eq!(
                RpcServerConfig::default()
                    .with_max_transactions_per_page(limit)
                    .validate(),
                Err(RpcServerConfigError::InvalidTransactionPageLimit(limit))
            );
        }
    }
}
