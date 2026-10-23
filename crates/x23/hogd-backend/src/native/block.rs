//! Strict committed block metadata from the indexer.

use super::{BackendDataInvalidReason, RpcServerError, invalid};
use thruput_indexer::{BlockLookupOutcome, BlockLookupResult};
use thruput_json_rpc_types::{BlockHeight, block, responses as out};

/// Only an explicit authoritative indexer miss is absence. RPC/decoding failures
/// must reach this function's caller as errors, never as a synthetic miss.
pub fn block_lookup(
    height: &BlockHeight,
    result: BlockLookupResult,
) -> Result<out::BlockResult, RpcServerError> {
    match result {
        BlockLookupResult::BlockFound { value } => {
            if *height <= 0.into() || value.proposal_header.block_height != *height {
                return Err(invalid(
                    BackendDataInvalidReason::InconsistentResult,
                    "block metadata has an unexpected height",
                ));
            }
            let block = block::BlockInfo::try_from(value).map_err(|error| {
                let reason = match error {
                    block::BlockProjectionError::UnsupportedExtension { .. } => {
                        BackendDataInvalidReason::UnsupportedValue
                    }
                    block::BlockProjectionError::ConsensusValueEncoding { .. } => {
                        BackendDataInvalidReason::InvalidEncoding
                    }
                    _ => BackendDataInvalidReason::UnsupportedValue,
                };
                invalid(reason, "invalid committed block projection")
            })?;
            Ok(Some(block.into()))
        }
        BlockLookupResult::Default { outcome }
            if outcome == BlockLookupOutcome::BlockNotFound.0 =>
        {
            Ok(None)
        }
        _ => Err(invalid(
            BackendDataInvalidReason::UnsupportedValue,
            "unknown block lookup outcome",
        )),
    }
}

#[cfg(test)]
mod tests;
