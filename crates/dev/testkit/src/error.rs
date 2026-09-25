//! Error type for the test harness.

use std::fmt;

use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};
use groundhog_node_supervisor::BlockProductionError;

/// Result alias used throughout `groundhog-testkit`.
pub type Result<T, E = TestkitError> = std::result::Result<T, E>;

/// One agent error retained after every non-panicking agent has finished.
#[derive(Debug)]
pub struct AgentFailure {
    name: String,
    index: usize,
    error: TestkitError,
}

impl AgentFailure {
    pub(crate) fn new(name: String, index: usize, error: TestkitError) -> Self {
        Self { name, index, error }
    }

    /// Name of the failed agent.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Position of the failed agent in the scenario's agent list.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Error returned by the agent body.
    pub fn error(&self) -> &TestkitError {
        &self.error
    }
}

/// Ordered collection of errors returned by agents in one scenario run.
#[derive(Debug)]
pub struct AgentFailures {
    failures: Vec<AgentFailure>,
}

impl AgentFailures {
    pub(crate) fn new(failures: Vec<AgentFailure>) -> Self {
        debug_assert!(!failures.is_empty());
        Self { failures }
    }

    /// Agent failures ordered by their position in the scenario's agent list.
    pub fn failures(&self) -> &[AgentFailure] {
        &self.failures
    }
}

impl fmt::Display for AgentFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.failures.len();
        write!(
            f,
            "{count} agent{} failed",
            if count == 1 { "" } else { "s" }
        )?;
        for failure in &self.failures {
            write!(
                f,
                "; {} (index {}): {}",
                failure.name, failure.index, failure.error
            )?;
        }
        Ok(())
    }
}

