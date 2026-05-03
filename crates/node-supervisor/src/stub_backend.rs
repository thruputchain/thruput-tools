//! Stub backend planes that always return `NotImplemented`.
//!
//! The node supervisor wires backend planes incrementally. `chainTip` and
//! `sendRawTransaction` now have a real hogd-dev scheduler adapter; read
//! methods still use this stub until PR 4c lands the corresponding x23
//! adapters.
//!
//! Wire-error strings carry only the JSON-RPC method name, so the public
//! protocol surface is stable. The
//! "stubbed because hogd-dev adapters aren't wired yet" context goes to
//! `tracing::warn!` at supervisor startup and to module-level docs, not
//! into the wire error.
//!
//! The stub is intentionally `pub(crate)`. It is not a long-term API; once
//! submit and read have real adapters the type goes away.

use async_trait::async_trait;
use groundhog_rpc_api_types::{
    BlockHeight,
    block::{BlockInfo, ChainTip},
    events::{EventPage, EventQuery},
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};
use groundhog_rpc_server::{
    ChainPlane, ReadPlane, RpcServerError, SubmitPlane, ValidatedTransaction,
};

/// Backend that returns `NotImplemented` for unwired planes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StubBackend;

impl StubBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SubmitPlane for StubBackend {
    async fn submit_raw_transaction(
        &self,
        _tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        Err(RpcServerError::NotImplemented("sendRawTransaction".into()))
    }
}

#[async_trait]
impl ChainPlane for StubBackend {
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
        Err(RpcServerError::NotImplemented("chainTip".into()))
    }

    async fn get_block_by_number(
        &self,
        _height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
    }

    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "pendingTransactionCount".into(),
        ))
    }
}

#[async_trait]
impl ReadPlane for StubBackend {
    async fn transaction_receipt(
        &self,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionReceipt".into()))
    }

    async fn transaction_status(
        &self,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionStatus".into()))
    }

    async fn contract_events(&self, _query: EventQuery) -> Result<EventPage, RpcServerError> {
        Err(RpcServerError::NotImplemented("getEvents".into()))
    }
}
