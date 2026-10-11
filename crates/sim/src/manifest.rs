use std::path::PathBuf;

use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};
use serde::{Deserialize, Serialize};

use crate::target::TargetProviderSpec;

pub const SIMULATION_SCHEMA_VERSION: u32 = 3;
pub const SIMULATION_KIND: &str = "groundhog-simulation";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationManifest {
    pub schema_version: u32,
    pub kind: String,
    pub name: String,
    pub seed: String,
    pub target: TargetProviderSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<BootstrapSpec>,
    /// Explicit native actors; omission retains manifest-backed bootstrap accounts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<NativeAccountsSpec>,
    #[serde(default)]
    pub load_shape: Vec<LoadPhase>,
    #[serde(default)]
    pub workloads: Vec<WorkloadSpec>,
    #[serde(default)]
    pub contract_artifacts: Vec<ContractArtifactSpec>,
    #[serde(default)]
    pub view_calls: Vec<ViewCallSpec>,
    #[serde(default)]
    pub contract_calls: Vec<ContractCallSpec>,
    #[serde(default)]
    pub storage_reads: Vec<StorageReadSpec>,
    #[serde(default)]
    pub event_queries: Vec<EventQuerySpec>,
    #[serde(default)]
    pub read_after_write: Vec<ReadAfterWriteSpec>,
    #[serde(default)]
    pub assertions: AssertionSpec,
    #[serde(default)]
    pub chaos: Vec<ChaosEventSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observability: Option<ObservabilitySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedRunSpec>,
}

