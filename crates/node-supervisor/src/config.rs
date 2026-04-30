//! Configuration for the node supervisor.

use std::{path::PathBuf, time::Duration};

/// Default JSON-RPC listen address for `groundhog node`.
pub const DEFAULT_RPC_LISTEN_ADDRESS: &str = "127.0.0.1:9545";

/// Default listen host passed to hogd-dev for internal services.
pub const DEFAULT_HOGD_DEV_LISTEN_HOST: &str = "127.0.0.1";

/// Default graceful shutdown timeout for ephemeral hogd-dev.
pub const DEFAULT_HOGD_DEV_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Default startup timeout while waiting for hogd-dev readiness.
pub const DEFAULT_HOGD_DEV_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Supervisor configuration for a local Groundhog node.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeSupervisorConfig {
    /// Public JSON-RPC listen address exposed by the Rust facade.
    pub rpc_listen_address: String,
    /// Configuration for the supervised hogd-dev child process.
    pub hogd_dev: HogdDevConfig,
}

impl Default for NodeSupervisorConfig {
    fn default() -> Self {
        Self {
            rpc_listen_address: DEFAULT_RPC_LISTEN_ADDRESS.to_string(),
            hogd_dev: HogdDevConfig::default(),
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
}

impl Default for HogdDevConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            listen_host: DEFAULT_HOGD_DEV_LISTEN_HOST.to_string(),
            shutdown_grace: DEFAULT_HOGD_DEV_SHUTDOWN_GRACE,
            startup_timeout: DEFAULT_HOGD_DEV_STARTUP_TIMEOUT,
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
}
