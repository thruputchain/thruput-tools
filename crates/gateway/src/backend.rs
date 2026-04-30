use std::sync::Arc;

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

use crate::{context::GatewayContext, error::GatewayError, handlers};

/// Production gateway backend adapter for the shared RPC server.
#[derive(Clone)]
pub struct GatewayBackend {
    ctx: Arc<GatewayContext>,
}

impl GatewayBackend {
    /// Create a backend adapter over gateway runtime state.
    pub fn new(ctx: Arc<GatewayContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl SubmitPlane for GatewayBackend {
    #[tracing::instrument(skip_all, fields(tx_hash = ?tx.hash, tx_bytes_len = tx.bytes.len()))]
    async fn submit_raw_transaction(&self, tx: ValidatedTransaction) -> Result<(), RpcServerError> {
        self.ctx
            .batch_submitter
            .submit(tx.bytes, tx.hash)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl ChainPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
        handlers::blocks::chain_tip(&self.ctx)
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(?height))]
    async fn get_block_by_number(
        &self,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        handlers::blocks::get_block_by_number(&self.ctx, height)
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
        handlers::blocks::pending_transaction_count(&self.ctx)
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl ReadPlane for GatewayBackend {
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        handlers::receipt::get_transaction_receipt(&self.ctx, tx_hash)
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        handlers::receipt::get_transaction_status(&self.ctx, tx_hash)
            .await
            .map_err(Into::into)
    }

    #[tracing::instrument(skip_all, fields(filters_len = query.filters.0.len(), limit = query.limit))]
    async fn contract_events(&self, query: EventQuery) -> Result<EventPage, RpcServerError> {
        handlers::events::get_events(&self.ctx, query)
            .await
            .map_err(Into::into)
    }
}

impl From<GatewayError> for RpcServerError {
    fn from(err: GatewayError) -> Self {
        match err {
            GatewayError::TxInvalid(msg) => Self::TxInvalid(msg),
            GatewayError::TxRejected(msg) => Self::TxRejected(msg),
            GatewayError::TxNotFound => Self::TxNotFound,
            GatewayError::BlockNotFound => Self::BlockNotFound,
            GatewayError::InvalidParam(msg) => Self::InvalidParam(msg),
            GatewayError::RangeTooLarge(msg) => Self::RangeTooLarge(msg),
            GatewayError::BackendUnavailable(msg) => Self::BackendUnavailable(msg),
            GatewayError::BackendTimeout => Self::BackendTimeout,
            GatewayError::Overloaded(msg) => Self::Overloaded(msg),
            GatewayError::NotImplemented(method) => Self::NotImplemented(method),
        }
    }
}
