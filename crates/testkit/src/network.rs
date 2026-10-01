//! The network a scenario runs against: how to reach it ([`NetworkConfig`])
//! and the live handle agents use ([`Network`]).

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use groundhog_node_supervisor::{BlockDriverConfig, LogLevel, ManualBlockProducer, ProducedBlock};

use groundhog_json_rpc_client::{
    GroundhogApiClient, HttpClient, HttpClientBuilder, ReceiptPollConfig, ReceiptPollError,
    ReceiptSource, poll_receipt,
};
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
const PENDING_POLL_INTERVAL: Duration = Duration::from_millis(25);
const PENDING_POLL_ATTEMPTS: usize = 400; // ~10 seconds at the interval above.

/// A cheap, cloneable handle onto the target network.
///
/// Chain verbs return [`TestkitError::Offline`] when the scenario has no live
/// target. [`Network::client`] exposes the underlying typed JSON-RPC client as
/// an escape hatch for methods this handle does not wrap.
#[derive(Clone)]
pub struct Network {
    inner: Arc<NetworkInner>,
}

/// A submitted transaction whose pending status and receipt can be awaited separately.
#[derive(Debug, Clone)]
#[must_use = "submitted transactions should be awaited or intentionally retained"]
pub struct SubmittedTransaction {
    network: Network,
    tx_hash: TxHash,
    tx_hex: String,
}

impl SubmittedTransaction {
    pub(crate) fn new(network: Network, tx_hash: TxHash, tx_hex: String) -> Self {
        Self {
            network,
            tx_hash,
            tx_hex,
        }
    }

    /// Signed bytes retained for reconciliation of the original attempt.
    pub fn signed_transaction(&self) -> &str {
        &self.tx_hex
    }

    /// Canonical transaction hash computed before submission.
    pub fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    /// Wait until the scheduler reports this transaction as pending.
    pub async fn await_pending(&self) -> Result<()> {
        self.network.await_pending(self.tx_hash).await
    }

    /// Wait until the indexer exposes this transaction's receipt.
    pub async fn await_receipt(&self) -> Result<TransactionReceipt> {
        self.network.await_receipt(self.tx_hash).await
    }
}

struct NetworkInner {
    rpc_url: Option<String>,
    client: Option<HttpClient>,
    faucet: tokio::sync::Mutex<Option<crate::manifest::TestAccount>>,
    do_action_entrypoint: Option<ContractEntrypointID>,
    manual_block_producer: Option<ManualBlockProducer>,
}

impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Network")
            .field("rpc_url", &self.inner.rpc_url)
            .field("online", &self.inner.client.is_some())
            .field(
                "manual_block_production",
                &self.inner.manual_block_producer.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl Network {
    /// Connect to a live target and retain its explicit bootstrap faucet adapter.
    pub(crate) fn connect(
        rpc_url: &str,
        manifest: &RuntimeManifest,
        manual_block_producer: Option<ManualBlockProducer>,
    ) -> Result<Self> {
        let client = HttpClientBuilder::default()
            .build(rpc_url)
            .map_err(|e| TestkitError::rpc("connect", e))?;
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
                faucet: tokio::sync::Mutex::new(crate::manifest::derive_faucet(manifest)?),
                do_action_entrypoint: Some(do_action_entrypoint),
                manual_block_producer,
            }),
        })
    }

    /// A handle with no live target; every chain verb reports [`TestkitError::Offline`].
    pub(crate) fn offline() -> Self {
        Self {
            inner: Arc::new(NetworkInner {
                rpc_url: None,
                client: None,
                faucet: tokio::sync::Mutex::new(None),
                do_action_entrypoint: None,
                manual_block_producer: None,
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
        match poll_receipt(
            &NetworkReceipts(self),
            tx_hash,
            ReceiptPollConfig::attempts(RECEIPT_POLL_ATTEMPTS, RECEIPT_POLL_INTERVAL),
        )
        .await
        {
            Ok(receipt) if receipt.tx_hash == tx_hash => Ok(receipt),
            Ok(_) => Err(TestkitError::ReceiptHashMismatch { expected: tx_hash }),
            Err(ReceiptPollError::Lookup { source, .. }) => Err(source),
            Err(_) => Err(TestkitError::ReceiptTimeout {
                tx_hash: format!("{tx_hash:?}"),
                status: self.describe_status(tx_hash).await,
            }),
        }
    }

    /// Fetch a transaction's status.
    pub async fn transaction_status(&self, tx_hash: TxHash) -> Result<TransactionStatus> {
        self.require_client("getTransactionStatus")?
            .get_transaction_status(tx_hash)
            .await
            .map_err(|e| TestkitError::rpc("getTransactionStatus", e))
    }

    /// Wait until the scheduler reports a submitted transaction as pending.
    ///
    /// A successful submit RPC only confirms `submit_unvetted` ingest. Manual
    /// block tests should await this barrier before requesting nomination.
    /// If the transaction commits before `Pending` is observed, this returns
    /// [`TestkitError::TransactionNotPending`] because the requested nomination
    /// boundary has already passed.
    pub async fn await_pending(&self, tx_hash: TxHash) -> Result<()> {
        self.await_pending_inner(tx_hash).await
    }

    async fn await_pending_inner(&self, tx_hash: TxHash) -> Result<()> {
        let mut last_status = TransactionStatus::Unknown;
        for _ in 0..PENDING_POLL_ATTEMPTS {
            let status = self.transaction_status(tx_hash).await?;
            match classify_pending_status(&status) {
                PendingStatusClassification::Ready => return Ok(()),
                PendingStatusClassification::Retry => {
                    last_status = status;
                    tokio::time::sleep(PENDING_POLL_INTERVAL).await;
                }
                PendingStatusClassification::Terminal => {
                    return Err(TestkitError::TransactionNotPending {
                        tx_hash: format!("{tx_hash:?}"),
                        status,
                    });
                }
            }
        }
        Err(TestkitError::PendingTimeout {
            tx_hash: format!("{tx_hash:?}"),
            last_status,
        })
    }

    /// Produce one block through a harness-owned local node in manual mode.
    pub async fn produce_block(&self) -> Result<ProducedBlock> {
        self.inner
            .manual_block_producer
            .as_ref()
            .ok_or(TestkitError::ManualBlockProductionUnavailable)?
            .produce_block()
            .await
            .map_err(|source| TestkitError::BlockProduction { source })
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
            gas_limit: None,
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

    pub(crate) async fn submit_fund(
        &self,
        recipient: ContractAddress,
        amount: u128,
    ) -> Result<SubmittedTransaction> {
        self.require_client("fund")?;
        let calldata = groundhog_signing::native_hog_transfer_calldata(&recipient, amount)
            .map_err(TestkitError::signing)?;
        let faucet = self.inner.faucet.lock().await;
        let faucet = faucet.as_ref().ok_or(TestkitError::MissingFaucet)?;
        // The bootstrap faucet is shared across scenarios and processes. Its
        // funding transactions must not replay a deterministic scenario nonce.
        let mut nonce = [0; 8];
        getrandom::getrandom(&mut nonce).map_err(|error| {
            TestkitError::signing(format!("generate faucet transaction nonce: {error}"))
        })?;
        let (tx_hash, tx_hex) = crate::account::prepare_call(
            faucet,
            self,
            groundhog_signing::NATIVE_HOG_ADDRESS,
            groundhog_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
            &calldata,
            u64::from_le_bytes(nonce),
        )?;
        crate::account::submit_signed(self, tx_hash, &tx_hex).await?;
        Ok(SubmittedTransaction::new(self.clone(), tx_hash, tx_hex))
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

/// Drives the shared receipt poll over a [`Network`].
///
/// Status is deliberately left out: `await_receipt` summarizes it once, when
/// the budget runs out, instead of spending a status request per attempt.
struct NetworkReceipts<'a>(&'a Network);

impl ReceiptSource for NetworkReceipts<'_> {
    type Error = TestkitError;

    async fn transaction_receipt(&self, tx_hash: TxHash) -> Result<Option<TransactionReceipt>> {
        self.0.receipt(tx_hash).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingStatusClassification {
    Ready,
    Retry,
    Terminal,
}

fn classify_pending_status(status: &TransactionStatus) -> PendingStatusClassification {
    match status {
        TransactionStatus::Pending => PendingStatusClassification::Ready,
        TransactionStatus::Unknown => PendingStatusClassification::Retry,
        _ => PendingStatusClassification::Terminal,
    }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::transaction::TransactionStatus;
    use groundhog_node_supervisor::BlockDriverMode;

    use super::{
        LocalNetworkConfig, Network, PendingStatusClassification, classify_pending_status,
    };
    use crate::TestkitError;

    #[test]
    fn local_network_keeps_instant_block_production_by_default() {
        assert_eq!(
            LocalNetworkConfig::default().block_driver.mode,
            BlockDriverMode::Instant
        );
    }

    #[test]
    fn pending_status_classification_never_resubmits() {
        assert_eq!(
            classify_pending_status(&TransactionStatus::Pending),
            PendingStatusClassification::Ready
        );
        assert_eq!(
            classify_pending_status(&TransactionStatus::Unknown),
            PendingStatusClassification::Retry
        );
        assert_eq!(
            classify_pending_status(&TransactionStatus::Invalid {
                reason: "rejected".to_owned(),
            }),
            PendingStatusClassification::Terminal
        );
        assert_eq!(
            classify_pending_status(&TransactionStatus::Committed { height: 2.into() }),
            PendingStatusClassification::Terminal
        );
        assert_eq!(
            classify_pending_status(&TransactionStatus::RecentFailure {
                attempt_height: 1.into(),
                conflict: groundhog_json_rpc_types::transaction::ConflictDiagnostic {
                    address: groundhog_json_rpc_types::hex::Hex0x([0; 32].into()),
                    key: groundhog_json_rpc_types::hex::HexBytes(Vec::new()),
                    code: groundhog_json_rpc_types::transaction::ConflictCode::Other,
                },
            }),
            PendingStatusClassification::Terminal
        );
    }

    #[tokio::test]
    async fn offline_network_rejects_manual_block_production() {
        let error = Network::offline()
            .produce_block()
            .await
            .expect_err("offline network must not expose manual production");
        assert!(matches!(
            error,
            TestkitError::ManualBlockProductionUnavailable
        ));
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
        /// Number of empty native accounts to make available to agents.
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
    /// Artifact directories searched before the selected devnode's defaults.
    pub contract_artifact_directories: Vec<PathBuf>,
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
    /// Number of funded local-user accounts to bootstrap, excluding the faucet.
    pub dev_account_count: usize,
    /// Seed for deterministic dev-account derivation.
    pub dev_account_seed: String,
    /// Initial storage-token balance per local-user account.
    pub dev_account_tokens: u64,
    /// Maximum time to wait for the node to report readiness.
    pub startup_timeout: Duration,
    /// Local block production policy. Defaults to submit-triggered production.
    pub block_driver: BlockDriverConfig,
}

impl Default for LocalNetworkConfig {
    fn default() -> Self {
        Self {
            hog_devnode_binary: None,
            contract_artifact_directories: Vec::new(),
            rpc_port: 0,
            hogd_protocol: None,
            log_level: None,
            dev_account_count: groundhog_signing::DEFAULT_DEV_ACCOUNT_COUNT,
            dev_account_seed: groundhog_signing::DEFAULT_DEV_ACCOUNT_SEED.to_owned(),
            dev_account_tokens: groundhog_node_supervisor::config::DEFAULT_HOGD_DEV_ACCOUNT_TOKENS,
            startup_timeout: Duration::from_secs(10),
            block_driver: BlockDriverConfig::default(),
        }
    }
}
