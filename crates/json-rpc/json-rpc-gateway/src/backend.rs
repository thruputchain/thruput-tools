use std::sync::Arc;

use async_trait::async_trait;
use thruput_json_rpc_server::{ChainPlane, RpcRequestContext, RpcServerError, TipReading};
use thruput_json_rpc_types::{BlockHeight, ContractAddress, block::BlockInfo};

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

mod native;
