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

#[derive(Debug)]
struct LocalFundingAttempt {
    request: groundhog_json_rpc_types::airdrop::FundAccountRequest,
    admitted: Option<groundhog_json_rpc_types::airdrop::FundAccountResponse>,
    evidence: Option<groundhog_json_rpc_types::airdrop::LocalFundingErrorData>,
}

impl LocalFundingAttempt {
    fn retain_error_evidence(&mut self, error: &jsonrpsee::core::client::Error) -> Result<()> {
        if let jsonrpsee::core::client::Error::Call(call) = error
            && call.code() == groundhog_json_rpc_types::error::codes::LOCAL_FUNDING_UNRESOLVED
            && let Some(data) = call.data()
        {
            let evidence: groundhog_json_rpc_types::airdrop::LocalFundingErrorData =
                serde_json::from_str(data.get()).map_err(TestkitError::signing)?;
            validate_funding_evidence(&evidence)?;
            if self
                .evidence
                .as_ref()
                .is_some_and(|original| original != &evidence)
            {
                return Err(TestkitError::signing(
                    "local funding replaced retained signed evidence",
                ));
            }
            self.evidence = Some(evidence);
        }
        Ok(())
    }
}

struct NetworkInner {
    rpc_url: Option<String>,
    client: Option<HttpClient>,
    faucet: tokio::sync::Mutex<Option<crate::manifest::TestAccount>>,
    local_faucet: Option<ContractAddress>,
    local_funding:
        tokio::sync::Mutex<std::collections::HashMap<ContractAddress, LocalFundingAttempt>>,
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
        Ok(Self {
            inner: Arc::new(NetworkInner {
                rpc_url: Some(rpc_url.to_owned()),
                client: Some(client),
                faucet: tokio::sync::Mutex::new(crate::manifest::derive_faucet(manifest)?),
                local_faucet: local_faucet(manifest)?,
                local_funding: tokio::sync::Mutex::new(std::collections::HashMap::new()),
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
                local_faucet: None,
                local_funding: tokio::sync::Mutex::new(std::collections::HashMap::new()),
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
        let receipt = self
            .require_client("getTransactionReceipt")?
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|e| TestkitError::rpc("getTransactionReceipt", e))?;
        if receipt.as_ref().is_some_and(|r| r.tx_hash == tx_hash) {
            self.inner.local_funding.lock().await.retain(|_, attempt| {
                !attempt
                    .admitted
                    .as_ref()
                    .is_some_and(|admitted| admitted.tx_hash == tx_hash)
                    && !attempt
                        .evidence
                        .as_ref()
                        .is_some_and(|evidence| evidence.tx_hash == tx_hash)
            });
        }
        Ok(receipt)
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
        if let Some(faucet) = self.inner.local_faucet {
            return self.submit_local_fund(recipient, amount, faucet).await;
        }
        let calldata = groundhog_signing::native_hog_transfer_calldata(&recipient, amount)
            .map_err(TestkitError::signing)?;
        let faucet = self.inner.faucet.lock().await;
        let faucet = faucet.as_ref().ok_or(TestkitError::MissingFaucet)?;
        // The bootstrap faucet is shared across scenarios and processes. Its
        // funding transactions must not replay a deterministic scenario nonce.
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).map_err(|error| {
            TestkitError::signing(format!("generate faucet transaction nonce: {error}"))
        })?;
        let (tx_hash, tx_hex) = crate::account::prepare_call(
            faucet,
            groundhog_signing::NATIVE_HOG_ADDRESS,
            groundhog_signing::NATIVE_HOG_TRANSFER_ENTRYPOINT,
            &calldata,
            u64::from_le_bytes(nonce),
        )?;
        crate::account::submit_signed(self, tx_hash, &tx_hex).await?;
        Ok(SubmittedTransaction::new(self.clone(), tx_hash, tx_hex))
    }

    // Pending intent is retained on this Network handle before sending. Callers
    // retry on the same handle after uncertainty. Admission and signed bytes are
    // retained until an authoritative receipt, including cancellation of fund().
    async fn submit_local_fund(
        &self,
        recipient: ContractAddress,
        amount: u128,
        faucet: ContractAddress,
    ) -> Result<SubmittedTransaction> {
        use groundhog_json_rpc_client::LocalFundingApiClient;
        use groundhog_json_rpc_types::airdrop::{ClientNonce, FundAccountRequest};
        if amount == 0 {
            return Err(TestkitError::signing("funding amount must be positive"));
        }
        let mut pending = self.inner.local_funding.lock().await;
        if let Some(request) = pending.get(&recipient) {
            if request.request.raw_amount != amount.to_string() {
                return Err(TestkitError::FundingRequestMismatch { recipient });
            }
        } else {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(TestkitError::signing)?;
            pending.insert(
                recipient,
                LocalFundingAttempt {
                    request: FundAccountRequest {
                        recipient: Hex0x(recipient),
                        raw_amount: amount.to_string(),
                        faucet: Hex0x(faucet),
                        client_nonce: ClientNonce::new(nonce.to_vec())
                            .map_err(TestkitError::signing)?,
                    },
                    admitted: None,
                    evidence: None,
                },
            );
        }
        let attempt = pending
            .get(&recipient)
            .expect("bug: retained funding intent");
        let request = attempt.request.clone();
        let response = if let Some(response) = &attempt.admitted {
            response.clone()
        } else {
            match self
                .require_client("fund")?
                .fund_account(request.clone())
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    let attempt = pending
                        .get_mut(&recipient)
                        .expect("bug: retained funding intent");
                    attempt.retain_error_evidence(&error)?;
                    return Err(match &attempt.evidence {
                        Some(evidence) => TestkitError::Submission {
                            tx_hash: evidence.tx_hash,
                            signed_transaction: groundhog_json_rpc_types::hex::prefixed_hex(
                                &evidence.signed_transaction.0,
                            ),
                            detail: error.to_string(),
                        },
                        None => TestkitError::rpc("fundAccount", error),
                    });
                }
            }
        };
        let signed: groundhog_xdr::transaction::SignedTransaction =
            xdrs::txs::decode(&response.signed_transaction.0).map_err(TestkitError::signing)?;
        let hash =
            groundhog_signing::transaction_hash(&signed.tx).map_err(TestkitError::signing)?;
        if response.recipient != request.recipient
            || response.raw_amount != request.raw_amount
            || response.tx_hash.0.0 != hash
        {
            return Err(TestkitError::signing(
                "local funding response does not match retained request and signed hash",
            ));
        }
        let evidence = groundhog_json_rpc_types::airdrop::LocalFundingErrorData::new(
            response.tx_hash,
            response.signed_transaction.clone(),
        );
        if pending
            .get(&recipient)
            .and_then(|attempt| attempt.evidence.as_ref())
            .is_some_and(|original| original != &evidence)
        {
            return Err(TestkitError::signing(
                "local funding replaced retained signed evidence",
            ));
        }
        pending
            .get_mut(&recipient)
            .expect("bug: retained funding intent")
            .admitted = Some(response.clone());
        Ok(SubmittedTransaction::new(
            self.clone(),
            response.tx_hash,
            groundhog_json_rpc_types::hex::prefixed_hex(&response.signed_transaction.0),
        ))
    }

    fn require_client(&self, operation: &'static str) -> Result<&HttpClient> {
        self.inner
            .client
            .as_ref()
            .ok_or(TestkitError::Offline { operation })
    }
}

