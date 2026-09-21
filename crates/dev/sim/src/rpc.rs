//! The RPC surface the simulator drives its targets through.
//!
//! [`SimRpcClient`] is the seam the executor, provisioner and contract phases
//! share: the live client pool implements it for real runs, and unit tests
//! implement it with doubles. Receipt waiting is not reimplemented here — it
//! delegates to the one poll loop in `groundhog-json-rpc-client` through
//! [`ReceiptSource`].

use std::future::Future;
use std::time::Duration;

use groundhog_json_rpc_client::{
    GroundhogApiClient, ReceiptPollConfig, ReceiptPollError, ReceiptSource, poll_receipt,
};
use groundhog_json_rpc_types::{
    ContractAddress,
    block::ChainTip,
    events::{EventPage, EventQuery},
    hex::HexBytes,
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::TxHash,
    view::{ViewRequest, ViewResponse},
};

/// Why a simulator RPC call did not produce a result.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SimRpcError {
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

impl<T> SimRpcClient for T
where
    T: GroundhogApiClient + Send + Sync,
{
    #[allow(clippy::manual_async_fn)]
    fn chain_tip(&self) -> impl Future<Output = Result<ChainTip, SimRpcError>> + Send {
        async {
            GroundhogApiClient::chain_tip(self)
                .await
                .map_err(|source| SimRpcError::rpc("chainTip", source))
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn send_raw_transaction(
        &self,
        tx_hex: String,
    ) -> impl Future<Output = Result<TxHash, SimRpcError>> + Send {
        async {
            GroundhogApiClient::send_raw_transaction(self, tx_hex)
                .await
                .map_err(|source| SimRpcError::rpc("sendRawTransaction", source))
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> impl Future<Output = Result<Option<TransactionReceipt>, SimRpcError>> + Send {
        async move {
            GroundhogApiClient::get_transaction_receipt(self, tx_hash)
                .await
                .map_err(|source| SimRpcError::rpc("getTransactionReceipt", source))
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn view(
        &self,
        request: ViewRequest,
    ) -> impl Future<Output = Result<ViewResponse, SimRpcError>> + Send {
        async move {
            GroundhogApiClient::view(self, request)
                .await
                .map_err(|source| SimRpcError::rpc("view", source))
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn storage_read(
        &self,
        address: ContractAddress,
        key: HexBytes,
    ) -> impl Future<Output = Result<StorageItemRead, SimRpcError>> + Send {
        async move {
            GroundhogApiClient::get_storage_at(self, address, key)
                .await
                .map_err(|source| SimRpcError::rpc("getStorageAt", source))
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn events_query(
        &self,
        query: EventQuery,
    ) -> impl Future<Output = Result<EventPage, SimRpcError>> + Send {
        async move {
            GroundhogApiClient::get_events(self, query)
                .await
                .map_err(|source| SimRpcError::rpc("getEvents", source))
        }
    }
}

/// One-line diagnostic for a receipt wait that ended without a receipt.
///
/// `label` names what was waiting, such as `"airdrop"` or `"contract-call"`,
/// so a simulator event says which step stalled.
pub fn receipt_wait_detail(label: &str, error: &ReceiptWaitError) -> String {
    match error {
        ReceiptPollError::Timeout { attempts, .. } => {
            format!("{label} receipt poll timed out after {attempts} attempts")
        }
        other => format!("{label} receipt poll failed: {other}"),
    }
}

/// Adapts a [`SimRpcClient`] to the shared receipt-poll loop.
///
/// The simulator meters every request through its client pool, so polling has
/// to go back through [`SimRpcClient`] per attempt rather than borrow one
/// connection for the whole wait.
struct SimReceiptSource<'a, C: ?Sized>(&'a C);

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

/// Wait for `tx_hash`'s receipt, retrying for at most `budget`.
///
/// The simulator polls receipts only: it records what a client observes, so an
/// advisory status lookup would add a request per attempt without changing the
/// outcome it measures.
pub async fn wait_for_receipt<C>(
    client: &C,
    tx_hash: TxHash,
    budget: Duration,
    interval: Duration,
) -> Result<TransactionReceipt, ReceiptWaitError>
where
    C: SimRpcClient + Sync + ?Sized,
{
    poll_receipt(
        &SimReceiptSource(client),
        tx_hash,
        ReceiptPollConfig::within(budget, interval),
    )
    .await
}
