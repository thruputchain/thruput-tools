//! Dev node error types.

use groundhog_rpc_api_types::error::codes;
use jsonrpsee::types::ErrorObjectOwned;

/// Errors returned by the dev node backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DevNodeError {
    /// Transaction XDR was malformed.
    #[error("invalid transaction: {0}")]
    TxInvalid(String),

    /// Auth verification failed.
    #[error("auth verification failed: {0}")]
    AuthFailed(String),

    /// Contract execution reverted or faulted.
    #[error("execution failed: {0}")]
    ExecFailed(String),

    /// Transaction hash not found in receipts.
    #[error("transaction not found")]
    TxNotFound,

    /// Worker thread failed during startup.
    #[error("dev node worker failed to initialize: {0}")]
    WorkerInit(String),

    /// Worker startup failed because the host environment (sandbox /
    /// process isolation) blocks `native_exec` from opening its embedded
    /// database.
    #[error("dev node worker failed to initialize: {0}")]
    SandboxInit(String),

    /// Worker thread has stopped.
    #[error("dev node worker stopped")]
    WorkerStopped,
}

impl From<DevNodeError> for ErrorObjectOwned {
    fn from(err: DevNodeError) -> Self {
        let (code, msg) = match &err {
            DevNodeError::TxInvalid(_) | DevNodeError::AuthFailed(_) => {
                (codes::TX_INVALID, err.to_string())
            }
            DevNodeError::ExecFailed(_) => (codes::TX_REJECTED, err.to_string()),
            DevNodeError::TxNotFound => (codes::TX_NOT_FOUND, err.to_string()),
            DevNodeError::WorkerInit(_)
            | DevNodeError::SandboxInit(_)
            | DevNodeError::WorkerStopped => (codes::BACKEND_UNAVAILABLE, err.to_string()),
        };
        ErrorObjectOwned::owned(code, msg, None::<()>)
    }
}
