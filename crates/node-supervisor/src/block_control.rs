//! Local block production and block time RPC methods over the block driver.

use std::time::Duration;

use async_trait::async_trait;
use jsonrpsee::core::RpcResult;
use thruput_json_rpc_server::{
    RpcServerError,
    native::block_control::{NativeBlockControlPlane, rpc_error},
};
use thruput_json_rpc_types::{
    error::native::BackendDataInvalidReason, requests as input, responses as out,
};
use thruput_xdr::types::TimePoint;

use crate::{
    BlockDriverConfig, BlockDriverMode, BlockProducer, BlockProductionError, ProducedBlock,
};

/// Serves the `dev_` block control methods from the local block driver.
#[derive(Debug, Clone)]
pub(crate) struct LocalBlockControl {
    producer: BlockProducer,
}

impl LocalBlockControl {
    pub(crate) fn new(producer: BlockProducer) -> Self {
        Self { producer }
    }
}

#[async_trait]
impl NativeBlockControlPlane for LocalBlockControl {
    fn enabled(&self) -> bool {
        true
    }

    async fn produce_block(
        &self,
        request: input::ProduceBlockRequest,
    ) -> RpcResult<out::ProducedBlock> {
        let block = match request.block_timepoint {
            Some(timepoint) => self.producer.produce_block_at(timepoint).await,
            None => self.producer.produce_block().await,
        };
        block
            .map(produced_block)
            .map_err(|error| rpc_failure(&error))
    }

    async fn produce_blocks(
        &self,
        request: input::ProduceBlocksRequest,
    ) -> RpcResult<out::ProducedBlocks> {
        let interval = request.interval_nanoseconds.map(Duration::from_nanos);
        let blocks = self
            .producer
            .produce_blocks(request.count, interval)
            .await
            .map_err(|error| rpc_failure(&error))?;
        Ok(out::ProducedBlocks {
            blocks: blocks.into_iter().map(produced_block).collect(),
        })
    }

    async fn set_next_block_timepoint(
        &self,
        request: input::SetNextBlockTimepointRequest,
    ) -> RpcResult<out::BlockClock> {
        self.producer
            .set_next_block_timepoint(request.block_timepoint)
            .await
            .map(block_clock)
            .map_err(|error| rpc_failure(&error))
    }

    async fn increase_time(
        &self,
        request: input::IncreaseTimeRequest,
    ) -> RpcResult<out::BlockClock> {
        self.producer
            .increase_time(Duration::from_nanos(request.nanoseconds))
            .await
            .map(block_clock)
            .map_err(|error| rpc_failure(&error))
    }

    async fn set_block_driver(
        &self,
        request: input::SetBlockDriverRequest,
    ) -> RpcResult<out::BlockDriverStatus> {
        let mode = match request.mode {
            input::BlockDriverMode::Instant => BlockDriverMode::Instant,
            input::BlockDriverMode::Interval => BlockDriverMode::Interval,
            input::BlockDriverMode::Manual => BlockDriverMode::Manual,
        };
        let interval = request.interval_milliseconds.map(Duration::from_millis);
        self.producer
            .set_block_driver(mode, interval)
            .await
            .map(block_driver_status)
            .map_err(|error| rpc_failure(&error))
    }
}

fn produced_block(block: ProducedBlock) -> out::ProducedBlock {
    out::ProducedBlock {
        block_height: block.height.into(),
        block_timepoint: block.timepoint,
    }
}

fn block_clock(next_block_timepoint: TimePoint) -> out::BlockClock {
    out::BlockClock {
        next_block_timepoint,
    }
}

fn block_driver_status(config: BlockDriverConfig) -> out::BlockDriverStatus {
    let mode = match config.mode {
        BlockDriverMode::Instant => out::ActiveBlockDriverMode::Instant,
        BlockDriverMode::Interval => out::ActiveBlockDriverMode::Interval,
        BlockDriverMode::Manual => out::ActiveBlockDriverMode::Manual,
    };
    out::BlockDriverStatus {
        mode,
        interval_milliseconds: u64::try_from(config.effective_interval().as_millis())
            .unwrap_or(u64::MAX),
    }
}

fn rpc_failure(error: &BlockProductionError) -> jsonrpsee::types::ErrorObjectOwned {
    rpc_error(&server_error(error))
}

/// Classify a block driver failure for the native error catalogue.
fn server_error(error: &BlockProductionError) -> RpcServerError {
    let message = error.to_string();
    match error {
        BlockProductionError::TimepointBeforeTip { .. }
        | BlockProductionError::TimepointOverflow
        | BlockProductionError::InvalidBlockCount { .. } => RpcServerError::InvalidParam(message),
        BlockProductionError::TimedOut { .. } => RpcServerError::BackendTimeout,
        BlockProductionError::DriverStopped
        | BlockProductionError::OutcomeUnknown
        | BlockProductionError::NoGenesis
        | BlockProductionError::SchedulerRpc { .. }
        | BlockProductionError::ProposalOutcomeUnknown { .. }
        | BlockProductionError::SchedulerRejected { .. }
        | BlockProductionError::CommitRejected { .. }
        | BlockProductionError::CommitOutcomeUnknown { .. } => {
            RpcServerError::BackendUnavailable(message)
        }
        BlockProductionError::UnexpectedTipResult { .. }
        | BlockProductionError::MalformedSchedulerProposal { .. }
        | BlockProductionError::ConsensusHashEncoding { .. } => {
            RpcServerError::invalid_data(BackendDataInvalidReason::InconsistentResult, message)
        }
    }
}

#[cfg(test)]
mod tests {
    use jsonrpsee::types::error::INVALID_PARAMS_CODE;
    use thruput_json_rpc_types::error::native::codes;

    use super::*;

    #[test]
    fn caller_errors_are_invalid_params() {
        for error in [
            BlockProductionError::TimepointBeforeTip {
                requested: 1,
                tip: 2,
            },
            BlockProductionError::TimepointOverflow,
            BlockProductionError::InvalidBlockCount { count: 0, max: 1 },
        ] {
            let object = rpc_failure(&error);
            assert_eq!(object.code(), INVALID_PARAMS_CODE);
            assert!(object.message().contains("invalid params"));
        }
    }

    #[test]
    fn driver_failures_keep_their_native_class() {
        assert_eq!(
            rpc_failure(&BlockProductionError::DriverStopped).code(),
            codes::BACKEND_UNAVAILABLE
        );
        assert_eq!(
            rpc_failure(&BlockProductionError::TimedOut {
                timeout: Duration::from_secs(1)
            })
            .code(),
            codes::BACKEND_TIMEOUT
        );
        assert_eq!(
            rpc_failure(&BlockProductionError::UnexpectedTipResult { result: "x".into() }).code(),
            codes::BACKEND_DATA_INVALID
        );
    }

    #[test]
    fn driver_status_reports_the_effective_interval() {
        let status = block_driver_status(BlockDriverConfig::interval(Duration::from_millis(250)));
        assert!(matches!(status.mode, out::ActiveBlockDriverMode::Interval));
        assert_eq!(status.interval_milliseconds, 250);
    }
}
