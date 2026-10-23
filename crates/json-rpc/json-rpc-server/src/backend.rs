//! Shared backend context and the chain plane.
//!
//! [`RpcRequestContext`] carries one request deadline to every backend call.
//! [`ChainPlane`] reads the committed tip with its network config, which the
//! native read service and the submission pre-checks share, plus the local
//! pending count. Other backend planes are native and live under
//! [`crate::native`].
//!
//! Adapters translate hogd's x23 outcome-tagged results into these shapes, so
//! wire-format concerns stay out of the JSON-RPC layer. Methods a backend
//! cannot answer return `RpcServerError::NotImplemented`.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thruput_json_rpc_types::{BlockHeight, block::ChainTip, transaction::TxHash};

use thruput_xdr::network_config::NetworkConfig;

use crate::error::RpcServerError;

/// Absolute timing budget for one public JSON-RPC request.
#[derive(Debug, Clone, Copy)]
pub struct RpcRequestContext {
    started_at: Instant,
    deadline: Option<Instant>,
}

impl RpcRequestContext {
    /// Create a context with an optional end-to-end timeout.
    pub fn new(timeout: Option<Duration>) -> Self {
        let started_at = Instant::now();
        Self {
            started_at,
            deadline: timeout.and_then(|timeout| started_at.checked_add(timeout)),
        }
    }

    /// Create a context without an end-to-end deadline.
    pub fn unbounded() -> Self {
        Self::new(None)
    }

    /// Return the instant when this request started.
    pub fn started_at(self) -> Instant {
        self.started_at
    }

    /// Return the absolute request deadline, when configured.
    pub fn deadline(self) -> Option<Instant> {
        self.deadline
    }

    /// Return the remaining request budget, when configured.
    pub fn remaining(self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// Restrict this context to a shorter operation-specific timeout.
    #[must_use]
    pub fn capped(self, timeout: Duration) -> Self {
        let cap = Instant::now().checked_add(timeout);
        self.with_cap(cap)
    }

    /// Restrict this context to a timeout measured from request start.
    #[must_use]
    pub fn capped_from_start(self, timeout: Duration) -> Self {
        let cap = self.started_at.checked_add(timeout);
        self.with_cap(cap)
    }

    fn with_cap(self, cap: Option<Instant>) -> Self {
        Self {
            started_at: self.started_at,
            deadline: match (self.deadline, cap) {
                (Some(request), Some(cap)) => Some(request.min(cap)),
                (Some(request), None) => Some(request),
                (None, cap) => cap,
            },
        }
    }
}

/// A transaction that has passed shared RPC-layer validation.
///
/// Obtain instances via `tx::validate_raw_transaction_bytes`.
/// No public constructor is provided so callers do not bypass validation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ValidatedTransaction {
    /// Raw signed transaction XDR bytes supplied by the client.
    pub bytes: Vec<u8>,
    /// Canonical transaction hash over the inner `Transaction`.
    pub hash: TxHash,
    /// Last block height at which the transaction may be included,
    /// decoded from the transaction's uniquifier.
    pub block_expiry_height: BlockHeight,
}

/// One reading of the committed db tip: the public chain tip together with
/// the execution parameters in force for the block that follows it.
///
/// hogd returns these in a single `dbgateway_get_config` reply, and the
/// shared server refreshes its cached network config from every reading it
/// receives, so a backend never has to serve the config separately.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TipReading {
    /// Chain state that advances with each commit; what `getChainTip` returns.
    pub tip: ChainTip,
    /// Network config the tip carries.
    pub network_config: NetworkConfig,
    /// Effective limits and storage prices read with this committed tip.
    ///
    /// `getChainConfig` projects its execution limits and prices from this.
    pub execution: Option<Arc<thruput_json_rpc_types::network_config::ExecutionConfig>>,
}

impl TipReading {
    /// Pair a chain tip with the network config read alongside it.
    pub fn new(tip: ChainTip, network_config: NetworkConfig) -> Self {
        Self {
            tip,
            network_config,
            execution: None,
        }
    }

    /// Attach the execution rules returned with this tip.
    #[must_use]
    pub fn with_execution(
        mut self,
        execution: impl Into<Arc<thruput_json_rpc_types::network_config::ExecutionConfig>>,
    ) -> Self {
        self.execution = Some(execution.into());
        self
    }
}

/// Backend plane for the committed chain tip and the pending transaction count.
#[async_trait]
pub trait ChainPlane: Send + Sync + 'static {
    /// Return the current chain tip and the network config it carries.
    async fn chain_tip(&self, context: RpcRequestContext) -> Result<TipReading, RpcServerError>;

    /// Return the number of currently pending transactions.
    async fn pending_transaction_count(
        &self,
        context: RpcRequestContext,
    ) -> Result<u64, RpcServerError>;
}
