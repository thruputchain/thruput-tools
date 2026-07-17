//! The network a scenario runs against: how to reach it ([`NetworkConfig`])
//! and the live handle agents use ([`Network`]).

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use groundhog_node_supervisor::LogLevel;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClient, HttpClientBuilder};
use groundhog_json_rpc_types::block::ChainTip;
use groundhog_json_rpc_types::events::{EventPage, EventQuery};
use groundhog_json_rpc_types::hex::{Hex0x, HexBytes};
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;
use groundhog_json_rpc_types::state::StorageItemRead;
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};
use groundhog_json_rpc_types::view::{ViewRequest, ViewResponse};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;

use crate::error::{Result, TestkitError};

const RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const RECEIPT_POLL_ATTEMPTS: usize = 40; // ~10 seconds at the interval above.

/// A cheap, cloneable handle onto the target network.
///
/// Chain verbs return [`TestkitError::Offline`] when the scenario has no live
/// target. [`Network::client`] exposes the underlying typed JSON-RPC client as
/// an escape hatch for methods this handle does not wrap.
#[derive(Clone)]
pub struct Network {
    inner: Arc<NetworkInner>,
}

struct NetworkInner {
    rpc_url: Option<String>,
    client: Option<HttpClient>,
    deploy_entrypoint: Option<ContractEntrypointID>,
    do_action_entrypoint: Option<ContractEntrypointID>,
}

impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Network")
            .field("rpc_url", &self.inner.rpc_url)
            .field("online", &self.inner.client.is_some())
            .finish_non_exhaustive()
    }
}

impl Network {
    /// Connect to a live target and resolve the dev-wallet entrypoints.
    pub(crate) fn connect(rpc_url: &str, manifest: &RuntimeManifest) -> Result<Self> {
        let client = HttpClientBuilder::default()
            .build(rpc_url)
            .map_err(|e| TestkitError::rpc("connect", e))?;
        let deploy_entrypoint = manifest
            .bootstrap
            .dev_wallet
            .deploy_entrypoint_id()
            .map_err(|source| TestkitError::Manifest {
                field: "bootstrap.devWallet.deployEntrypoint",
                detail: source.to_string(),
            })?;
        let do_action_entrypoint = manifest
            .bootstrap
            .dev_wallet
            .do_action_entrypoint_id()
            .map_err(|source| TestkitError::Manifest {
                field: "bootstrap.devWallet.doActionEntrypoint",
                detail: source.to_string(),
            })?;
        Ok(Self {
            inner: Arc::new(NetworkInner {
                rpc_url: Some(rpc_url.to_owned()),
                client: Some(client),
                deploy_entrypoint: Some(deploy_entrypoint),
                do_action_entrypoint: Some(do_action_entrypoint),
            }),
        })
    }

    /// A handle with no live target; every chain verb reports [`TestkitError::Offline`].
    pub(crate) fn offline() -> Self {
        Self {
            inner: Arc::new(NetworkInner {
                rpc_url: None,
                client: None,
                deploy_entrypoint: None,
                do_action_entrypoint: None,
            }),
        }
    }

    /// The public JSON-RPC URL, when the target has one.
    pub fn rpc_url(&self) -> Option<&str> {
        self.inner.rpc_url.as_deref()
    }

    /// The underlying typed JSON-RPC client, for methods this handle does not wrap.
    pub fn client(&self) -> Option<&HttpClient> {
        self.inner.client.as_ref()
    }

    /// Current chain tip.
    pub async fn chain_tip(&self) -> Result<ChainTip> {
        self.require_client("chainTip")?
            .chain_tip()
            .await
            .map_err(|e| TestkitError::rpc("chainTip", e))
    }

    /// Submit a pre-signed `0x`-hex transaction and return its hash.
    pub async fn submit_raw(&self, tx_hex: String) -> Result<TxHash> {
        self.require_client("sendRawTransaction")?
            .send_raw_transaction(tx_hex)
            .await
            .map_err(|e| TestkitError::rpc("sendRawTransaction", e))
    }