/// Errors returned by the harness while building or running a scenario.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TestkitError {
    /// The caller has not asserted support for the new deployment ABI.
    #[error("native deployment requires experimental_native_deploy() and a supporting runtime")]
    NativeDeploymentUnsupported,
    /// Existing legacy wallet identities cannot use native deployment semantics.
    #[error("legacy bootstrap accounts support calls only; deploy from a native account")]
    LegacyDeploymentUnsupported,
    /// The runtime manifest has no faucet account to fund test accounts.
    #[error("runtime manifest has no bootstrap faucet")]
    MissingFaucet,
    /// A receipt belongs to another transaction.
    #[error("receipt hash does not match {expected:?}")]
    ReceiptHashMismatch {
        /// Locally computed transaction identity.
        expected: TxHash,
    },
    /// An ambiguous submission retains the exact original signed transaction.
    #[error("submission of {tx_hash:?} was not verified: {detail}")]
    Submission {
        /// Locally computed transaction identity.
        tx_hash: TxHash,
        /// Exact signed bytes. Observe the original hash before any further action.
        signed_transaction: String,
        /// Transport error or unexpected returned identity.
        detail: String,
    },
    /// A submitted deployment could not be verified; preserve the original attempt.
    #[error("deployment {tx_hash:?} at {predicted_address:?} was not verified: {detail}")]
    UnverifiedDeployment {
        /// Locally computed transaction identity.
        tx_hash: TxHash,
        /// Address predicted from the signed deployment effects.
        predicted_address: groundhog_xdr::types::ContractAddress,
        /// Exact original signed bytes for reconciliation.
        signed_transaction: String,
        /// Failed receipt lookup or verification.
        detail: String,
    },
    /// Retained deployment effects differ from a new request by the same caller.
    #[error("artifact {name} has a retained deployment with different funding or initialization")]
    DeploymentRequestMismatch {
        /// Artifact with the retained attempt.
        name: String,
    },
    /// A scenario was built without a network configuration.
    #[error("no network configured for the scenario")]
    NoNetwork,

    /// A JSON-RPC call to the target failed.
    #[error("rpc `{method}` failed: {detail}")]
    Rpc {
        /// The RPC method that failed.
        method: &'static str,
        /// Underlying error description.
        detail: String,
    },

    /// A chain operation was attempted against an offline target.
    #[error("operation `{operation}` requires a live target, but this scenario is offline")]
    Offline {
        /// The attempted operation.
        operation: &'static str,
    },

    /// The runtime manifest could not be read or parsed.
    #[error("failed to load runtime manifest `{path}`: {detail}")]
    ManifestLoad {
        /// Manifest path.
        path: String,
        /// Underlying error description.
        detail: String,
    },

    /// The runtime manifest declared an unsupported schema version.
    #[error("unsupported runtime manifest schema version {actual}")]
    ManifestSchema {
        /// The version found in the manifest.
        actual: u32,
    },

    /// A field could not be read from the runtime manifest.
    #[error("runtime manifest field `{field}` is invalid: {detail}")]
    Manifest {
        /// The manifest field at fault.
        field: &'static str,
        /// Underlying error description.
        detail: String,
    },

    /// The manifest used a bootstrap-account derivation scheme we do not support.
    #[error("unsupported bootstrap-account derivation scheme `{scheme}`")]
    UnsupportedDerivationScheme {
        /// The scheme string found in the manifest.
        scheme: String,
    },

    /// The manifest carried no bootstrap accounts to assign to agents.
    #[error("runtime manifest declares no bootstrap accounts")]
    NoBootstrapAccounts,

    /// A derived signer did not match the public key the manifest advertised.
    #[error("bootstrap account {index} public key does not match the derived signer")]
    BootstrapPublicKeyMismatch {
        /// The bootstrap account index at fault.
        index: u32,
    },

    /// The scenario has fewer accounts than it has agents.
    #[error("scenario needs {needed} accounts but only {available} are available")]
    NotEnoughAccounts {
        /// Number of agents that need accounts.
        needed: usize,
        /// Number of accounts available.
        available: usize,
    },

    /// Two agents in one run were given the same name.
    #[error("agent name `{name}` appears more than once in the scenario")]
    DuplicateAgent {
        /// The duplicated agent name.
        name: String,
    },

    /// One or more agent bodies returned errors.
    #[error("{failures}")]
    AgentsFailed {
        /// All returned agent errors, ordered by agent position.
        failures: AgentFailures,
    },

    /// An account exhausted its per-account nonce sequence.
    #[error("account {index} exhausted its nonce sequence")]
    NonceExhausted {
        /// The account index at fault.
        index: u32,
    },

    /// Building or signing a transaction failed.
    #[error("transaction build/sign failed: {detail}")]
    Signing {
        /// Underlying error description.
        detail: String,
    },

    /// Encoding calldata or a contract failed.
    #[error("xdr encoding failed: {detail}")]
    Encode {
        /// Underlying error description.
        detail: String,
    },

    /// Decoding a contract return value failed.
    #[error("xdr decoding failed: {detail}")]
    Decode {
        /// Underlying error description.
        detail: String,
    },

    /// A contract interface version is not valid `SemVer`.
    #[error("contract interface version `{version}` is not valid SemVer")]
    InvalidContractVersion {
        /// Version string used to derive the method selector.
        version: String,
    },

    /// A deploy transaction did not execute successfully.
    #[error("deploy did not succeed: {detail}")]
    Deploy {
        /// Description of the failing execution status.
        detail: String,
    },

    /// A contract call executed but its status was not `Ok`.
    ///
    /// Carries the method name; add call-site context with `map_err` where
    /// that alone does not identify which call reverted.
    #[error("contract call to `{method}` reverted: {status}")]
    Reverted {
        /// The method that was called.
        method: String,
        /// Description of the failing execution status.
        status: String,
    },

    /// A contract call or view did not return successfully.
    #[error("contract {kind} did not return a value: {detail}")]
    NoReturn {
        /// Whether it was a `call` or a `view`.
        kind: &'static str,
        /// Description of the non-returning status.
        detail: String,
    },

    /// A receipt did not appear within the polling budget.
    ///
    /// `status` reports what `getTransactionStatus` last knew about the
    /// transaction, which usually explains the missing receipt: a scheduler
    /// rejection, or a two-phase-commit conflict (for example a balance that
    /// could not cover its own storage cost) that kept it out of every block.
    #[error("timed out waiting for receipt of `{tx_hash}` ({status})")]
    ReceiptTimeout {
        /// The transaction hash that never produced a receipt.
        tx_hash: String,
        /// The last observed transaction status, as a human-readable summary.
        status: String,
    },

    /// A submitted transaction did not become pending before the polling budget elapsed.
    #[error("timed out waiting for `{tx_hash}` to become pending (last status: {last_status:?})")]
    PendingTimeout {
        /// Transaction hash that never appeared as pending.
        tx_hash: String,
        /// Last status observed before timeout.
        last_status: TransactionStatus,
    },

    /// A submitted transaction reached a terminal or unexpected state before becoming pending.
    #[error("transaction `{tx_hash}` did not become pending: {status:?}")]
    TransactionNotPending {
        /// Transaction hash whose status was observed.
        tx_hash: String,
        /// Status observed instead of `Pending`.
        status: TransactionStatus,
    },

    /// Manual block production is unavailable for this target or driver mode.
    #[error("manual block production requires a harness-owned local node in manual mode")]
    ManualBlockProductionUnavailable,

    /// The local supervisor failed to produce a manually-requested block.
    #[error("manual block production failed: {source}")]
    BlockProduction {
        /// Underlying supervisor block-production error.
        #[source]
        source: BlockProductionError,
    },

    /// A contract artifact could not be read from disk.
    #[error("failed to load artifact `{name}` from `{path}`: {detail}")]
    ArtifactLoad {
        /// Registry name of the artifact.
        name: String,
        /// Path that failed to load.
        path: String,
        /// Underlying error description.
        detail: String,
    },

    /// A file artifact had an extension the harness does not recognize.
    #[error("cannot infer artifact kind from `{path}` (expected .rv32a)")]
    UnknownArtifactExtension {
        /// The path whose extension was unrecognized.
        path: String,
    },

    /// Two artifacts were registered under the same name.
    #[error("artifact `{name}` is registered more than once")]
    DuplicateArtifact {
        /// The duplicated artifact name.
        name: String,
    },

    /// An agent referenced an artifact name that was never registered.
    #[error("no artifact named `{name}` was registered with the scenario")]
    UnknownArtifact {
        /// The unrecognized artifact name.
        name: String,
    },

    /// An artifact registration transaction is still awaiting a receipt.
    #[error("artifact `{name}` registration transaction `{tx_hash:?}` is still pending")]
    ArtifactRegistrationPending {
        /// Registry artifact name.
        name: String,
        /// Transaction currently registering the artifact.
        tx_hash: TxHash,
    },

    /// The local node supervisor failed to start or stop the target.
    #[error("local node supervisor error: {detail}")]
    NodeSupervisor {
        /// Underlying error description.
        detail: String,
    },

    /// A started local node did not advertise a runtime manifest.
    #[error("local node did not advertise a runtime manifest")]
    MissingRuntimeManifest,

    /// A message was addressed to an agent that is not part of the scenario.
    #[error("no agent named `{name}` in the scenario roster")]
    UnknownAgent {
        /// The unrecognized recipient name.
        name: String,
    },

    /// The recipient agent's mailbox was already closed.
    #[error("mailbox for agent `{name}` is closed")]
    MailboxClosed {
        /// The recipient whose mailbox is closed.
        name: String,
    },

    /// An agent task failed to join for a reason other than a panic.
    #[error("agent task failed to join: {detail}")]
    AgentJoin {
        /// Description of the join failure.
        detail: String,
    },
}

impl TestkitError {
    /// Build an [`TestkitError::Rpc`] from a failing call.
    pub(crate) fn rpc(method: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Rpc {
            method,
            detail: error.to_string(),
        }
    }

    pub(crate) fn signing(error: impl std::fmt::Display) -> Self {
        Self::Signing {
            detail: error.to_string(),
        }
    }

    pub(crate) fn encode(error: impl std::fmt::Display) -> Self {
        Self::Encode {
            detail: error.to_string(),
        }
    }

    pub(crate) fn decode(error: impl std::fmt::Display) -> Self {
        Self::Decode {
            detail: error.to_string(),
        }
    }
}
