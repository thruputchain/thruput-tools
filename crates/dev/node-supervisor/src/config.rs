//! Configuration for the node supervisor.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use groundhog_json_rpc_types::NetworkHashHex;
use groundhog_json_rpc_types::bootstrap_manifest::{
    BootstrapManifest, BootstrapManifestAccounts, BootstrapManifestDevWallet,
    BootstrapManifestFaucet, BootstrapManifestStorageCostModel,
};
use groundhog_signing::{
    DEFAULT_DEV_ACCOUNT_COUNT, DEFAULT_DEV_ACCOUNT_SEED, DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS,
};

/// Default JSON-RPC listen address for `groundhog node`.
pub const DEFAULT_RPC_LISTEN_ADDRESS: &str = "127.0.0.1:9545";

/// Default maximum simultaneous public JSON-RPC connections for `groundhog node`.
pub const DEFAULT_RPC_MAX_CONNECTIONS: u32 = 2_000;

/// Default timeout for one hogd-dev `ViewGateway` execution RPC.
pub const DEFAULT_HOGD_DEV_VIEW_TIMEOUT: Duration = Duration::from_secs(30);

/// Default interval for coordinator-like local block production.
pub const DEFAULT_BLOCK_DRIVER_INTERVAL: Duration = Duration::from_millis(500);

/// Default deadline for one explicitly requested block-production cycle.
pub const DEFAULT_MANUAL_BLOCK_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default listen host passed to hogd-dev for internal services.
pub const DEFAULT_HOGD_DEV_LISTEN_HOST: &str = "127.0.0.1";

/// Default graceful shutdown timeout for ephemeral hogd-dev.
pub const DEFAULT_HOGD_DEV_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Default graceful shutdown timeout for hogd-dev with persistent state.
pub const DEFAULT_HOGD_DEV_PERSISTENT_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// Default startup timeout while waiting for hogd-dev readiness.
pub const DEFAULT_HOGD_DEV_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Default number of local-user accounts requested from hogd-dev, excluding the faucet.
pub const DEFAULT_HOGD_DEV_ACCOUNT_COUNT: usize = DEFAULT_DEV_ACCOUNT_COUNT;

/// Default dev account signer seed requested from hogd-dev.
pub const DEFAULT_HOGD_DEV_ACCOUNT_SEED: &str = DEFAULT_DEV_ACCOUNT_SEED;

/// Fixed local faucet allocation in raw HOG units.
pub const LOCAL_FAUCET_STORAGE_TOKENS: u64 = 10_000_000_000_000;

/// Default local-user allocation in raw HOG units.
pub const DEFAULT_HOGD_DEV_ACCOUNT_TOKENS: u64 = DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS;

/// Fixed local faucet derivation index.
pub const LOCAL_FAUCET_INDEX: u32 = 0;

/// First local-user derivation index.
pub const FIRST_LOCAL_USER_INDEX: u32 = 1;

/// Bootstrap manifest kind emitted for hogd-dev shorthand lowering.
pub const HOGD_DEV_BOOTSTRAP_MANIFEST_KIND: &str = "groundhog-bootstrap";

/// Default bootstrap profile requested from hogd-dev.
pub const DEFAULT_HOGD_DEV_BOOTSTRAP_PROFILE: &str = "local-dev";

/// Default dev-wallet profile requested from hog-devnode.
pub const DEFAULT_HOGD_DEV_WALLET_PROFILE: &str = "hog-devnode-default";

/// Default storage-cost model profile requested from hogd-dev.
pub const DEFAULT_HOGD_DEV_STORAGE_COST_MODEL_PROFILE: &str = "hogd-default";

/// Default local airdrop amount: 10 HOG in indivisible units.
pub const DEFAULT_LOCAL_AIRDROP_RAW_AMOUNT: u128 = 10 * groundhog_signing::NATIVE_HOG_SCALE;

/// Resource bounds and transfer amount for the optional local airdrop RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalAirdropConfig {
    /// Native amount transferred by each logical attempt.
    pub raw_amount: u128,
    /// Maximum retained process-local attempt records.
    pub attempt_capacity: usize,
    /// Maximum concurrently advancing attempts.
    pub max_concurrency: usize,
    /// End-to-end backend deadline for one attempt advancement.
    pub request_timeout: Duration,
}

