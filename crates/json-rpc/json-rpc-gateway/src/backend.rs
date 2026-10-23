use std::sync::Arc;

use async_trait::async_trait;
use thruput_json_rpc_server::{
    ChainPlane, ReadPlane, RpcRequestContext, RpcServerError, SubmitPlane, TipReading,
    ValidatedTransaction,
};
use thruput_json_rpc_types::{
    BlockHeight, ContractAddress,
    address_auth::AddressAuthDataResponse,
    block::BlockInfo,
    events::{EventPage, EventQuery},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
};

use crate::{context::GatewayContext, handlers};

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

    fn bounded_context(&self, context: RpcRequestContext) -> RpcRequestContext {
        // The shared RPC server enforces the request deadline. Carry its budget
        // through queueing and backend operations without another request timer.
        context.capped_from_start(self.ctx.config.request_timeout())
    }
}

#[async_trait]
impl SubmitPlane for GatewayBackend {
    #[tracing::instrument(skip_all, fields(tx_hash = ?tx.hash, tx_bytes_len = tx.bytes.len()))]
    async fn submit_raw_transaction(
        &self,
        context: RpcRequestContext,
        tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        let context = self.bounded_context(context);
        self.ctx
            .batch_submitter
            .submit(context, tx.bytes, tx.hash)
            .await
    }
}

#[async_trait]
impl ChainPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self, context: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::blocks::chain_tip(&self.ctx, context).await
    }

    #[tracing::instrument(skip_all, fields(?height))]
    async fn get_block_by_number(
        &self,
        context: RpcRequestContext,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::blocks::get_block_by_number(&self.ctx, context, height).await
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(
        &self,
        _context: RpcRequestContext,
    ) -> Result<u64, RpcServerError> {
        handlers::blocks::pending_transaction_count(&self.ctx).await
    }
}

#[async_trait]
impl ReadPlane for GatewayBackend {
    #[tracing::instrument(skip_all, fields(?query.height))]
    async fn transactions_by_block(
        &self,
        context: RpcRequestContext,
        query: thruput_json_rpc_types::transaction::BlockTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::transactions::get_transactions_by_block(&self.ctx, context, query).await
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::receipt::get_transaction_receipt(&self.ctx, context, tx_hash).await
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::receipt::get_transaction_status(&self.ctx, context, tx_hash).await
    }

    #[tracing::instrument(skip_all, fields(filters_len = query.filters.0.len(), limit = query.limit))]
    async fn contract_events(
        &self,
        context: RpcRequestContext,
        query: EventQuery,
    ) -> Result<EventPage, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::events::get_events(&self.ctx, context, query).await
    }

    #[tracing::instrument(skip_all, fields(?query.address, limit = query.limit))]
    async fn transactions_by_address(
        &self,
        context: RpcRequestContext,
        query: AddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::transactions::get_transactions_by_address(&self.ctx, context, query).await
    }

    #[tracing::instrument(skip_all, fields(?address))]
    async fn get_storage_at(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        let context = self.bounded_context(context);
        self.ctx.storage_batcher.read(context, address, key).await
    }

    #[tracing::instrument(skip_all, fields(?address))]
    async fn read_native_hog_balance(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError> {
        let context = self.bounded_context(context);
        self.ctx
            .storage_batcher
            .read_native_hog_balance(context, address)
            .await
    }

    #[tracing::instrument(skip_all, fields(?address))]
    async fn read_address_auth_data(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<AddressAuthDataResponse, RpcServerError> {
        let context = self.bounded_context(context);
        self.ctx
            .storage_batcher
            .read_address_auth_data(context, address)
            .await
    }
}

mod native;
