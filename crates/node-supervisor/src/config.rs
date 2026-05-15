//! Configuration for the node supervisor.

use std::{path::PathBuf, time::Duration};

use groundhog_signing::{
    DEFAULT_DEV_ACCOUNT_COUNT, DEFAULT_DEV_ACCOUNT_SEED, DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS,
};

/// Default JSON-RPC listen address for `groundhog node`.
pub const DEFAULT_RPC_LISTEN_ADDRESS: &str = "127.0.0.1:9545";

/// Default timeout for one hogd-dev `ViewGateway` execution RPC.
pub const DEFAULT_HOGD_DEV_VIEW_TIMEOUT: Duration = Duration::from_secs(30);

/// Default listen host passed to hogd-dev for internal services.
pub const DEFAULT_HOGD_DEV_LISTEN_HOST: &str = "127.0.0.1";

/// Default graceful shutdown timeout for ephemeral hogd-dev.
pub const DEFAULT_HOGD_DEV_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Default startup timeout while waiting for hogd-dev readiness.
pub const DEFAULT_HOGD_DEV_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Default number of dev accounts requested from hogd-dev.
pub const DEFAULT_HOGD_DEV_ACCOUNT_COUNT: usize = DEFAULT_DEV_ACCOUNT_COUNT;

/// Default dev account signer seed requested from hogd-dev.
pub const DEFAULT_HOGD_DEV_ACCOUNT_SEED: &str = DEFAULT_DEV_ACCOUNT_SEED;

/// Default storage-token funding target requested from hogd-dev per account.
pub const DEFAULT_HOGD_DEV_ACCOUNT_TOKENS: u64 = DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS;

/// Supervisor configuration for a local Groundhog node.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeSupervisorConfig {
    /// Public JSON-RPC listen address exposed by the Rust facade.
    pub rpc_listen_address: String,
    /// Configuration for the supervised hogd-dev child process.
    pub hogd_dev: HogdDevConfig,
    /// Optional protocol string used for hogd-dev intro handshakes.
    ///
    /// Defaults to the protocol embedded in the linked `groundhog-xdr` crate.
    /// This override is intended for local cross-repo smoke tests before a
    /// matching Rust protocol crate has been published.
    pub hogd_protocol_override: Option<String>,
    /// Gas budget assigned to hogd-dev `ViewGateway` root invocations.
    pub view_gas_limit: u64,
    /// Maximum time to wait for one hogd-dev `ViewGateway` execution RPC.
    pub view_timeout: Duration,
}

impl Default for NodeSupervisorConfig {
    fn default() -> Self {
        Self {
            rpc_listen_address: DEFAULT_RPC_LISTEN_ADDRESS.to_string(),
            hogd_dev: HogdDevConfig::default(),
            hogd_protocol_override: None,
            view_gas_limit: groundhog_rpc_server::DEFAULT_VIEW_GAS_LIMIT,
            view_timeout: DEFAULT_HOGD_DEV_VIEW_TIMEOUT,
        }
    }
}

impl NodeSupervisorConfig {
    /// Return a copy with a different public JSON-RPC listen address.
    #[must_use]
    pub fn with_rpc_listen_address(mut self, listen_address: impl Into<String>) -> Self {
        self.rpc_listen_address = listen_address.into();
        self
    }

    /// Return a copy with an explicit hogd-dev binary path.
    #[must_use]
    pub fn with_hogd_dev_binary(mut self, binary_path: impl Into<PathBuf>) -> Self {
        self.hogd_dev.binary_path = Some(binary_path.into());
        self
    }

    /// Return a copy with the supplied hogd-dev configuration.
    #[must_use]
    pub fn with_hogd_dev(mut self, hogd_dev: HogdDevConfig) -> Self {
        self.hogd_dev = hogd_dev;
        self
    }

    /// Return a copy with an explicit hogd protocol string for intro probes.
    ///
    /// This is a development escape hatch. It should only be used when the
    /// caller has verified that the linked Rust protocol structs are compatible
    /// with the hogd-dev binary being tested.
    #[must_use]
    pub fn with_hogd_protocol_override(mut self, protocol: impl Into<String>) -> Self {
        self.hogd_protocol_override = Some(protocol.into());
        self
    }