impl Default for LocalAirdropConfig {
    fn default() -> Self {
        Self {
            raw_amount: DEFAULT_LOCAL_AIRDROP_RAW_AMOUNT,
            attempt_capacity: 1_024,
            max_concurrency: 16,
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl LocalAirdropConfig {
    pub(crate) fn validate(self) -> Result<Self, &'static str> {
        if self.raw_amount == 0 {
            return Err("raw amount must be greater than zero");
        }
        if self.attempt_capacity == 0 {
            return Err("attempt capacity must be greater than zero");
        }
        if self.max_concurrency == 0 {
            return Err("maximum concurrency must be greater than zero");
        }
        if self.request_timeout.is_zero() {
            return Err("request timeout must be greater than zero");
        }
        Ok(self)
    }
}

/// Supervisor configuration for a local Groundhog node.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeSupervisorConfig {
    /// Public JSON-RPC listen address exposed by the Rust facade.
    pub rpc_listen_address: String,
    /// Maximum simultaneous public JSON-RPC connections.
    pub rpc_max_connections: u32,
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
    /// Local block production policy for hogd-dev scheduler coordination.
    pub block_driver: BlockDriverConfig,
    /// Optional local airdrop behavior and resource bounds.
    pub local_airdrop: LocalAirdropConfig,
}

impl Default for NodeSupervisorConfig {
    fn default() -> Self {
        Self {
            rpc_listen_address: DEFAULT_RPC_LISTEN_ADDRESS.to_string(),
            rpc_max_connections: DEFAULT_RPC_MAX_CONNECTIONS,
            hogd_dev: HogdDevConfig::default(),
            hogd_protocol_override: None,
            view_gas_limit: groundhog_json_rpc_server::DEFAULT_VIEW_GAS_LIMIT,
            view_timeout: DEFAULT_HOGD_DEV_VIEW_TIMEOUT,
            block_driver: BlockDriverConfig::default(),
            local_airdrop: LocalAirdropConfig::default(),
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

    /// Return a copy with a different public JSON-RPC connection cap.
    #[must_use]
    pub const fn with_rpc_max_connections(mut self, max_connections: u32) -> Self {
        self.rpc_max_connections = max_connections;
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

    /// Return a copy with a different local block production policy.
    #[must_use]
    pub fn with_block_driver(mut self, block_driver: BlockDriverConfig) -> Self {
        self.block_driver = block_driver;
        self
    }

    /// Return a copy with different local airdrop controls.
    #[must_use]
    pub const fn with_local_airdrop(mut self, local_airdrop: LocalAirdropConfig) -> Self {
        self.local_airdrop = local_airdrop;
        self
    }
}

/// Local block production policy used by the hogd-dev supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BlockDriverConfig {
    /// Driver wake mode.
    pub mode: BlockDriverMode,
    /// Timer interval for [`BlockDriverMode::Interval`].
    pub interval: Duration,
    /// Execution deadline once a request reaches the manual driver.
    ///
    /// Time spent waiting behind an earlier request is not included.
    pub manual_request_timeout: Duration,
}

impl BlockDriverConfig {
    /// Return an instant submit-wake policy.
    #[must_use]
    pub const fn instant() -> Self {
        Self {
            mode: BlockDriverMode::Instant,
            interval: DEFAULT_BLOCK_DRIVER_INTERVAL,
            manual_request_timeout: DEFAULT_MANUAL_BLOCK_REQUEST_TIMEOUT,
        }
    }

    /// Return a fixed-interval policy.
    #[must_use]
    pub const fn interval(interval: Duration) -> Self {
        Self {
            mode: BlockDriverMode::Interval,
            interval,
            manual_request_timeout: DEFAULT_MANUAL_BLOCK_REQUEST_TIMEOUT,
        }
    }

    /// Return an explicitly-triggered block production policy.
    #[must_use]
    pub const fn manual() -> Self {
        Self {
            mode: BlockDriverMode::Manual,
            interval: DEFAULT_BLOCK_DRIVER_INTERVAL,
            manual_request_timeout: DEFAULT_MANUAL_BLOCK_REQUEST_TIMEOUT,
        }
    }

    /// Return a copy with a different mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: BlockDriverMode) -> Self {
        self.mode = mode;
        self
    }

    /// Return a copy with a different interval.
    #[must_use]
    pub const fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Return a copy with a different manual request deadline.
    #[must_use]
    pub const fn with_manual_request_timeout(mut self, timeout: Duration) -> Self {
        self.manual_request_timeout = timeout;
        self
    }

    pub(crate) fn effective_interval(self) -> Duration {
        if self.interval.is_zero() {
            DEFAULT_BLOCK_DRIVER_INTERVAL
        } else {
            self.interval
        }
    }

    pub(crate) fn effective_manual_request_timeout(self) -> Duration {
        if self.manual_request_timeout.is_zero() {
            DEFAULT_MANUAL_BLOCK_REQUEST_TIMEOUT
        } else {
            self.manual_request_timeout
        }
    }
}

impl Default for BlockDriverConfig {
    fn default() -> Self {
        Self::instant()
    }
}

/// Block driver wake mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockDriverMode {
    /// Submit-plane successes wake the block driver immediately.
    #[default]
    Instant,
    /// A fixed timer drives nomination/commit attempts.
    Interval,
    /// Blocks are produced only through the supervisor's in-process control handle.
    Manual,
}

/// Log verbosity level for hog-devnode
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogLevel {
    Critical,
    Error,
    #[default]
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(&self) -> &str {
        match self {
            LogLevel::Critical => "critical",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
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
    /// Directory where hog-devnode persists recoverable node state.
    ///
    /// When unset, hog-devnode runs with ephemeral state.
    pub data_dir: Option<PathBuf>,
    /// Log level to request from hogd-dev
    pub log_level: Option<LogLevel>,
    /// Host address hogd-dev should bind its internal services to.
    pub listen_host: String,
    /// Grace period after SIGTERM before the supervisor escalates to SIGKILL.
    ///
    /// This allows log drain and orderly task cancellation before forced
    /// process termination.
    pub shutdown_grace: Duration,
    /// Maximum time to wait for the readiness line after spawning hogd-dev.
    pub startup_timeout: Duration,
    /// Number of deterministic local-user accounts hogd-dev should bootstrap.
    ///
    /// The always-present faucet at `dev-0` is excluded.
    pub dev_account_count: usize,
    /// Seed used by hogd-dev and the supervisor to derive dev-account signers.
    pub dev_account_seed: String,
    /// Storage-token funding target per local-user account.
    pub dev_account_tokens: u64,
    /// RAM database capacity in bytes hog-devnode reserves at startup.
    ///
    /// When unset, hog-devnode uses its built-in default capacity.
    pub ramdb_bytes: Option<u64>,
    /// PID file used to detect and clean up orphaned hogd-dev children.
    pub pid_file: Option<PathBuf>,
}

impl Default for HogdDevConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            data_dir: None,
            log_level: None,
            listen_host: DEFAULT_HOGD_DEV_LISTEN_HOST.to_string(),
            shutdown_grace: DEFAULT_HOGD_DEV_SHUTDOWN_GRACE,
            startup_timeout: DEFAULT_HOGD_DEV_STARTUP_TIMEOUT,
            dev_account_count: DEFAULT_HOGD_DEV_ACCOUNT_COUNT,
            dev_account_seed: DEFAULT_HOGD_DEV_ACCOUNT_SEED.to_string(),
            dev_account_tokens: DEFAULT_HOGD_DEV_ACCOUNT_TOKENS,
            ramdb_bytes: None,
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

    /// Return a copy with a directory for persistent hog-devnode state.
    #[must_use]
    pub fn with_data_dir(mut self, data_dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(resolve_data_dir(data_dir.into()));
        self
    }

    /// Return a copy with an explicit hogd-dev log level.
    #[must_use]
    pub fn with_log_level(mut self, log_level: LogLevel) -> Self {
        self.log_level = Some(log_level);
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

    /// Return a copy with an explicit hog-devnode RAM database capacity in bytes.
    #[must_use]
    pub fn with_ramdb_bytes(mut self, ramdb_bytes: u64) -> Self {
        self.ramdb_bytes = Some(ramdb_bytes);
        self
    }

    /// Return a copy with a PID file for orphan detection.
    #[must_use]
    pub fn with_pid_file(mut self, pid_file: impl Into<PathBuf>) -> Self {
        self.pid_file = Some(pid_file.into());
        self
    }

    /// Lower the current hogd-dev shorthand settings into bootstrap intent.
    ///
    /// Supervisor launch code serializes this manifest and passes it to
    /// `hogd-dev --bootstrap-manifest`.
    pub fn to_bootstrap_manifest(
        &self,
        network_hash: NetworkHashHex,
    ) -> Result<BootstrapManifest, std::num::TryFromIntError> {
        Ok(BootstrapManifest::new(
            HOGD_DEV_BOOTSTRAP_MANIFEST_KIND,
            network_hash,
            DEFAULT_HOGD_DEV_BOOTSTRAP_PROFILE,
            BootstrapManifestDevWallet::profile(DEFAULT_HOGD_DEV_WALLET_PROFILE),
            BootstrapManifestFaucet::new(
                LOCAL_FAUCET_INDEX,
                LOCAL_FAUCET_STORAGE_TOKENS.to_string(),
            ),
            BootstrapManifestAccounts::new(
                u32::try_from(self.dev_account_count)?,
                FIRST_LOCAL_USER_INDEX,
                self.dev_account_seed.clone(),
                self.dev_account_tokens.to_string(),
            ),
            BootstrapManifestStorageCostModel::profile(DEFAULT_HOGD_DEV_STORAGE_COST_MODEL_PROFILE),
        ))
    }
}

fn resolve_data_dir(path: PathBuf) -> PathBuf {
    let expanded = match path.strip_prefix(Path::new("~")) {
        Ok(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(rest))
            .unwrap_or(path),
        Err(_) => path,
    };
    std::path::absolute(&expanded).unwrap_or(expanded)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use groundhog_json_rpc_types::NetworkHashHex;
    use serde_json::json;

    use super::{BlockDriverConfig, BlockDriverMode, HogdDevConfig, LocalAirdropConfig};

    #[test]
    fn data_dir_is_absolute_and_expands_home() {
        let relative = HogdDevConfig::default().with_data_dir("groundhog-test-data");
        let relative = relative
            .data_dir
            .expect("bug: data dir should be configured");
        assert!(relative.is_absolute());
        assert!(relative.ends_with("groundhog-test-data"));

        if let Some(home) = std::env::var_os("HOME") {
            let expanded = HogdDevConfig::default().with_data_dir("~/groundhog-test-data");
            assert_eq!(
                expanded.data_dir,
                Some(PathBuf::from(home).join("groundhog-test-data"))
            );
        }
    }

    #[test]
    fn block_driver_default_keeps_instant_mode_with_500ms_interval() {
        let config = BlockDriverConfig::default();

        assert_eq!(config.mode, BlockDriverMode::Instant);
        assert_eq!(config.interval, std::time::Duration::from_millis(500));
        assert_eq!(
            config.manual_request_timeout,
            std::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn manual_block_driver_keeps_default_interval_inert() {
        let config = BlockDriverConfig::manual();

        assert_eq!(config.mode, BlockDriverMode::Manual);
        assert_eq!(config.interval, std::time::Duration::from_millis(500));
        assert_eq!(
            config.manual_request_timeout,
            std::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn local_airdrop_default_is_valid() {
        assert!(LocalAirdropConfig::default().validate().is_ok());
    }

    #[test]
    fn local_airdrop_rejects_zero_resource_bounds() {
        let default = LocalAirdropConfig::default();
        let invalid = [
            LocalAirdropConfig {
                raw_amount: 0,
                ..default
            },
            LocalAirdropConfig {
                attempt_capacity: 0,
                ..default
            },
            LocalAirdropConfig {
                max_concurrency: 0,
                ..default
            },
            LocalAirdropConfig {
                request_timeout: std::time::Duration::ZERO,
                ..default
            },
        ];

        assert!(invalid.into_iter().all(|config| config.validate().is_err()));
    }

    #[test]
    fn hogd_dev_config_lowers_to_bootstrap_manifest() {
        let config = HogdDevConfig::default()
            .with_dev_account_count(3)
            .with_dev_account_seed("bench-seed")
            .with_dev_account_tokens(42);

        let manifest = config
            .to_bootstrap_manifest(NetworkHashHex::from([0x51; 32]))
            .expect("bug: account count fits u32");

        let value = serde_json::to_value(manifest).expect("bug: manifest serializes");

        assert_eq!(
            value,
            json!({
                "schemaVersion": 2,
                "kind": "groundhog-bootstrap",
                "networkHash": "5151515151515151515151515151515151515151515151515151515151515151",
                "profile": "local-dev",
                "devWallet": {
                    "profile": "hog-devnode-default",
                },
                "bootstrapFaucet": {
                    "index": 0,
                    "initialStorageTokens": "10000000000000",
                },
                "bootstrapAccounts": {
                    "count": 3,
                    "firstIndex": 1,
                    "seed": "bench-seed",
                    "initialStorageTokens": "42",
                },
                "storageCostModel": {
                    "profile": "hogd-default",
                },
            })
        );
    }

    #[test]
    fn hogd_dev_default_funds_each_user_with_one_hundred_hog() {
        assert_eq!(
            HogdDevConfig::default().dev_account_tokens,
            100 * 10_000_000
        );
    }
}
