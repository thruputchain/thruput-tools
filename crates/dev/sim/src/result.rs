use std::collections::BTreeMap;

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

use crate::compare::ResultDiff;
use crate::manifest::{ContractArtifactKind, WorkloadKind};
use crate::target::{TargetOwnership, TargetProviderKind};

pub const SIM_RESULT_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimResult {
    pub schema_version: u32,
    pub run_id: String,
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
    #[serde(default)]
    pub backpressure_rate: f64,
    #[serde(default)]
    pub simulator_overload_rate: f64,
    #[serde(default)]
    pub simulator_overload_count: u64,
    #[serde(default, skip_serializing_if = "RunDiagnostics::is_empty")]
    pub diagnostics: RunDiagnostics,
    #[serde(default = "default_resource_timeline")]
    pub resource_timeline: MetricAvailability<ResourceTimeline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_progress: Option<ChainProgressSummary>,
    #[serde(default)]
    pub assertion_results: Vec<AssertionResult>,
    #[serde(default)]
    pub chaos_events: Vec<ChaosEventRecord>,
    #[serde(default)]
    pub contracts: Vec<ContractDeploymentRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_diff: Option<BaselineDiff>,
}

/// Diff of the current run against a baseline result declared by the scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineDiff {
    /// Baseline reference exactly as authored in the scenario manifest, not the
    /// path resolved relative to the scenario file.
    pub path: String,
    /// Per-metric deltas computed as current run minus baseline.
    pub diff: ResultDiff,
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
    /// SHA-256 of the exact emitted `simulation.json` bytes.
    pub simulation_manifest_sha256: String,
    /// SHA-256 of effective configuration after excluding run-resolved identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation_config_sha256: Option<String>,
    pub runtime_manifest_sha256: String,
    /// SHA-256 of runtime configuration after excluding backend dial addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_config_sha256: Option<String>,
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
    pub network_hash: String,
    pub protocol: String,
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

fn default_resource_timeline() -> MetricAvailability<ResourceTimeline> {
    MetricAvailability::Unavailable {
        reason: "target resource sampling was not recorded".into(),
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceSampleScope {
    /// Aggregate over the target provider's owned process group.
    ProcessGroup,
    /// Aggregate over only the target provider's owned root process.
    NodeProcess,
}

/// Resource samples and restart-aware aggregates for the target process scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTimeline {
    /// Scope used by the recorded samples.
    pub scope: ResourceSampleScope,
    /// Configured sampling cadence in milliseconds.
    pub sample_interval_ms: u64,
    /// Number of observed samples processed, including samples omitted from the retained window.
    pub sample_count: u64,
    /// First observed sample.
    pub first: ResourceSamplePoint,
    /// Last observed sample.
    pub last: ResourceSamplePoint,
    /// Maximum RSS observed across all samples with RSS available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rss_bytes: Option<u64>,
    /// Algebraic maximum RSS growth across restart segments, in bytes per wall-clock minute.
    ///
    /// Segments with fewer than two RSS observations do not contribute. If every contributing
    /// segment shrinks, this is the least-negative segment growth rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_segment_rss_growth_bytes_per_minute: Option<i64>,
    /// RSS growth for the final restart segment, in bytes per wall-clock minute.
    ///
    /// This is absent when the final segment has fewer than two RSS observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_segment_rss_growth_bytes_per_minute: Option<i64>,
    /// Maximum thread count observed across samples where thread count was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_threads: Option<u64>,
    /// Maximum open-fd count observed across samples where fd count was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_open_fds: Option<u64>,
    /// Sum of monotonic user+system CPU deltas within each restart segment.
    ///
    /// CPU counters are never differenced across process identity changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cpu_ms: Option<u64>,
    /// Restart-aware per-process-identity segments used for growth and CPU aggregates.
    #[serde(default)]
    pub segments: Vec<ResourceSegment>,
    /// Retained raw sample window, capped for long soaks; aggregate fields include all samples.
    #[serde(default)]
    pub samples: Vec<ResourceSamplePoint>,
}

/// One target resource observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSamplePoint {
    /// Scheduled simulation time for the sample tick.
    pub simulation_time_ms: u64,
    /// Wall-clock observation timestamp in Unix milliseconds, encoded as a JSON-safe string.
    pub observed_at_unix_ms: String,
    /// Scope used for this sample.
    pub scope: ResourceSampleScope,
    /// Root process ID owned by the target provider at sample time.
    pub root_pid: u32,
    /// Process group ID when the provider can identify one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// Number of processes included in the point-in-time aggregate.
    pub process_count: u32,
    /// Resident set size in bytes when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// User CPU time in milliseconds when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_user_ms: Option<u64>,
    /// System CPU time in milliseconds when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_system_ms: Option<u64>,
    /// Thread count when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<u64>,
    /// Open file descriptor count when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_fds: Option<u64>,
}

