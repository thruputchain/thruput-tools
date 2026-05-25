/// Errors returned by the simulation harness.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("{0}")]
    Validation(#[from] crate::validation::ValidationError),
    #[error("{0}")]
    RuntimeManifest(#[from] crate::runtime::RuntimeManifestError),
    #[error("{0}")]
    Nonce(#[from] crate::nonce::NonceError),
    #[error("{0}")]
    Schedule(#[from] crate::scheduler::ScheduleError),
    #[error("{0}")]
    EventWrite(#[from] crate::event::EventWriteError),
    #[error("failed filesystem operation: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse or write JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "Phase 1 execution currently supports only chain-tip workloads; unsupported workloads: {0}"
    )]
    UnsupportedRuntimeWorkloads(String),
    #[error("failed to build JSON-RPC client: {0}")]
    RpcClient(String),
    #[error("executor error: {0}")]
    Executor(String),
    #[error("run completed with failed workloads; result directory: {0}")]
    RunFailed(String),
    #[error("existing target requires an RPC URL")]
    MissingRpcUrl,
    #[error("existing target requires a runtime manifest path")]
    MissingRuntimeManifest,
    #[error("system clock is before Unix epoch")]
    ClockBeforeEpoch,
    #[error("unsupported result schemaVersion {actual} in {path}")]
    UnsupportedResultSchema { actual: u32, path: String },
}
