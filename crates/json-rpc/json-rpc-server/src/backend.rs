//! Backend traits for the shared JSON-RPC server.
//!
//! # Design intent
//!
//! These traits speak the JSON-RPC consumer's mental model — per-call,
//! `Option<T>` for missing data, semantic enums for outcomes — not the XDR
//! wire shape used by hogd's x23 services. The protocol-rs surface
//! batches submission (`ParallelSignedTransactionBatch`), paginates events
//! (`EventQuery` / `EventPage`), and tags every read response with an
//! outcome enum (`TxInfoResult`, `SchedulerGetTipResult`, ...). Adapters
//! collapse those to the trait shapes here. Surfacing the XDR shape on the
//! traits would push wire-format concerns into JSON-RPC handler code and
//! is explicitly avoided.
//!
//! # Plane split vs backend topology
//!
//! The XDR backend cleaves into scheduler and indexer x23 services. The
//! plane split is Submit / Chain / Read. The two do not align cleanly:
//!
//! - `SubmitPlane`                            → scheduler
//! - `ChainPlane::chain_tip`                  → scheduler
//! - `ChainPlane::get_block_by_number`        → indexer
//! - `ChainPlane::pending_transaction_count`  → scheduler
//! - `ReadPlane::transaction_receipt`         → indexer
//! - `ReadPlane::transaction_status`          → **both** (scheduler for
//!   `Pending`, indexer for `Committed`) — the adapter owns this
//!   orchestration; the shared server calls the trait method once.
//! - `ReadPlane::contract_events`             → indexer
//! - `ReadPlane::get_storage_at`              → `DbGateway1` contract-data reads
//! - `ReadPlane::read_native_hog_balance`     → `DbGateway1` free native-token balance reads
//! - `ReadPlane::read_address_auth_data`      → `DbGateway1` address auth-data reads
//! - `ViewPlane::view`                        → view executor service (TBD)
//!
//! `GroundhogRpcServer::new` accepts per-plane adapter types, so a future
//! composition (e.g. the hogd-dev supervisor wiring distinct x23 clients)
//! can supply different concrete types per plane. `transaction_status`
//! will still need scheduler + indexer reach regardless of which plane
//! conceptually owns it.
//!
//! # Capability divergence is `NotImplemented`, for now
//!
//! Methods a given backend cannot answer return
//! `RpcServerError::NotImplemented`. Today this is rare
//! (`pendingTransactionCount` on gateway, `getBlockByNumber` on the
//! current hogd-dev node supervisor). If the trait accumulates several more
//! capability asymmetries, a richer capability descriptor — or an `Option<T>`
//! return mapped to `NotImplemented` in the shared server — may earn its keep.
//! Until then, concrete error returns are simpler than typed capability
//! checks.
//!
//! # Known places the trait shape will need to grow
//!
//! - `chain_tip` cannot distinguish "chain not yet initialized" from a
//!   real tip. Production never hits the case; pre-genesis hogd-dev will.
//!   Add a dedicated error variant or sentinel when wiring that backend.
//!
//! # Things to leave alone
//!
//! - Per-call submit shape. Batching is a gateway-internal performance
//!   concern; future single-shot backends call `submit_unvetted` with a
//!   one-element batch.
//! - `Option<T>` / semantic enums on read paths. Translating XDR outcome-
//!   tag enums once per call inside the adapter is correct; pushing those
//!   tags into the trait would couple the JSON-RPC presentation layer to
//!   the wire protocol.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thruput_json_rpc_types::{
    BlockHeight, ContractAddress,
    address_auth::AddressAuthDataResponse,
    block::{BlockInfo, ChainTip},
    events::{EventPage, EventQuery},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
    view::{ViewRequest, ViewResponse},
};

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