/// Resource aggregate for one continuous target process identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSegment {
    /// Scope used by samples in the segment.
    pub scope: ResourceSampleScope,
    /// Root process ID for this segment.
    pub root_pid: u32,
    /// Process group ID for this segment when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// Number of observed samples in this segment.
    pub sample_count: u64,
    /// Wall-clock timestamp of the first segment sample in Unix milliseconds.
    pub started_at_unix_ms: String,
    /// Wall-clock timestamp of the last segment sample in Unix milliseconds.
    pub ended_at_unix_ms: String,
    /// First RSS observation in the segment when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_rss_bytes: Option<u64>,
    /// Last RSS observation in the segment when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rss_bytes: Option<u64>,
    /// Maximum RSS observed in the segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rss_bytes: Option<u64>,
    /// RSS growth for this segment in bytes per wall-clock minute.
    ///
    /// Absent when the segment has fewer than two RSS observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_growth_bytes_per_minute: Option<i64>,
    /// Sum of monotonic user+system CPU deltas within this segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cpu_ms: Option<u64>,
}

/// Compact failure diagnostics promoted from the JSONL event stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDiagnostics {
    /// Cross-workload error-class counts with first/last occurrence and sample text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_classes: Vec<ErrorClassSummary>,
    /// Resource-sample failures that did not contribute to the observed resource timeline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_sample_failures: Vec<ResourceSampleFailureSummary>,
}

impl RunDiagnostics {
    pub fn is_empty(&self) -> bool {
        self.error_classes.is_empty() && self.resource_sample_failures.is_empty()
    }
}

/// Summary of one workload error class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorClassSummary {
    pub error_class: String,
    pub count: u64,
    pub first_scheduled_time_ms: u64,
    pub last_scheduled_time_ms: u64,
    #[serde(default)]
    pub workloads: BTreeMap<WorkloadKind, u64>,
    #[serde(default)]
    pub rpc_methods: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_error: Option<String>,
}

/// Summary of one resource-sample failure reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSampleFailureSummary {
    pub reason: String,
    pub count: u64,
    pub first_scheduled_time_ms: u64,
    pub last_scheduled_time_ms: u64,
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
    /// Latency summary for successful workload completions only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_latency_ms: Option<LatencySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_to_receipt_latency_ms: Option<LatencySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_poll: Option<ReceiptPollSummary>,
}

/// Receipt-poll outcome summary for submitted transactions.
///
/// `present` counts transactions whose receipt was observed. `absent` counts
/// poll attempts that returned `null` while the transaction stayed pending.
/// `pending_at_shutdown` counts submitted transactions that still had no
/// observed receipt when the run ended.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptPollSummary {
    pub present: u64,
    pub absent: u64,
    pub pending_at_shutdown: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencySummary {
    pub sample_count: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub p999_ms: u64,
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
            p999_ms: histogram.value_at_quantile(0.999),
            max_ms: histogram.max(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainProgressSummary {
    pub first_height: i64,
    pub last_height: i64,
    pub max_height: i64,
    pub total_observations: u64,
    /// Maximum observed chain stall in simulation time, computed from all
    /// chain-tip observations before the public observation list is capped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_stall_ms: Option<u64>,
    pub observations: Vec<ChainTipObservation>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chaos_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractDeploymentRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collateral: Option<NativeDeploymentCollateral>,
    pub name: String,
    pub status: ContractDeploymentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_kind: Option<ContractArtifactKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_address_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_source: Option<DeployAddressSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeDeploymentCollateral {
    pub recipient: String,
    pub raw_amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContractDeploymentStatus {
    Started,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployAddressSource {
    Receipt,
    Derived,
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
    fn chaos_event_record_serializes_aggregate_shape() {
        let record = ChaosEventRecord {
            kind: "restart-target".into(),
            state: ChaosEventState::Recovered,
            simulation_time_ms: 1_000,
            chaos_index: Some(0),
            latency_ms: Some(12),
            detail: Some("restarted local target oldPid=1 newPid=2".into()),
        };

        let value = serde_json::to_value(record).expect("record serializes");

        assert_eq!(
            value,
            json!({
                "kind": "restart-target",
                "state": "recovered",
                "simulationTimeMs": 1000,
                "chaosIndex": 0,
                "latencyMs": 12,
                "detail": "restarted local target oldPid=1 newPid=2"
            })
        );
    }

    #[test]
    fn result_defaults_missing_contracts_for_old_artifacts() {
        let value = json!({
            "schemaVersion": 1,
            "runId": "run-1",
            "name": "old-result",
            "outcome": "passed",
            "targetProvider": "existing",
            "targetOwnership": {
                "provider": "existing",
                "startedBySim": false,
                "cleanupAllowed": false
            },
            "nonceNamespace": 1,
            "manifestHashes": {
                "simulationManifestSha256": "sim",
                "runtimeManifestSha256": "runtime",
                "bootstrapManifest": {
                    "status": "unsupported",
                    "reason": "old"
                }
            },
            "metricAvailability": {
                "storageTokensConsumed": {
                    "status": "unsupported",
                    "reason": "old"
                },
                "nnintComputeUsed": {
                    "status": "unsupported",
                    "reason": "old"
                }
            }
        });

        let result: SimResult = serde_json::from_value(value).expect("old result should parse");

        assert!(result.contracts.is_empty());
        assert!(result.manifest_hashes.simulation_config_sha256.is_none());
        assert!(result.manifest_hashes.runtime_config_sha256.is_none());
    }
}
