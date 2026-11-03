//! Native discovery, state, simulation and history handlers.

mod projection;
mod service;
pub use service::history::{
    validate_address_transaction_page, validate_block_transaction_page, validate_event_page,
};
pub use service::{NativeReadCapabilities, NativeReadService};

use crate::{RpcRequestContext, RpcServerError, ViewBatchConfig};
use async_trait::async_trait;
use jsonrpsee::types::ErrorObjectOwned;
use thruput_json_rpc_types::error::native::{BackendDataInvalidReason, ErrorData, Reason, codes};
use thruput_json_rpc_types::{ContractAddress, requests as input, responses as out};

/// The two state keys used by storage and balance methods.
#[derive(Debug, Clone, Copy)]
pub enum StateKey {
    ContractData([u8; 32]),
    FreeNativeTokenBalance,
}

/// Committed indexer metadata. Implementations must distinguish authoritative
/// absence from unavailable, rebuilding or invalid index data.
#[async_trait]
pub trait NativeBlockPlane: Send + Sync + 'static {
    async fn native_block(
        &self,
        context: RpcRequestContext,
        height: thruput_json_rpc_types::BlockHeight,
    ) -> Result<out::BlockResult, RpcServerError>;
}

/// Strict history pages from the indexer. Cursor contents remain backend-owned.
#[async_trait]
pub trait NativeHistoryPlane: Send + Sync + 'static {
    async fn native_events(
        &self,
        context: RpcRequestContext,
        request: input::EventQuery,
    ) -> Result<out::EventPage, RpcServerError>;
    async fn native_address_transactions(
        &self,
        context: RpcRequestContext,
        request: input::AddressHistoryQuery,
    ) -> Result<out::AddressTransactionPage, RpcServerError>;
    async fn native_block_transactions(
        &self,
        context: RpcRequestContext,
        request: input::BlockPageRequest,
    ) -> Result<out::BlockTransactionPage, RpcServerError>;
}

/// State answers validated before transport demultiplexing loses evidence.
#[async_trait]
pub trait NativeStatePlane: Send + Sync + 'static {
    async fn native_contract_deployment(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::ContractDeploymentResult, RpcServerError>;
    async fn native_contract_artifact(
        &self,
        context: RpcRequestContext,
        hash: thruput_json_rpc_types::SHA256Hash,
    ) -> Result<out::ContractArtifactResult, RpcServerError>;
    async fn native_storage(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: StateKey,
    ) -> Result<out::StorageResult, RpcServerError>;
    async fn native_registered_auth(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::RegisteredAuthResult, RpcServerError>;
}

/// Strict simulation answers. A legacy view adapter can discard malformed
/// events and therefore cannot implement this trait by renaming its JSON.
#[async_trait]
pub trait NativeSimulationPlane: Send + Sync + 'static {
    async fn simulate(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationRequest,
        gas_limit: u64,
    ) -> Result<out::SimulationResult, RpcServerError>;
    async fn simulate_batch(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationsRequest,
        gas_limit: u64,
        limits: ViewBatchConfig,
    ) -> Result<out::SimulationResults, RpcServerError>;
}

fn invalid(message: &str) -> RpcServerError {
    RpcServerError::invalid_data(BackendDataInvalidReason::InconsistentResult, message)
}

/// Project read-only failure evidence; no error here asserts write admission.
pub(super) fn rpc_error(error: &RpcServerError) -> ErrorObjectOwned {
    let (code, data) = match error {
        RpcServerError::ContractArtifactUnavailable(hash) => (
            codes::CONTRACT_ARTIFACT_UNAVAILABLE,
            ErrorData::ContractArtifactUnavailable { hash: *hash },
        ),
        RpcServerError::InvalidParam(_) => (
            codes::INVALID_PARAMS,
            ErrorData::InvalidParams {
                reason: None,
                path: None,
            },
        ),
        RpcServerError::InvalidCursor(_) => (
            codes::INVALID_PARAMS,
            ErrorData::InvalidCursor {
                reason: None,
                path: Some("/params/cursor".into()),
            },
        ),
        RpcServerError::HistoryUnavailable(_) => (
            codes::HISTORY_UNAVAILABLE,
            ErrorData::HistoryUnavailable {
                reason: None,
                oldest_indexed_block_height: None,
                latest_indexed_block_height: None,
            },
        ),
        RpcServerError::BackendUnavailable(_) => (
            codes::BACKEND_UNAVAILABLE,
            ErrorData::BackendUnavailable {
                admission: None,
                transaction_hash: None,
                retry_after_ms: None,
                service_code: None,
                error: None,
            },
        ),
        RpcServerError::BackendTimeout => (
            codes::BACKEND_TIMEOUT,
            ErrorData::BackendTimeout {
                admission: None,
                transaction_hash: None,
                retry_after_ms: None,
            },
        ),
        RpcServerError::Overloaded(_) => (
            codes::OVERLOADED,
            ErrorData::Overloaded {
                admission: None,
                transaction_hash: None,
                retry_after_ms: None,
                error: None,
            },
        ),
        RpcServerError::BackendDataInvalid { reason, .. } => (
            codes::BACKEND_DATA_INVALID,
            ErrorData::BackendDataInvalid {
                admission: None,
                transaction_hash: None,
                reason: Some(Reason::Known(*reason)),
                observation: None,
            },
        ),
        RpcServerError::ViewHeightUnavailable(value)
            if *value.observed == -1 && *value.requested >= 0 =>
        {
            return rpc_error(&RpcServerError::BackendUnavailable(
                "no committed snapshot".into(),
            ));
        }
        RpcServerError::ViewHeightUnavailable(value)
            if *value.observed >= 0 && value.observed < value.requested =>
        {
            (
                codes::STATE_HEIGHT_UNAVAILABLE,
                ErrorData::StateHeightUnavailable {
                    requested_state_block_height: value.requested.clone(),
                    observed_state_block_height: value.observed.clone(),
                },
            )
        }
        RpcServerError::ViewHeightUnavailable(_) => {
            return rpc_error(&invalid("invalid snapshot freshness evidence"));
        }
        RpcServerError::NotImplemented(_) => {
            return ErrorObjectOwned::owned(-32601, "Method not found", None::<()>);
        }
        _ => return ErrorObjectOwned::owned(-32603, "Unexpected read backend error", None::<()>),
    };
    let message = match error {
        RpcServerError::BackendDataInvalid { message, .. } => {
            format!("backend data invalid: {message}")
        }
        _ => error.to_string(),
    };
    ErrorObjectOwned::owned(code, message, Some(data))
}

#[cfg(test)]
mod tests;