    /// Return a copy with a different hogd-dev `ViewGateway` gas budget.
    #[must_use]
    pub fn with_view_gas_limit(mut self, view_gas_limit: u64) -> Self {
        self.view_gas_limit = view_gas_limit;
        self
    }

    /// Return a copy with a different hogd-dev `ViewGateway` RPC timeout.
    #[must_use]
    pub fn with_view_timeout(mut self, view_timeout: Duration) -> Self {
        self.view_timeout = view_timeout;
        self
    }
}

/// Configuration for the supervised hogd-dev process.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HogdDevConfig {
    /// Explicit binary path, if supplied by CLI flag or higher-level config.
    ///
    /// When unset, the supervisor falls back to the documented hogd-dev binary
    /// lookup order.
    pub binary_path: Option<PathBuf>,
    /// Host address hogd-dev should bind its internal services to.
    pub listen_host: String,
    /// Grace period after SIGTERM before the supervisor escalates to SIGKILL.
    ///
    /// v1 hogd-dev is ephemeral, so this exists for log drain and orderly task
    /// cancellation rather than state persistence.
    pub shutdown_grace: Duration,
    /// Maximum time to wait for the readiness line after spawning hogd-dev.
    pub startup_timeout: Duration,
    /// Number of deterministic dev accounts hogd-dev should bootstrap.
    pub dev_account_count: usize,
    /// Seed used by hogd-dev and the supervisor to derive dev-account signers.
    pub dev_account_seed: String,
    /// Storage-token funding target per dev account.
    pub dev_account_tokens: u64,
    /// PID file used to detect and clean up orphaned hogd-dev children.
    pub pid_file: Option<PathBuf>,
}

impl Default for HogdDevConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            listen_host: DEFAULT_HOGD_DEV_LISTEN_HOST.to_string(),
            shutdown_grace: DEFAULT_HOGD_DEV_SHUTDOWN_GRACE,
            startup_timeout: DEFAULT_HOGD_DEV_STARTUP_TIMEOUT,
            dev_account_count: DEFAULT_HOGD_DEV_ACCOUNT_COUNT,
            dev_account_seed: DEFAULT_HOGD_DEV_ACCOUNT_SEED.to_string(),
            dev_account_tokens: DEFAULT_HOGD_DEV_ACCOUNT_TOKENS,
            pid_file: None,
        }
    }
}

impl HogdDevConfig {
    /// Return a copy with an explicit hogd-dev binary path.
    #[must_use]
    pub fn with_binary_path(mut self, binary_path: impl Into<PathBuf>) -> Self {
        self.binary_path = Some(binary_path.into());
        self
    }

    /// Return a copy with a different internal listen host.
    #[must_use]
    pub fn with_listen_host(mut self, listen_host: impl Into<String>) -> Self {
        self.listen_host = listen_host.into();
        self
    }

    /// Return a copy with a different graceful shutdown timeout.
    #[must_use]
    pub fn with_shutdown_grace(mut self, shutdown_grace: Duration) -> Self {
        self.shutdown_grace = shutdown_grace;
        self
    }

    /// Return a copy with a different startup readiness timeout.
    #[must_use]
    pub fn with_startup_timeout(mut self, startup_timeout: Duration) -> Self {
        self.startup_timeout = startup_timeout;
        self
    }

    /// Return a copy with a different dev-account count.
    #[must_use]
    pub fn with_dev_account_count(mut self, dev_account_count: usize) -> Self {
        self.dev_account_count = dev_account_count;
        self
    }

    /// Return a copy with a different dev-account derivation seed.
    #[must_use]
    pub fn with_dev_account_seed(mut self, dev_account_seed: impl Into<String>) -> Self {
        self.dev_account_seed = dev_account_seed.into();
        self
    }

    /// Return a copy with a different dev-account storage-token funding target.
    #[must_use]
    pub fn with_dev_account_tokens(mut self, dev_account_tokens: u64) -> Self {
        self.dev_account_tokens = dev_account_tokens;
        self
    }

    /// Return a copy with a PID file for orphan detection.
    #[must_use]
    pub fn with_pid_file(mut self, pid_file: impl Into<PathBuf>) -> Self {
        self.pid_file = Some(pid_file.into());
        self
    }
}
