use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::target::TargetProviderSpec;

pub const SIMULATION_SCHEMA_VERSION: u32 = 1;
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
    pub bootstrap: Option<BootstrapSpec>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSpec {
    pub accounts: u32,
    pub account_seed: String,
    pub initial_storage_tokens: String,
    pub dev_wallet_profile: String,
    pub storage_cost_profile: String,
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
    pub wasm: PathBuf,
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
    pub entrypoint: String,
    pub calldata: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_return_data: Option<String>,
}

/// A scheduled state-changing contract call available to `contract-call` workloads.
///
/// The simulator expects raw entrypoint and calldata hex. It does not inspect
/// the contract ABI or encode method arguments for this manifest surface. This
/// surface supports one or more configured targets selected by relative weight.
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
    /// Eight-byte target contract entrypoint selector as `0x`-prefixed hex.
    pub entrypoint: String,
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
/// deferred storage read mode. The simulator uses raw 8-byte entrypoint and raw
/// calldata hex; it does not encode method arguments. The workload supports one
/// or more configured entries selected by relative weight.
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
    /// The state-changing write submitted through dev-wallet `doAction`.
    pub write: RawWriteSpec,
    /// The view read-back issued after the write commits.
    pub read: RawReadSpec,
}

/// The write half of a read-after-write round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWriteSpec {
    /// Eight-byte target contract entrypoint selector as `0x`-prefixed hex.
    pub entrypoint: String,
    /// Target contract calldata as `0x`-prefixed hex; empty (`0x`) for no-arg writes.
    #[serde(default = "default_empty_calldata")]
    pub calldata: String,
}

/// The read half of a read-after-write round-trip. View-only in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReadSpec {
    /// Eight-byte view entrypoint selector as `0x`-prefixed hex.
    pub entrypoint: String,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::target::TargetProviderKind;

    #[test]
    fn parses_manifest_with_baseline_and_closed_workload_enum() {
        let manifest: SimulationManifest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "local-wallet-soak",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "10s", "users": 2, "ratePerUserHz": 1.5}],
            "workloads": [
                {"name": "chain-tip", "weight": 10},
                {"name": "wallet-submit-smoke", "weight": 20}
            ],
            "baseline": "baselines/local-wallet-soak.json"
        }))
        .expect("manifest should parse");

        assert_eq!(manifest.target.provider, TargetProviderKind::Existing);
        assert_eq!(manifest.workloads[1].name, WorkloadKind::WalletSubmitSmoke);
        assert_eq!(
            manifest.load_shape[0].duration.parse_millis().unwrap(),
            10_000
        );
        assert_eq!(
            manifest.baseline.unwrap(),
            PathBuf::from("baselines/local-wallet-soak.json")
        );
    }

    #[test]
    fn parses_optional_observability_without_serializing_empty_object() {
        let manifest: SimulationManifest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "observed",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "10s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}],
            "observability": {"resourceSampleInterval": "1s"}
        }))
        .expect("manifest should parse");

        assert_eq!(
            manifest
                .observability
                .as_ref()
                .and_then(|observability| observability.resource_sample_interval.as_ref())
                .expect("interval should parse")
                .parse_millis()
                .unwrap(),
            1_000
        );

        let without_observability: SimulationManifest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "unobserved",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "10s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "chain-tip", "weight": 1}]
        }))
        .expect("manifest should parse");
        let value =
            serde_json::to_value(&without_observability).expect("manifest should serialize");
        assert!(value.get("observability").is_none());
    }

    #[test]
    fn rejects_unknown_workload_name() {
        let err = serde_json::from_value::<SimulationManifest>(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "bad",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "10s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "surprise", "weight": 1}]
        }))
        .expect_err("closed enum should reject unknown workloads");

        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn event_query_defaults_match_first_page_at_end() {
        let manifest: SimulationManifest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "events",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "events-query", "weight": 1}],
            "contractArtifacts": [{"name": "fibonacci", "wasm": "fibonacci.wasm"}],
            "eventQueries": [{"name": "fib-events", "contract": "fibonacci"}]
        }))
        .expect("minimal event query should parse");

        let query = &manifest.event_queries[0];
        assert_eq!(query.weight, 1);
        assert_eq!(query.order, EventQueryOrderSpec::Descending);
        assert_eq!(query.anchor, EventQueryAnchorSpec::AtEnd);
        assert_eq!(query.cursor, "0x");
        assert_eq!(query.limit, 50);
        assert_eq!(query.expect_min_events, None);
    }

    #[test]
    fn default_target_weights_are_not_serialized() {
        let manifest: SimulationManifest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "kind": "groundhog-simulation",
            "name": "weights",
            "seed": "seed",
            "target": {"provider": "existing"},
            "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
            "workloads": [{"name": "contract-call", "weight": 1}],
            "contractArtifacts": [{"name": "events", "wasm": "events.wasm"}],
            "contractCalls": [{
                "name": "events-store-value",
                "contract": "events",
                "entrypoint": "0x0c012d3b21f5676d",
                "calldata": "0x2a64"
            }]
        }))
        .expect("manifest should parse");

        let json = serde_json::to_value(&manifest).expect("manifest should serialize");

        assert_eq!(manifest.contract_calls[0].weight, 1);
        assert!(json["contractCalls"][0].get("weight").is_none());
    }

    #[test]
    fn parses_duration_suffixes_to_milliseconds() {
        assert_eq!(DurationSpec("1500ms".into()).parse_millis().unwrap(), 1_500);
        assert_eq!(DurationSpec("1.5s".into()).parse_millis().unwrap(), 1_500);
        assert_eq!(DurationSpec("2s".into()).parse_millis().unwrap(), 2_000);
        assert_eq!(DurationSpec("3m".into()).parse_millis().unwrap(), 180_000);
        assert_eq!(DurationSpec("1h".into()).parse_millis().unwrap(), 3_600_000);
    }
}
