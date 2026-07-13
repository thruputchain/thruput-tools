//! Explicit target configuration.

use std::path::PathBuf;
use std::time::Duration;

use groundhog_node_supervisor::LogLevel;

/// Which network a scenario runs against, and how to reach it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TargetConfig {
    /// An already-running target reached over its public JSON-RPC URL.
    Existing {
        /// Public JSON-RPC URL.
        rpc_url: String,
        /// Path to the target's runtime manifest.
        runtime_manifest: PathBuf,
    },
    /// A local node the harness launches and owns.
    Local(LocalTargetConfig),
    /// No target: agents run with locally-derived accounts and no chain.
    ///
    /// Useful for exercising agent coordination, timing, and messaging logic
    /// without a node. Chain verbs return [`crate::TestkitError::Offline`].
    Offline {
        /// Number of dev accounts to make available to agents.
        accounts: usize,
    },
}

impl TargetConfig {
    /// Configure an already-running (`existing`) target.
    pub fn existing(rpc_url: impl Into<String>, runtime_manifest: impl Into<PathBuf>) -> Self {
        Self::Existing {
            rpc_url: rpc_url.into(),
            runtime_manifest: runtime_manifest.into(),
        }
    }

    /// Configure a harness-managed (`local`) target.
    pub fn local(config: LocalTargetConfig) -> Self {
        Self::Local(config)
    }

    /// Configure an offline target with `accounts` locally-derived accounts.
    pub fn offline(accounts: usize) -> Self {
        Self::Offline { accounts }
    }
}

/// Settings for a harness-managed local node.
///
/// The harness binds the public JSON-RPC facade on an ephemeral port and keeps
/// no PID file, so many local targets can run in one process without colliding.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LocalTargetConfig {
    /// Explicit `hog-devnode` binary path. Falls back to discovery when unset
    /// (the `GROUNDHOG_DEVNODE_BINARY` env var, then a sibling of the current
    /// executable, then `PATH`).
    pub hog_devnode_binary: Option<PathBuf>,
    /// Requested public JSON-RPC port; `0` asks the OS to choose.
    pub rpc_port: u16,
    /// Optional protocol token override for `hog-devnode` intro handshakes.
    ///
    /// Use when a local `hog-devnode` build is ahead of the protocol compiled
    /// into the supervisor; it only overrides the intro token.
    pub hogd_protocol: Option<String>,
    /// Devnode log verbosity. When set, `hog-devnode` is started at this level
    /// and testkit installs a stderr `tracing` subscriber (via `try_init`,
    /// honoring `RUST_LOG`) so the forwarded logs print. Leave `None` for a
    /// quiet node; call [`crate::init_tracing`] directly for finer control.
    pub log_level: Option<LogLevel>,
    /// Number of funded dev accounts to bootstrap.
    pub dev_account_count: usize,
    /// Seed for deterministic dev-account derivation.
    pub dev_account_seed: String,
    /// Initial storage-token balance per dev account.
    pub dev_account_tokens: u64,
    /// Maximum time to wait for the node to report readiness.
    pub startup_timeout: Duration,
}

impl Default for LocalTargetConfig {
    fn default() -> Self {
        Self {
            hog_devnode_binary: None,
            rpc_port: 0,
            hogd_protocol: None,
            log_level: None,
            dev_account_count: 10,
            dev_account_seed: "groundhog-dev-accounts".to_owned(),
            dev_account_tokens: 1_000_000_000,
            startup_timeout: Duration::from_secs(10),
        }
    }
}