/// Explicit fixture-owned native workload accounts; omission permits the hosted bootstrap adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAccountsSpec {
    /// Explicit account model discriminator, serialized as `native`.
    pub kind: NativeAccountKind,
    /// Deterministic test-key seed; these accounts must not hold valuable funds.
    pub seed: String,
    /// Whether setup may request faucet funds; defaults to balance verification only.
    #[serde(default)]
    pub funding: NativeAccountFunding,
    /// Raw amount for local initial allocation or faucet transfer. Faucet mode requires it.
    /// Choose an amount that covers the minimum balance plus account storage costs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding_amount_raw: Option<String>,
    /// Minimum raw HOG balance required per actor before load, as a decimal string.
    /// Defaults to one raw unit; this is a setup check, not a workload cost estimate.
    #[serde(default = "default_native_minimum_balance")]
    pub minimum_balance_raw: String,
    /// Raw HOG transfer to each predicted deployment address, as a decimal string.
    /// Required when deploying native artifacts; omission does not fund collateral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_collateral_raw: Option<String>,
}
/// Account model selected by the explicit `accounts` configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeAccountKind {
    /// Derive nonce-zero pubkey accounts and use the native `DefaultWallet` ABI.
    Native,
}
/// Funding permission applied during native account setup, before measured load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeAccountFunding {
    /// Verify existing balances and fail if any actor is below its configured minimum.
    #[default]
    PreFunded,
    /// Request one airdrop per underfunded actor, then verify its minimum balance.
    Faucet,
}
fn default_native_minimum_balance() -> String {
    "1".into()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapSpec {
    /// Exact atomic funding for each contract deployment by a bootstrap actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_collateral_raw: Option<String>,
    pub accounts: u32,
    pub account_seed: String,
    pub initial_storage_tokens: String,
    pub storage_cost_profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_provisioning: Option<ActorProvisioningSpec>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActorProvisioningSpec {
    #[serde(default, skip_serializing_if = "is_false")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub airdrop_hog: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadPhase {
    pub duration: DurationSpec,
    pub users: u32,
    pub rate_per_user_hz: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stagger_actors: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationSpec(pub String);

impl DurationSpec {
    pub fn parse_millis(&self) -> Result<u64, DurationParseError> {
        parse_duration_millis(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DurationParseError {
    #[error("duration must not be empty")]
    Empty,
    #[error("duration `{0}` has an unsupported suffix; use ms, s, m, or h")]
    UnsupportedSuffix(String),
    #[error("duration `{0}` is not a positive number")]
    InvalidNumber(String),
    #[error("duration `{0}` is shorter than one millisecond")]
    TooShort(String),
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn parse_duration_millis(value: &str) -> Result<u64, DurationParseError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(DurationParseError::Empty);
    }

    let (number, multiplier) = if let Some(number) = trimmed.strip_suffix("ms") {
        (number, 1.0)
    } else if let Some(number) = trimmed.strip_suffix('s') {
        (number, 1_000.0)
    } else if let Some(number) = trimmed.strip_suffix('m') {
        (number, 60_000.0)
    } else if let Some(number) = trimmed.strip_suffix('h') {
        (number, 3_600_000.0)
    } else {
        return Err(DurationParseError::UnsupportedSuffix(trimmed.to_string()));
    };

    let parsed: f64 = number
        .trim()
        .parse()
        .map_err(|_| DurationParseError::InvalidNumber(trimmed.to_string()))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(DurationParseError::InvalidNumber(trimmed.to_string()));
    }

    let millis = parsed * multiplier;
    if millis < 1.0 {
        return Err(DurationParseError::TooShort(trimmed.to_string()));
    }
    let rounded = millis.round();
    if !rounded.is_finite() || (millis - rounded).abs() > f64::EPSILON * rounded.max(1.0) {
        return Err(DurationParseError::InvalidNumber(trimmed.to_string()));
    }
    Ok(rounded as u64)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadSpec {
    pub name: WorkloadKind,
    pub weight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractArtifactSpec {
    pub name: String,
    pub artifact: ContractArtifactRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractArtifactRef {
    pub kind: ContractArtifactKind,
    pub path: PathBuf,
}

/// A contract entrypoint expressed as raw selector bytes or a stable method identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntrypointSpec {
    /// An eight-byte selector encoded as `0x`-prefixed hex.
    Raw(String),
    /// A selector derived through the canonical contract selector implementation.
    Symbolic(SymbolicEntrypointSpec),
}

impl EntrypointSpec {
    pub fn resolve(&self) -> Result<[u8; 8], EntrypointSpecError> {
        match self {
            Self::Raw(value) => {
                let stripped = value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
                    .ok_or(EntrypointSpecError::MissingHexPrefix)?;
                let bytes = hex::decode(stripped)?;
                let actual = bytes.len();
                bytes
                    .try_into()
                    .map_err(|_| EntrypointSpecError::InvalidLength { actual })
            }
            Self::Symbolic(entrypoint) => entrypoint.resolve(),
        }
    }
}

impl From<String> for EntrypointSpec {
    fn from(value: String) -> Self {
        Self::Raw(value)
    }
}

impl From<&str> for EntrypointSpec {
    fn from(value: &str) -> Self {
        Self::Raw(value.to_owned())
    }
}

/// A contract method identity used to derive its canonical entrypoint selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SymbolicEntrypointSpec {
    pub interface: String,
    pub version: String,
    pub method: String,
}

impl SymbolicEntrypointSpec {
    fn resolve(&self) -> Result<[u8; 8], EntrypointSpecError> {
        validate_symbolic_entrypoint_field("interface", &self.interface)?;
        validate_symbolic_entrypoint_field("version", &self.version)?;
        validate_symbolic_entrypoint_field("method", &self.method)?;
        if groundhog_contract_util::semver::try_major_version_from_string(&self.version).is_none() {
            return Err(EntrypointSpecError::InvalidVersion {
                version: self.version.clone(),
            });
        }

        let signature = NormalizedMethodSignature::new(&self.method);
        let selector = contract_entrypoint_id(Some(&self.interface), &self.version, &signature);
        Ok(selector
            .as_ref()
            .try_into()
            .expect("bug: contract entrypoint selectors are always eight bytes"))
    }
}

fn validate_symbolic_entrypoint_field(
    field: &'static str,
    value: &str,
) -> Result<(), EntrypointSpecError> {
    if value.is_empty() {
        return Err(EntrypointSpecError::EmptySymbolicField { field });
    }
    if value.trim() != value {
        return Err(EntrypointSpecError::PaddedSymbolicField { field });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum EntrypointSpecError {
    #[error("raw entrypoint selector must be 0x-prefixed")]
    MissingHexPrefix,
    #[error("raw entrypoint selector contains invalid hex: {0}")]
    InvalidHex(#[from] hex::FromHexError),
    #[error("raw entrypoint selector must be eight bytes, got {actual}")]
    InvalidLength { actual: usize },
    #[error("symbolic entrypoint {field} must not be empty")]
    EmptySymbolicField { field: &'static str },
    #[error("symbolic entrypoint {field} must not contain leading or trailing whitespace")]
    PaddedSymbolicField { field: &'static str },
    #[error("symbolic entrypoint version `{version}` is not valid SemVer")]
    InvalidVersion { version: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
pub enum ContractArtifactKind {
    Rv32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewCallSpec {
    pub name: String,
    pub contract: String,
    #[serde(
        default = "default_target_weight",
        skip_serializing_if = "is_default_target_weight"
    )]
    pub weight: u32,
    pub entrypoint: EntrypointSpec,
    pub calldata: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_return_data: Option<String>,
}

/// A scheduled state-changing contract call available to `contract-call` workloads.
///
/// The simulator accepts a raw selector or derives one from a symbolic method
/// identity. Calldata remains raw hex and is not ABI-encoded by this surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractCallSpec {
    /// Scenario-local call name used in result artifacts.
    pub name: String,
    /// Name of the contract artifact this call targets.
    pub contract: String,
    /// Relative selection weight among configured contract-call targets.
    #[serde(
        default = "default_target_weight",
        skip_serializing_if = "is_default_target_weight"
    )]
    pub weight: u32,
    /// Raw selector bytes or the interface, version, and method used to derive them.
    pub entrypoint: EntrypointSpec,
    /// Target contract calldata as `0x`-prefixed hex.
    pub calldata: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageReadSpec {
    pub name: String,
    pub contract: String,
    #[serde(
        default = "default_target_weight",
        skip_serializing_if = "is_default_target_weight"
    )]
    pub weight: u32,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_present: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventQuerySpec {
    pub name: String,
    pub contract: String,
    #[serde(
        default = "default_target_weight",
        skip_serializing_if = "is_default_target_weight"
    )]
    pub weight: u32,
    #[serde(default = "default_event_query_order")]
    pub order: EventQueryOrderSpec,
    #[serde(default = "default_event_query_anchor")]
    pub anchor: EventQueryAnchorSpec,
    #[serde(default = "default_event_query_cursor")]
    pub cursor: String,
    #[serde(default = "default_event_query_limit")]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_min_events: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventQueryOrderSpec {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EventQueryAnchorSpec {
    AtBeginning,
    AtEnd,
    AtHeight { height: i64 },
}

fn default_event_query_order() -> EventQueryOrderSpec {
    EventQueryOrderSpec::Descending
}

fn default_event_query_anchor() -> EventQueryAnchorSpec {
    EventQueryAnchorSpec::AtEnd
}

fn default_event_query_cursor() -> String {
    "0x".into()
}

fn default_event_query_limit() -> u32 {
    50
}

/// A read-after-write target: a state-changing write whose committed effect is
/// read back and asserted against an actor-tracked expectation.
///
/// v1 reads are view-only; see the actor-state read-after-write design for the
/// deferred storage read mode. Entrypoints may be raw selectors or symbolic
/// method identities. Calldata remains raw hex and is not ABI-encoded. The
/// workload supports one or more configured entries selected by relative weight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadAfterWriteSpec {
    /// Scenario-local name used in result artifacts.
    pub name: String,
    /// Name of the contract artifact this round-trip targets.
    pub contract: String,
    /// Relative selection weight among configured read-after-write targets.
    #[serde(
        default = "default_target_weight",
        skip_serializing_if = "is_default_target_weight"
    )]
    pub weight: u32,
    /// The state-changing write submitted through native-wallet `execute`.
    pub write: RawWriteSpec,
    /// The view read-back issued after the write commits.
    pub read: RawReadSpec,
}

/// The write half of a read-after-write round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWriteSpec {
    /// Raw selector bytes or the interface, version, and method used to derive them.
    pub entrypoint: EntrypointSpec,
    /// Target contract calldata as `0x`-prefixed hex; empty (`0x`) for no-arg writes.
    #[serde(default = "default_empty_calldata")]
    pub calldata: String,
}

/// The read half of a read-after-write round-trip. View-only in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReadSpec {
    /// Raw selector bytes or the interface, version, and method used to derive them.
    pub entrypoint: EntrypointSpec,
    /// View calldata as `0x`-prefixed hex; empty (`0x`) for no-arg reads.
    #[serde(default = "default_empty_calldata")]
    pub calldata: String,
}

fn default_empty_calldata() -> String {
    "0x".into()
}

fn default_target_weight() -> u32 {
    1
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_target_weight(weight: &u32) -> bool {
    *weight == default_target_weight()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkloadKind {
    ChainTip,
    WalletSubmitSmoke,
    ReceiptPoll,
    StatusPoll,
    EventsQuery,
    StorageRead,
    ViewCall,
    ContractCall,
    ReadAfterWrite,
}

impl WorkloadKind {
    pub fn requires_indexer(self) -> bool {
        matches!(
            self,
            Self::ReceiptPoll
                | Self::StatusPoll
                | Self::EventsQuery
                | Self::ContractCall
                | Self::ReadAfterWrite
        )
    }

    pub fn requires_db_gateway(self) -> bool {
        matches!(self, Self::StorageRead)
    }

    pub fn requires_view_gateway(self) -> bool {
        matches!(self, Self::ViewCall | Self::ReadAfterWrite)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChaosEventSpec {
    pub at: DurationSpec,
    pub kind: ChaosEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilitySpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_sample_interval: Option<DurationSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChaosEventKind {
    RestartTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssertionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_error_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_receipt_timeout_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_backpressure_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_simulator_overload_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rss_growth_bytes_per_minute: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chain_stall: Option<DurationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p99_submit_latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRunSpec {
    pub rpc_url: String,
    pub runtime_manifest: PathBuf,
    pub nonce_namespace: u32,
}
