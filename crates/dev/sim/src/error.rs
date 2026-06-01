/// Errors returned by the simulation harness.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("{0}")]
    Validation(#[from] crate::validation::ValidationError),
    #[error("{0}")]
    Catalog(#[from] crate::catalog::CatalogError),
    #[error("{0}")]
    RuntimeManifest(#[from] crate::runtime::RuntimeManifestError),
    #[error("{0}")]
    Actor(#[from] crate::actor::ActorError),
    #[error("{0}")]
    Nonce(#[from] crate::nonce::NonceError),
    #[error("{0}")]
    TransactionBuild(#[from] crate::transaction::TransactionBuildError),
    #[error("{0}")]
    Schedule(#[from] crate::scheduler::ScheduleError),
    #[error("{0}")]
    EventWrite(#[from] crate::event::EventWriteError),
    #[error("failed filesystem operation: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse or write JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "execution does not yet support these workloads: {0}. Supported workloads are \
         chain-tip, wallet-submit-smoke, receipt-poll, contract-call, view-call, \
         storage-read, events-query, and read-after-write."
    )]
    UnsupportedRuntimeWorkloads(String),
    #[error("failed to build JSON-RPC client: {0}")]
    RpcClient(String),
    #[error(
        "groundhog binary `{binary}` was not found; pass --groundhog-binary to hoglab run for local targets"
    )]
    GroundhogBinaryNotFound { binary: String },
    #[error("failed to spawn local groundhog node `{binary}`: {source}")]
    LocalNodeSpawn {
        binary: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "local groundhog node did not emit startup JSON within {timeout_secs}s; stderr: {stderr}"
    )]
    LocalNodeStartupTimeout { timeout_secs: u64, stderr: String },
    #[error("local groundhog node exited before startup JSON; status: {status}; stderr: {stderr}")]
    LocalNodeStartupExit { status: String, stderr: String },
    #[error("local groundhog node startup JSON is missing field `{field}`")]
    LocalNodeStartupJsonMissing { field: &'static str },
    #[error("local groundhog node startup JSON is invalid: {source}")]
    LocalNodeStartupJson {
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to signal local groundhog node pid {pid}: {source}")]
    LocalNodeSignal {
        pid: u32,
        #[source]
        source: std::io::Error,
    },
    #[error("local restart requires an acquired node")]
    LocalNodeRestartWithoutAcquiredTarget,
    #[error("local node startup rpc_url `{rpc_url}` is not a supported host:port URL")]
    LocalNodeInvalidRpcUrl { rpc_url: String },
    #[error("local restart resolved rpc_url `{actual}` but expected stable rpc_url `{expected}`")]
    LocalNodeRestartRpcUrlChanged { expected: String, actual: String },
    #[error("local target provider already has an acquired node; cleanup before acquiring again")]
    LocalNodeAlreadyAcquired,
    #[error("executor error: {0}")]
    Executor(String),
    #[error("workload task panicked for actor {actor_id} {workload:?}: {payload}")]
    WorkloadPanic {
        actor_id: u32,
        workload: crate::manifest::WorkloadKind,
        payload: String,
    },
    #[error("run completed with failed workloads; result directory: {0}")]
    RunFailed(String),
    #[error("system clock is before Unix epoch")]
    ClockBeforeEpoch,
    #[error("unsupported result schemaVersion {actual} in {path}")]
    UnsupportedResultSchema { actual: u32, path: String },
    #[error("serialized field `{field}` was not a JSON string")]
    JsonStringField { field: &'static str },
}
