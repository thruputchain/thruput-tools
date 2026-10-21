//! The RPC surface the simulator drives its targets through.
//!
//! [`SimRpcClient`] is the seam the executor, provisioner and contract phases
//! share: the live client pool implements it for real runs, and unit tests
//! implement it with doubles. Receipt waiting is not reimplemented here — it
//! delegates to the one poll loop in `thruput-json-rpc-client` through
//! [`ReceiptSource`].

use std::future::Future;
use std::time::Duration;

use thruput_json_rpc_client::receipt::{ReceiptPollConfig, ReceiptPollError, ReceiptSource};
use thruput_json_rpc_types::{
    ContractAddress,
    block::ChainTip,
    events::{EventPage, EventQuery},
    hex::HexBytes,
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::TxHash,
    view::{ViewRequest, ViewResponse},
};

use crate::error::SimError;
use crate::executor::DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL;

/// Why a simulator RPC call did not produce a result.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SimRpcError {
    /// Native RPC failure, retaining complete response or application evidence.
    #[error("{method} failed: {source}")]
    Native {
        method: &'static str,
        #[source]
        source: Box<thruput_json_rpc_client::native::NativeClientError>,
    },
    /// A native response cannot be represented by the simulator report model.
    #[error("{method}: {message}; response: {raw}")]
    InvalidResponse {
        method: &'static str,
        message: String,
        raw: serde_json::Value,
    },
    /// The JSON-RPC round trip failed.
    #[error("{method} failed: {source}")]
    Rpc {
        method: &'static str,
        #[source]
        source: jsonrpsee::core::client::Error,
    },
    /// The client pool shut down while a request waited for a permit.
    #[error("simulator RPC client limiter closed")]
    LimiterClosed,
    /// The client in use does not serve this method.
    #[error("{method} is not implemented by this client")]
    Unsupported { method: &'static str },
}

impl SimRpcError {
    /// Complete native result or error evidence for persisted setup diagnostics.
    pub fn evidence(&self) -> serde_json::Value {
        use thruput_json_rpc_client::native::NativeClientError;
        match self {
            Self::Native { method, source } => match source.as_ref() {
                NativeClientError::Application { source, .. } => {
                    serde_json::json!({"method": method, "rpcError": source})
                }

                _ => serde_json::json!({"method": method, "error": source.to_string()}),
            },
            Self::InvalidResponse {
                method,
                message,
                raw,
            } => serde_json::json!({"method": method, "result": raw, "error": message}),
            _ => serde_json::json!({"error": self.to_string()}),
        }
    }

    /// A failed round trip reported by `method`.
    pub fn rpc(method: &'static str, source: jsonrpsee::core::client::Error) -> Self {
        Self::Rpc { method, source }
    }

    /// A failed round trip whose only diagnostic is `detail`.
    ///
    /// Test doubles use this to stand in for a transport that never ran.
    pub fn detail(method: &'static str, detail: impl Into<String>) -> Self {
        Self::rpc(
            method,
            jsonrpsee::core::client::Error::Custom(detail.into()),
        )
    }
}

/// How the simulator waits on a receipt once a transaction is accepted.
pub type ReceiptWaitError = ReceiptPollError<SimRpcError>;

/// The RPC surface the simulator executor drives.
pub trait SimRpcClient {
    fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, SimRpcError>> + Send;

    fn send_raw_transaction(
        &self,
        tx_hex: String,
    ) -> impl Future<Output = Result<TxHash, SimRpcError>> + Send;

    /// Return the committed receipt for a transaction hash, or `None` when it is not indexed yet.
    fn transaction_receipt(
        &self,
        _tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, SimRpcError>> + Send {
        async {
            Err(SimRpcError::Unsupported {
                method: "transaction receipt",
            })
        }
    }

    fn view(
        &self,
        _request: ViewRequest,
    ) -> impl Future<Output = Result<ViewResponse, SimRpcError>> + Send {
        async { Err(SimRpcError::Unsupported { method: "view" }) }
    }

    fn storage_read(
        &self,
        _address: ContractAddress,
        _key: HexBytes,
    ) -> impl Future<Output = Result<StorageItemRead, SimRpcError>> + Send {
        async {
            Err(SimRpcError::Unsupported {
                method: "storage read",
            })
        }
    }

    fn events_query(
        &self,
        _query: EventQuery,
    ) -> impl Future<Output = Result<EventPage, SimRpcError>> + Send {
        async {
            Err(SimRpcError::Unsupported {
                method: "events query",
            })
        }
    }
}

mod native;

pub(crate) use native::known;

impl SimRpcClient for thruput_json_rpc_client::HttpClient {
    async fn chain_tip(&self) -> Result<ChainTip, SimRpcError> {
        native::chain_tip(self).await
    }
    async fn send_raw_transaction(&self, tx_hex: String) -> Result<TxHash, SimRpcError> {
        native::send_transaction(self, tx_hex).await
    }
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, SimRpcError> {
        native::receipt(self, tx_hash).await
    }
    async fn view(&self, request: ViewRequest) -> Result<ViewResponse, SimRpcError> {
        native::simulate(self, request).await
    }
    async fn storage_read(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> Result<StorageItemRead, SimRpcError> {
        native::storage(self, address, key).await
    }
    async fn events_query(&self, query: EventQuery) -> Result<EventPage, SimRpcError> {
        native::events(self, query).await
    }
}

/// A receipt wait that ended without a receipt.
///
/// `step` names what was waiting, such as `"airdrop"` or `"contract-call"`, so
/// a simulator event says which step stalled.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptWaitFailure {
    #[error("{step} receipt poll timed out after {attempts} attempts")]
    Timeout { step: String, attempts: usize },
    #[error("{step} receipt poll failed: {source}")]
    Failed {
        step: String,
        #[source]
        source: ReceiptWaitError,
    },
}

impl ReceiptWaitFailure {
    /// Name the workload step a finished receipt poll was waiting on.
    pub fn step(step: &str, source: ReceiptWaitError) -> Self {
        match source {
            ReceiptPollError::Timeout { attempts, .. } => ReceiptWaitFailure::Timeout {
                step: step.to_owned(),
                attempts,
            },
            source => ReceiptWaitFailure::Failed {
                step: step.to_owned(),
                source,
            },
        }
    }
}

impl From<ReceiptWaitFailure> for SimError {
    fn from(error: ReceiptWaitFailure) -> Self {
        SimError::Executor(error.to_string())
    }
}

/// Adapts a [`SimRpcClient`] to the shared receipt-poll loop.
///
/// The simulator meters every request through its client pool, so polling has
/// to go back through [`SimRpcClient`] per attempt rather than borrow one
/// connection for the whole wait. Pass one to the shared `poll_receipt`
/// simulator has no receipt wait of its own.
pub struct SimReceiptSource<'a, C: ?Sized>(pub &'a C);

impl<C> ReceiptSource for SimReceiptSource<'_, C>
where
    C: SimRpcClient + Sync + ?Sized,
{
    type Error = SimRpcError;

    #[allow(clippy::manual_async_fn)]
    fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, Self::Error>> + Send {
        async move { self.0.transaction_receipt(tx_hash).await }
    }
}

/// The retry schedule the simulator polls receipts on.
///
/// The simulator polls receipts only: it records what a client observes, so an
/// advisory status lookup would add a request per attempt without changing the
/// outcome it measures.
pub fn receipt_poll_config(budget: Duration) -> ReceiptPollConfig {
    ReceiptPollConfig::within(budget, DEFAULT_RECEIPT_DRAIN_RETRY_INTERVAL)
}
