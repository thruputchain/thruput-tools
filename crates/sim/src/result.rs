use std::collections::BTreeMap;

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

use crate::manifest::WorkloadKind;
use crate::target::{TargetOwnership, TargetProviderKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimResult {
    pub schema_version: u32,
    pub name: String,
    pub outcome: SimRunOutcome,
    pub target_provider: TargetProviderKind,
    pub target_ownership: TargetOwnership,
    #[serde(default)]
    pub target_identity: TargetIdentity,
    #[serde(default)]
    pub runtime_identity: RuntimeIdentity,
    pub nonce_namespace: u32,
    pub manifest_hashes: ManifestHashes,
    pub metric_availability: SimMetricAvailability,
    #[serde(default)]
    pub workload_counts: BTreeMap<WorkloadKind, WorkloadCounts>,
    #[serde(default)]
    pub workload_metrics: BTreeMap<WorkloadKind, WorkloadMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_progress: Option<ChainProgressSummary>,
    #[serde(default)]
    pub assertion_results: Vec<AssertionResult>,
    #[serde(default)]
    pub chaos_events: Vec<ChaosEventRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SimRunOutcome {
    Passed,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestHashes {
    pub simulation_manifest_sha256: String,
    pub runtime_manifest_sha256: String,
    pub bootstrap_manifest: MetricAvailability<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeIdentity {
    pub network_id: String,
    pub protocol: String,
    pub genesis_hash: String,
    pub producer: ProducerIdentity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerIdentity {
    pub binary: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum MetricAvailability<T> {
    Observed { value: T },
    Unsupported { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimMetricAvailability {
    pub storage_tokens_consumed: MetricAvailability<u64>,
    pub nnint_compute_used: MetricAvailability<u64>,
}

impl SimMetricAvailability {
    pub fn phase_one_defaults() -> Self {
        Self {
            storage_tokens_consumed: MetricAvailability::Unsupported {
                reason: "public receipt path does not surface storage-token deltas".into(),
            },
            nnint_compute_used: MetricAvailability::Unsupported {
                reason: "public receipt path does not surface NNInt compute usage".into(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadCounts {
    pub completed: u64,
    pub failed: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertionResult {
    pub name: String,
    pub status: AssertionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssertionStatus {
    Passed,
    Failed,
    Unsupported,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadMetrics {
    pub counts: WorkloadCounts,
    #[serde(default)]
    pub error_counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<LatencySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencySummary {
    pub sample_count: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    #[serde(rename = "p99_9Ms")]
    pub p99_9_ms: u64,
    pub max_ms: u64,
}

impl LatencySummary {
    pub fn from_histogram(histogram: &Histogram<u64>) -> Option<Self> {
        if histogram.is_empty() {
            return None;
        }

        Some(Self {
            sample_count: histogram.len(),
            p50_ms: histogram.value_at_quantile(0.50),
            p95_ms: histogram.value_at_quantile(0.95),
            p99_ms: histogram.value_at_quantile(0.99),
            p99_9_ms: histogram.value_at_quantile(0.999),
            max_ms: histogram.max(),
        })
    }

    pub fn from_sorted_samples(samples: &[u64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }

        Some(Self {
            sample_count: samples.len() as u64,
            p50_ms: percentile_per_mille(samples, 500),
            p95_ms: percentile_per_mille(samples, 950),
            p99_ms: percentile_per_mille(samples, 990),
            p99_9_ms: percentile_per_mille(samples, 999),
            max_ms: *samples
                .last()
                .expect("bug: samples was checked non-empty above"),
        })
    }
}

fn percentile_per_mille(sorted_samples: &[u64], per_mille: usize) -> u64 {
    let rank = sorted_samples
        .len()
        .saturating_mul(per_mille)
        .div_ceil(1_000);
    let index = rank.saturating_sub(1).min(sorted_samples.len() - 1);
    sorted_samples[index]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainProgressSummary {
    pub first_height: i64,
    pub last_height: i64,
    pub max_height: i64,
    pub total_observations: u64,
    pub observations: Vec<ChainTipObservation>,
}

impl ChainProgressSummary {
    pub fn from_observations(observations: Vec<ChainTipObservation>) -> Option<Self> {
        let first = observations.first()?;
        let last = observations
            .last()
            .expect("bug: first observation existed, so last must exist");
        let max_height = observations
            .iter()
            .map(|observation| observation.height)
            .max()
            .expect("bug: first observation existed, so max must exist");

        Some(Self {
            first_height: first.height,
            last_height: last.height,
            max_height,
            total_observations: u64::try_from(observations.len())
                .expect("bug: observation length should fit u64"),
            observations,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainTipObservation {
    pub simulation_time_ms: u64,
    pub height: i64,
    pub block_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChaosEventRecord {
    pub kind: String,
    pub state: ChaosEventState,
    pub simulation_time_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChaosEventState {
    Scheduled,
    Started,
    Completed,
    Failed,
    Recovered,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn metric_availability_is_explicit_not_zero_filled() {
        let availability = SimMetricAvailability::phase_one_defaults();
        let value = serde_json::to_value(availability).expect("availability serializes");

        assert_eq!(
            value["storageTokensConsumed"],
            json!({
                "status": "unsupported",
                "reason": "public receipt path does not surface storage-token deltas"
            })
        );
        assert_eq!(
            value["nnintComputeUsed"],
            json!({
                "status": "unsupported",
                "reason": "public receipt path does not surface NNInt compute usage"
            })
        );
    }

    #[test]
    fn chaos_event_states_match_design_shape() {
        let states = [
            ChaosEventState::Scheduled,
            ChaosEventState::Started,
            ChaosEventState::Completed,
            ChaosEventState::Failed,
            ChaosEventState::Recovered,
        ];

        let names: Vec<_> = states
            .into_iter()
            .map(|state| serde_json::to_value(state).expect("state serializes"))
            .collect();
        assert_eq!(
            names,
            vec![
                json!("scheduled"),
                json!("started"),
                json!("completed"),
                json!("failed"),
                json!("recovered"),
            ]
        );
    }

    #[test]
    fn latency_summary_uses_nearest_rank_percentiles() {
        let samples = [10, 20, 30, 40, 50];
        assert_eq!(
            LatencySummary::from_sorted_samples(&samples),
            Some(LatencySummary {
                sample_count: 5,
                p50_ms: 30,
                p95_ms: 50,
                p99_ms: 50,
                p99_9_ms: 50,
                max_ms: 50,
            })
        );
    }
}
