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
//! - `ChainPlane::get_block_by_number`        → coordinator (TBD)
//! - `ChainPlane::pending_transaction_count`  → scheduler
//! - `ReadPlane::transaction_receipt`         → indexer
//! - `ReadPlane::transaction_status`          → **both** (scheduler for
//!   `Pending`, indexer for `Committed`) — the adapter owns this
//!   orchestration; the shared server calls the trait method once.
//! - `ReadPlane::contract_events`             → indexer
//! - `ReadPlane::get_storage_at`              → `DbGateway1` contract-data reads
//! - `ReadPlane::read_native_hog_balance`     → `DbGateway1` free native-token balance reads
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
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    events::{EventPage, EventQuery},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{TransactionStatus, TxHash},
    view::{ViewRequest, ViewResponse},
};

use crate::error::RpcServerError;

/// A transaction that has passed shared RPC-layer validation.
///
/// Obtain instances via `tx::validate_raw_transaction`.
/// No public constructor is provided so callers do not bypass validation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ValidatedTransaction {
    /// Raw signed transaction XDR bytes supplied by the client.
    pub bytes: Vec<u8>,
    /// Canonical transaction hash over the inner `Transaction`.
    pub hash: TxHash,
}

/// Backend plane for accepted raw transaction submission.
#[async_trait]
pub trait SubmitPlane: Send + Sync + 'static {
    /// Submit one validated signed transaction.
    async fn submit_raw_transaction(&self, tx: ValidatedTransaction) -> Result<(), RpcServerError>;
}

/// Backend plane for chain metadata and block-oriented reads.
#[async_trait]
pub trait ChainPlane: Send + Sync + 'static {
    /// Return the current chain tip.
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError>;

    /// Return block data at a specific height, if available.
    async fn get_block_by_number(
        &self,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError>;

    /// Return the number of currently pending transactions.
    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError>;
}

/// Backend plane for transaction, event, and storage reads.
#[async_trait]
pub trait ReadPlane: Send + Sync + 'static {
    /// Return a committed transaction receipt, if available.
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError>;

    /// Return semantic transaction status.
    ///
    /// Backends own any source fallback needed to answer this. For example,
    /// the production gateway checks scheduler pending state before indexer
    /// committed state.
    async fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError>;

    /// Return one page of events matching `query`.
    ///
    /// Walk further pages by sending the same query back with `cursor`
    /// set to the value from the previous page.
    async fn contract_events(&self, query: EventQuery) -> Result<EventPage, RpcServerError>;

    /// Return the latest committed storage item at one (address, key).
    /// `key` is the 32-byte raw contract-data key that the wire form
    /// has already validated as exactly 32 bytes.  The returned
    /// `StorageItemRead` always carries `block_height` (even when no
    /// item is present at the key) so callers can compare against
    /// `chainTip` to gauge freshness.
    async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError>;

    /// Return the latest committed native HOG balance item for one address.
    /// This reads hogd's `FreeNativeTokenBalance` key, not contract storage.
    async fn read_native_hog_balance(
        &self,
        address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError>;
}

/// Backend plane for sandboxed contract simulation.
#[async_trait]
pub trait ViewPlane: Send + Sync + 'static {
    /// Execute a sandboxed contract simulation.
    ///
    /// `RootInvocationOutput::Abort` and `RootInvocationOutput::Fault`
    /// are successful RPC responses, not `RpcServerError`s. Errors
    /// here are reserved for transport, backend availability, and
    /// request-level failures.
    ///
    /// `gas_limit` is the server-configured execution cap to put on the
    /// root view invocation. It is not user supplied and no gas is charged.
    ///
    /// Backends should override this once they have a stable hogd view
    /// execution service. The default keeps the public method available
    /// while capability rollout catches up.
    async fn view(
        &self,
        _request: ViewRequest,
        _gas_limit: u64,
    ) -> Result<ViewResponse, RpcServerError> {
        Err(RpcServerError::NotImplemented("view".into()))
    }
}