    /// Fetch a transaction receipt if it is already available.
    pub async fn receipt(&self, tx_hash: TxHash) -> Result<Option<TransactionReceipt>> {
        self.require_client("getTransactionReceipt")?
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|e| TestkitError::rpc("getTransactionReceipt", e))
    }

    /// Poll for a receipt until it appears or the polling budget elapses.
    ///
    /// On timeout the error carries a `getTransactionStatus` summary, which
    /// usually explains the missing receipt (a scheduler rejection, or a
    /// two-phase-commit conflict such as a balance that could not cover its own
    /// storage cost) rather than leaving the caller with a bare timeout.
    pub async fn await_receipt(&self, tx_hash: TxHash) -> Result<TransactionReceipt> {
        for _ in 0..RECEIPT_POLL_ATTEMPTS {
            if let Some(receipt) = self.receipt(tx_hash).await? {
                return Ok(receipt);
            }
            tokio::time::sleep(RECEIPT_POLL_INTERVAL).await;
        }
        Err(TestkitError::ReceiptTimeout {
            tx_hash: format!("{tx_hash:?}"),
            status: self.describe_status(tx_hash).await,
        })
    }

    /// Fetch a transaction's status.
    pub async fn transaction_status(&self, tx_hash: TxHash) -> Result<TransactionStatus> {
        self.require_client("getTransactionStatus")?
            .get_transaction_status(tx_hash)
            .await
            .map_err(|e| TestkitError::rpc("getTransactionStatus", e))
    }

    /// Best-effort human summary of a transaction's status, for diagnostics.
    ///
    /// Never fails: a status query that errors is folded into the returned
    /// string so it can be embedded directly in a timeout error.
    async fn describe_status(&self, tx_hash: TxHash) -> String {
        match self.transaction_status(tx_hash).await {
            Ok(status) => describe_status(&status),
            Err(e) => format!("status unavailable: {e}"),
        }
    }

    /// Read one latest-committed storage item at `address`/`key`.
    pub async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: &[u8],
    ) -> Result<StorageItemRead> {
        self.require_client("getStorageAt")?
            .get_storage_at(Hex0x(address), HexBytes(key.to_vec()))
            .await
            .map_err(|e| TestkitError::rpc("getStorageAt", e))
    }

    /// Simulate a contract call through `groundhog_view`.
    ///
    /// `caller` sets the simulated `msg.sender`; pass an agent's own address to
    /// read state keyed by that caller.
    pub async fn view_call(
        &self,
        contract: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
        caller: Option<ContractAddress>,
    ) -> Result<ViewResponse> {
        let request = ViewRequest {
            contract: Hex0x(contract),
            entrypoint: HexBytes(entrypoint.into_inner().to_vec()),
            calldata: HexBytes(calldata.to_vec()),
            caller: caller.map(Hex0x),
        };
        self.require_client("view")?
            .view(request)
            .await
            .map_err(|e| TestkitError::rpc("view", e))
    }

    /// Query contract events.
    pub async fn get_events(&self, query: EventQuery) -> Result<EventPage> {
        self.require_client("getEvents")?
            .get_events(query)
            .await
            .map_err(|e| TestkitError::rpc("getEvents", e))
    }

    pub(crate) fn deploy_entrypoint(&self) -> Result<ContractEntrypointID> {
        self.inner.deploy_entrypoint.ok_or(TestkitError::Offline {
            operation: "deploy",
        })
    }

    pub(crate) fn do_action_entrypoint(&self) -> Result<ContractEntrypointID> {
        self.inner
            .do_action_entrypoint
            .ok_or(TestkitError::Offline { operation: "call" })
    }

    fn require_client(&self, operation: &'static str) -> Result<&HttpClient> {
        self.inner
            .client
            .as_ref()
            .ok_or(TestkitError::Offline { operation })
    }
}

/// Render a transaction status as a short diagnostic phrase.
fn describe_status(status: &TransactionStatus) -> String {
    match status {
        TransactionStatus::Pending => {
            "still pending; never committed within the polling budget".to_owned()
        }
        TransactionStatus::Committed { height } => {
            format!("committed at height {height:?}, but the receipt was not yet indexed")
        }
        TransactionStatus::Invalid { reason } => format!("rejected by the scheduler: {reason}"),
        TransactionStatus::RecentFailure {
            attempt_height,
            conflict,
        } => format!(
            "dropped during two-phase commit at height {attempt_height:?}: {:?} conflict on \
             contract {:?}",
            conflict.code, conflict.address
        ),
        TransactionStatus::Unknown => "unknown; not found in the mempool or the indexer".to_owned(),
        _ => "unrecognized status from the node".to_owned(),
    }
}

/// Which network a scenario runs against, and how to reach it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum NetworkConfig {
    /// An already-running network reached over its public JSON-RPC URL.
    Existing {
        /// Public JSON-RPC URL.
        rpc_url: String,
        /// Path to the network's runtime manifest.
        runtime_manifest: PathBuf,
    },
    /// A local node the harness launches and owns.
    Local(LocalNetworkConfig),
    /// No live network: agents run with locally-derived accounts and no chain.
    ///
    /// Useful for exercising agent coordination, timing, and messaging logic
    /// without a node. Chain verbs return [`crate::TestkitError::Offline`].
    Offline {
        /// Number of dev accounts to make available to agents.
        accounts: usize,
    },
}

impl NetworkConfig {
    /// Configure an already-running (`existing`) network.
    pub fn existing(rpc_url: impl Into<String>, runtime_manifest: impl Into<PathBuf>) -> Self {
        Self::Existing {
            rpc_url: rpc_url.into(),
            runtime_manifest: runtime_manifest.into(),
        }
    }

    /// Configure a harness-managed (`local`) network.
    pub fn local(config: LocalNetworkConfig) -> Self {
        Self::Local(config)
    }

    /// Configure an offline network with `accounts` locally-derived accounts.
    pub fn offline(accounts: usize) -> Self {
        Self::Offline { accounts }
    }
}

/// Settings for a harness-managed local node.
///
/// The harness binds the public JSON-RPC facade on an ephemeral port and keeps
/// no PID file, so many local networks can run in one process without
/// colliding.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LocalNetworkConfig {
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

impl Default for LocalNetworkConfig {
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