fn validate_funding_evidence(
    evidence: &groundhog_json_rpc_types::airdrop::LocalFundingErrorData,
) -> Result<()> {
    let signed: groundhog_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&evidence.signed_transaction.0).map_err(TestkitError::signing)?;
    let hash = groundhog_signing::transaction_hash(&signed.tx).map_err(TestkitError::signing)?;
    if evidence.tx_hash.0.0 != hash {
        return Err(TestkitError::signing(
            "local funding evidence hash does not match signed bytes",
        ));
    }
    Ok(())
}

fn local_faucet(manifest: &RuntimeManifest) -> Result<Option<ContractAddress>> {
    use groundhog_json_rpc_types::runtime_manifest::{
        BOOTSTRAP_ACCOUNT_ROLE_FAUCET, HOGD_DEV_RUNTIME_MANIFEST_KIND,
    };
    if manifest.kind != HOGD_DEV_RUNTIME_MANIFEST_KIND {
        return Ok(None);
    }
    let mut faucets = manifest
        .bootstrap_accounts
        .iter()
        .filter(|a| a.role == BOOTSTRAP_ACCOUNT_ROLE_FAUCET);
    let faucet = faucets.next().ok_or(TestkitError::MissingFaucet)?;
    if faucets.next().is_some()
        || groundhog_signing::PubkeyAccount::from_public_key_bytes(*faucet.public_key.0, 0)
            .address()
            != faucet.wallet_address.0
    {
        return Err(TestkitError::Manifest {
            field: "bootstrapAccounts",
            detail: "local faucet must uniquely match its public nonce-zero account".into(),
        });
    }
    Ok(Some(faucet.wallet_address.0))
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
                cause: groundhog_json_rpc_types::transaction::TransactionFailureCause::DbConflict {
                    conflict: groundhog_json_rpc_types::transaction::ConflictDiagnostic {
                        address: groundhog_json_rpc_types::hex::Hex0x([0; 32].into()),
                        key: groundhog_json_rpc_types::hex::HexBytes(Vec::new()),
                        code: groundhog_json_rpc_types::transaction::ConflictCode::Other,
                    }
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
            cause,
        } => format!("proposal attempt failed at height {attempt_height:?}: {cause}"),
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
    /// Initial raw HOG allocation per scenario-owned nonce-zero fixture account.
    pub initial_account_tokens: u64,
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
            initial_account_tokens: groundhog_node_supervisor::config::DEFAULT_LOCAL_ACCOUNT_TOKENS,
            startup_timeout: Duration::from_secs(10),
            block_driver: BlockDriverConfig::default(),
        }
    }
}