/// Backend plane for accepted raw transaction submission.
#[async_trait]
pub trait SubmitPlane: Send + Sync + 'static {
    /// Submit one validated signed transaction.
    async fn submit_raw_transaction(
        &self,
        context: RpcRequestContext,
        tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError>;
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
    /// Chain state that advances with each commit; what `chainTip` returns.
    pub tip: ChainTip,
    /// Network config the tip carries.
    pub network_config: NetworkConfig,
    /// Effective limits and storage prices read with this committed tip.
    ///
    /// Shared from the moment the backend builds it, so caching the reading
    /// and answering `chainConfig` from it both cost a refcount.
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

/// Backend plane for chain metadata and block-oriented reads.
#[async_trait]
pub trait ChainPlane: Send + Sync + 'static {
    /// Return the current chain tip and the network config it carries.
    async fn chain_tip(&self, context: RpcRequestContext) -> Result<TipReading, RpcServerError>;

    /// Return block data at a specific height, if available.
    async fn get_block_by_number(
        &self,
        context: RpcRequestContext,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError>;

    /// Return the number of currently pending transactions.
    async fn pending_transaction_count(
        &self,
        context: RpcRequestContext,
    ) -> Result<u64, RpcServerError>;
}

/// Backend plane for transaction, event, and storage reads.
#[async_trait]
pub trait ReadPlane: Send + Sync + 'static {
    /// Return a committed transaction receipt, if available.
    async fn transaction_receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError>;

    /// Return semantic transaction status.
    ///
    /// Backends own any source fallback needed to answer this. For example,
    /// the production gateway checks scheduler pending state before indexer
    /// committed state.
    async fn transaction_status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError>;

    /// Return one page of events matching `query`.
    ///
    /// Walk further pages by sending the same query back with `cursor`
    /// set to the value from the previous page.
    async fn contract_events(
        &self,
        context: RpcRequestContext,
        query: EventQuery,
    ) -> Result<EventPage, RpcServerError>;

    /// Return one page of transaction refs from the history of the
    /// query's address.
    ///
    /// Walk further pages by sending the same query back with `cursor`
    /// set to the value from the previous page.
    async fn transactions_by_address(
        &self,
        context: RpcRequestContext,
        query: AddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError>;

    /// Read hash-ordered transaction references for one committed height.
    /// Backends without an indexer report unavailability, never an empty page.
    async fn transactions_by_block(
        &self,
        _context: RpcRequestContext,
        _query: thruput_json_rpc_types::transaction::BlockTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        Err(RpcServerError::BackendUnavailable(
            "block query indexer is unavailable".into(),
        ))
    }

    /// Return the latest committed storage item at one (address, key).
    /// `key` is the 32-byte raw contract-data key that the wire form
    /// has already validated as exactly 32 bytes.  The returned
    /// `StorageItemRead` always carries `block_height` (even when no
    /// item is present at the key) so callers can compare against
    /// `chainTip` to gauge freshness.
    async fn get_storage_at(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError>;

    /// Return the latest committed native HOG balance item for one address.
    /// This reads hogd's `FreeNativeTokenBalance` key, not contract storage.
    async fn read_native_hog_balance(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError>;

    /// Return the latest committed authorization configuration for one
    /// address. This reads hogd's `AuthData` key, not contract storage. An
    /// address with no registered configuration is a successful read with
    /// `auth_data: None`, still labeled with the snapshot height.
    async fn read_address_auth_data(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<AddressAuthDataResponse, RpcServerError>;
}

/// Backend plane for sandboxed contract simulation.
#[async_trait]
pub trait ViewPlane: Send + Sync + 'static {
    /// Execute independent calls on one retained committed snapshot.
    async fn view_batch(
        &self,
        _context: RpcRequestContext,
        _request: thruput_json_rpc_types::view::ViewBatchRequest,
        _gas_limit: u64,
        _limits: crate::ViewBatchConfig,
    ) -> Result<thruput_json_rpc_types::view::ViewBatchResponse, RpcServerError> {
        Err(RpcServerError::NotImplemented("viewBatch".into()))
    }

    /// Execute a sandboxed contract simulation.
    ///
    /// `RootInvocationOutput::Abort` and `RootInvocationOutput::Fault`
    /// are successful RPC responses, not `RpcServerError`s. Errors
    /// here are reserved for transport, backend availability, and
    /// request-level failures.
    ///
    /// `gas_limit` is the validated budget to put on the root view
    /// invocation: the request's `gasLimit`, or the server's `view_gas_limit`
    /// when the request names none. No gas is charged.
    /// The shared server checks `min_height` against the returned execution
    /// snapshot height. Adapters must not replace it with a later chain-tip sample.
    ///
    /// Backends should override this once they have a stable hogd view
    /// execution service. The default keeps the public method available
    /// while capability rollout catches up.
    async fn view(
        &self,
        _context: RpcRequestContext,
        _request: ViewRequest,
        _gas_limit: u64,
    ) -> Result<ViewResponse, RpcServerError> {
        Err(RpcServerError::NotImplemented("view".into()))
    }
}
