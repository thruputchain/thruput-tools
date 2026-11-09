//! Native transaction evidence and backend boundaries.

mod attempt;
mod error;
mod service;
pub use attempt::{SubmissionAttempt, SubmissionGuard};
pub(crate) use error::{invalid_receipt, read_error, submit_error};
pub use service::NativeTransactionService;

use crate::{RpcRequestContext, RpcServerError, ValidatedTransaction};
use async_trait::async_trait;
use thruput_json_rpc_types::{error::native::ReceiptObservation, responses, transaction::TxHash};

/// A failed read may retain independently validated commitment identity.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct NativeReadError {
    #[source]
    pub error: RpcServerError,
    pub observation: Option<Box<ReceiptObservation>>,
}
impl From<RpcServerError> for NativeReadError {
    fn from(error: RpcServerError) -> Self {
        Self {
            error,
            observation: None,
        }
    }
}

/// Submit exactly once while carrying attempt evidence through queueing and dispatch.
#[async_trait]
pub trait NativeSubmitPlane: Send + Sync + 'static {
    async fn native_submit(
        &self,
        context: RpcRequestContext,
        tx: ValidatedTransaction,
        attempt: SubmissionAttempt,
    ) -> Result<(), RpcServerError>;
}

/// Transaction reads validated before lossy legacy projection.
#[async_trait]
pub trait NativeTransactionReadPlane: Send + Sync + 'static {
    async fn native_input(
        &self,
        context: RpcRequestContext,
        hash: TxHash,
    ) -> Result<responses::TransactionInputResult, NativeReadError>;
    async fn native_receipt(
        &self,
        context: RpcRequestContext,
        hash: TxHash,
    ) -> Result<Option<responses::TransactionReceipt>, NativeReadError>;
    async fn native_status(
        &self,
        context: RpcRequestContext,
        hash: TxHash,
    ) -> Result<responses::TransactionStatus, NativeReadError>;
}

/// Keep one read deadline without stripping validated observation on backend errors.
pub async fn within_native_read_deadline<T>(
    context: RpcRequestContext,
    future: impl std::future::Future<Output = Result<T, NativeReadError>>,
) -> Result<T, NativeReadError> {
    if let Some(deadline) = context.deadline() {
        tokio::time::timeout_at(deadline.into(), future)
            .await
            .map_err(|_| NativeReadError::from(RpcServerError::BackendTimeout))?
    } else {
        future.await
    }
}
