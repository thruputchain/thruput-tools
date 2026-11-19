//! Optional local block production and block time control. Only a local node
//! configures it; hosted gateways never register these methods.

use async_trait::async_trait;
use jsonrpsee::{core::RpcResult, types::ErrorObjectOwned};
use thruput_json_rpc_types::{requests as input, responses as out};

use crate::RpcServerError;

/// Project a block control failure with native error data. No error here
/// implies that a block was or was not produced, unless it is
/// `InvalidParams`, which the node returns before any scheduler work.
pub fn rpc_error(error: &RpcServerError) -> ErrorObjectOwned {
    super::read::rpc_error(error)
}

/// Local block production and block clock operations.
#[async_trait]
pub trait NativeBlockControlPlane: Send + Sync + 'static {
    /// Whether this plane is configured. Unconfigured methods are removed
    /// before the module is finalized.
    fn enabled(&self) -> bool;
    /// Produce one block now, optionally at a pinned timepoint.
    async fn produce_block(
        &self,
        request: input::ProduceBlockRequest,
    ) -> RpcResult<out::ProducedBlock>;
    /// Produce several blocks now, optionally a fixed interval apart.
    async fn produce_blocks(
        &self,
        request: input::ProduceBlocksRequest,
    ) -> RpcResult<out::ProducedBlocks>;
    /// Give the next block exactly the requested timepoint.
    async fn set_next_block_timepoint(
        &self,
        request: input::SetNextBlockTimepointRequest,
    ) -> RpcResult<out::BlockClock>;
    /// Move the block clock forward for every later block.
    async fn increase_time(
        &self,
        request: input::IncreaseTimeRequest,
    ) -> RpcResult<out::BlockClock>;
    /// Set or clear the fixed step between unpinned blocks.
    async fn set_block_time_step(
        &self,
        request: input::SetBlockTimeStepRequest,
    ) -> RpcResult<out::BlockTimeStep>;
    /// Change how blocks are produced.
    async fn set_block_driver(
        &self,
        request: input::SetBlockDriverRequest,
    ) -> RpcResult<out::BlockDriverStatus>;
}

#[async_trait]
impl NativeBlockControlPlane for () {
    fn enabled(&self) -> bool {
        false
    }
    async fn produce_block(&self, _: input::ProduceBlockRequest) -> RpcResult<out::ProducedBlock> {
        Err(super::service::method_not_found())
    }
    async fn produce_blocks(
        &self,
        _: input::ProduceBlocksRequest,
    ) -> RpcResult<out::ProducedBlocks> {
        Err(super::service::method_not_found())
    }
    async fn set_next_block_timepoint(
        &self,
        _: input::SetNextBlockTimepointRequest,
    ) -> RpcResult<out::BlockClock> {
        Err(super::service::method_not_found())
    }
    async fn increase_time(&self, _: input::IncreaseTimeRequest) -> RpcResult<out::BlockClock> {
        Err(super::service::method_not_found())
    }
    async fn set_block_time_step(
        &self,
        _: input::SetBlockTimeStepRequest,
    ) -> RpcResult<out::BlockTimeStep> {
        Err(super::service::method_not_found())
    }
    async fn set_block_driver(
        &self,
        _: input::SetBlockDriverRequest,
    ) -> RpcResult<out::BlockDriverStatus> {
        Err(super::service::method_not_found())
    }
}
