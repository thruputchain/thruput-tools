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
    pub assertions: AssertionSpec,
    #[serde(default)]
    pub chaos: Vec<ChaosEventSpec>,
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
    pub entrypoint: String,
    pub calldata: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_return_data: Option<String>,
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
}

impl WorkloadKind {
    pub fn requires_indexer(self) -> bool {
        matches!(
            self,
            Self::ReceiptPoll | Self::StatusPoll | Self::EventsQuery
        )
    }

    pub fn requires_db_gateway(self) -> bool {
        matches!(self, Self::StorageRead)
    }

    pub fn requires_view_gateway(self) -> bool {
        matches!(self, Self::ViewCall)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChaosEventSpec {
    pub at: DurationSpec,
    pub kind: ChaosEventKind,
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
    fn parses_duration_suffixes_to_milliseconds() {
        assert_eq!(DurationSpec("1500ms".into()).parse_millis().unwrap(), 1_500);
        assert_eq!(DurationSpec("1.5s".into()).parse_millis().unwrap(), 1_500);
        assert_eq!(DurationSpec("2s".into()).parse_millis().unwrap(), 2_000);
        assert_eq!(DurationSpec("3m".into()).parse_millis().unwrap(), 180_000);
        assert_eq!(DurationSpec("1h".into()).parse_millis().unwrap(), 3_600_000);
    }
}
