use std::sync::Arc;

use jsonrpsee::core::RpcResult;

use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    events::IdentifiedEvent,
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};

use crate::context::GatewayContext;
use crate::error;
use crate::handlers;

/// Implementation of the JSON-RPC trait, delegating to handler functions.
pub struct GroundhogApiImpl {
    ctx: Arc<GatewayContext>,
}

impl GroundhogApiImpl {
    pub fn new(ctx: Arc<GatewayContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait::async_trait]
impl GroundhogApiServer for GroundhogApiImpl {
    async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash> {
        let hex_str = tx_hex
            .strip_prefix("0x")
            .or_else(|| tx_hex.strip_prefix("0X"))
            .unwrap_or(&tx_hex);
        let tx_bytes = hex::decode(hex_str)
            .map_err(|e| error::GatewayError::TxInvalid(format!("invalid hex: {e}")))?;
        Ok(handlers::submit::send_raw_transaction(&self.ctx, &tx_bytes).await?)
    }

    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>> {
        Ok(handlers::receipt::get_transaction_receipt(&self.ctx, tx_hash).await?)
    }

    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus> {
        Ok(handlers::receipt::get_transaction_status(&self.ctx, tx_hash).await?)
    }

    async fn chain_tip(&self) -> RpcResult<ChainTip> {
        Ok(handlers::blocks::chain_tip(&self.ctx).await?)
    }

    async fn get_block_by_number(&self, height: BlockHeight) -> RpcResult<Option<BlockInfo>> {
        Ok(handlers::blocks::get_block_by_number(&self.ctx, height).await?)
    }

    async fn pending_transaction_count(&self) -> RpcResult<u64> {
        Ok(handlers::blocks::pending_transaction_count(&self.ctx).await?)
    }

    async fn get_contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> RpcResult<Vec<IdentifiedEvent>> {
        Ok(
            handlers::events::get_contract_events(&self.ctx, address, from_height, to_height)
                .await?,
        )
    }
}
