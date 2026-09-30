use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::SimError;
use crate::manifest::WorkloadKind;
use crate::result::{
    AssertionResult, AssertionStatus, ChainProgressSummary, LatencySummary, MetricAvailability,
    ResourceTimeline, SIM_RESULT_SCHEMA_VERSION, SimResult, SimRunOutcome, WorkloadCounts,
    WorkloadMetrics,
};

/// Schema version for the JSON shape emitted by `hogsim compare --json`.
///
/// Bump when an existing field changes meaning, is removed, or is renamed.
/// Purely additive fields (new optional keys) do not require a bump.
pub const COMPARE_DIFF_SCHEMA_VERSION: u32 = 2;

/// Load a `result.json` from either a run directory or direct file path.
pub async fn load_result(path: &Path) -> Result<SimResult, SimError> {
    let path = if path.is_dir() {
        path.join("result.json")
    } else {
        path.to_path_buf()
    };
    let bytes = tokio::fs::read(&path).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if let Some(actual) = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .and_then(|actual| u32::try_from(actual).ok())
        && actual != SIM_RESULT_SCHEMA_VERSION
    {
        return Err(SimError::UnsupportedResultSchema {
            actual,
            path: path.display().to_string(),
        });
    }
    Ok(serde_json::from_value(value)?)
}

/// Identity and availability diff for two run results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(rename_all = "camelCase")]
pub struct ResultDiff {
    pub schema_version: u32,
    pub same_config: bool,
    pub simulation_manifest_match: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation_config_match: Option<bool>,
    pub runtime_manifest_match: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_config_match: Option<bool>,
    pub runtime_identity_match: bool,
    pub producer_identity_match: bool,
    pub outcome_a: SimRunOutcome,
    pub outcome_b: SimRunOutcome,
    pub storage_tokens_a: String,
    pub storage_tokens_b: String,
    pub nnint_compute_a: String,
    pub nnint_compute_b: String,
    pub workload_count_deltas: BTreeMap<WorkloadKind, WorkloadCountDelta>,
    pub workload_error_deltas: BTreeMap<WorkloadKind, BTreeMap<String, i64>>,
    pub latency_deltas_ms: BTreeMap<WorkloadKind, WorkloadLatencyDelta>,
    pub assertion_transitions: Vec<AssertionTransition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_progress_delta: Option<ChainProgressDelta>,
    pub metric_numeric_deltas: Vec<MetricNumericDelta>,
    #[serde(default)]
    pub metric_float_deltas: Vec<MetricFloatDelta>,
    pub numeric_skips: Vec<NumericSkip>,
}

impl ResultDiff {
    /// Compare two run results.
    pub fn from_results(a: &SimResult, b: &SimResult) -> Self {
        let simulation_manifest_match = a.manifest_hashes.simulation_manifest_sha256
            == b.manifest_hashes.simulation_manifest_sha256;
        let simulation_config_match = match (
            &a.manifest_hashes.simulation_config_sha256,
            &b.manifest_hashes.simulation_config_sha256,
        ) {
            (Some(a), Some(b)) => a == b,
            _ => simulation_manifest_match,
        };
        let runtime_manifest_match =
            a.manifest_hashes.runtime_manifest_sha256 == b.manifest_hashes.runtime_manifest_sha256;
        let runtime_config_match = match (
            &a.manifest_hashes.runtime_config_sha256,
            &b.manifest_hashes.runtime_config_sha256,
        ) {
            (Some(a), Some(b)) => a == b,
            _ => runtime_manifest_match,
        };
        let runtime_identity_match = a.runtime_identity.network_hash
            == b.runtime_identity.network_hash
            && a.runtime_identity.protocol == b.runtime_identity.protocol;
        let producer_identity_match = a.runtime_identity.producer == b.runtime_identity.producer;

        let metric_diffs = metric_diffs(a, b);
        Self {
            schema_version: COMPARE_DIFF_SCHEMA_VERSION,
            same_config: simulation_config_match
                && runtime_config_match
                && runtime_identity_match
                && producer_identity_match,
            simulation_manifest_match,
            simulation_config_match: Some(simulation_config_match),
            runtime_manifest_match,
            runtime_config_match: Some(runtime_config_match),
            runtime_identity_match,
            producer_identity_match,
            outcome_a: a.outcome,
            outcome_b: b.outcome,
            storage_tokens_a: availability_status(&a.metric_availability.storage_tokens_consumed),
            storage_tokens_b: availability_status(&b.metric_availability.storage_tokens_consumed),
            nnint_compute_a: availability_status(&a.metric_availability.nnint_compute_used),
            nnint_compute_b: availability_status(&b.metric_availability.nnint_compute_used),
            workload_count_deltas: workload_count_deltas(a, b),
            workload_error_deltas: workload_error_deltas(a, b),
            latency_deltas_ms: latency_deltas(a, b),
            assertion_transitions: assertion_transitions(a, b),
            chain_progress_delta: chain_progress_delta(
                a.chain_progress.as_ref(),
                b.chain_progress.as_ref(),
            ),
            metric_numeric_deltas: metric_diffs.deltas,
            metric_float_deltas: metric_diffs.float_deltas,
            numeric_skips: metric_diffs.skips,
        }
    }

    /// Render a human-readable summary for terminal output.
    pub fn render_text(&self) -> String {
        format!(
            concat!(
                "same config: {}\n",
                "  simulation manifest match: {}\n",
                "  simulation config match: {}\n",
                "  runtime manifest match: {}\n",
                "  runtime config match: {}\n",
                "  runtime identity match: {}\n",
                "  producer identity match: {}\n",
                "outcome: {} -> {}\n",
                "storage-token availability: {} -> {}\n",
                "NNInt compute availability: {} -> {}\n",
                "workload count deltas: {}\n",
                "latency delta workloads: {}\n",
                "assertion transitions: {}\n",
                "metric numeric deltas: {}\n",
                "metric float deltas: {}\n",
                "numeric skips: {}"
            ),
            self.same_config,
            self.simulation_manifest_match,
            self.simulation_config_match
                .unwrap_or(self.simulation_manifest_match),
            self.runtime_manifest_match,
            self.runtime_config_match
                .unwrap_or(self.runtime_manifest_match),
            self.runtime_identity_match,
            self.producer_identity_match,
            outcome_label(self.outcome_a),
            outcome_label(self.outcome_b),
            self.storage_tokens_a,
            self.storage_tokens_b,
            self.nnint_compute_a,
            self.nnint_compute_b,
            self.workload_count_deltas.len(),
            self.latency_deltas_ms.len(),
            self.assertion_transitions.len(),
            self.metric_numeric_deltas.len(),
            self.metric_float_deltas.len(),
            self.numeric_skips.len()
        )
    }
}

fn outcome_label(outcome: SimRunOutcome) -> &'static str {
    match outcome {
        SimRunOutcome::Passed => "passed",
        SimRunOutcome::Failed => "failed",
        SimRunOutcome::Aborted => "aborted",
    }
}

/// Per-workload completed/failed/skipped deltas, computed as `b - a`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadCountDelta {
    pub completed: i64,
    pub failed: i64,
    pub skipped: i64,
}

/// Per-workload latency percentile deltas, computed as `b - a`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadLatencyDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<LatencySummaryDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_latency_ms: Option<LatencySummaryDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit_to_receipt_latency_ms: Option<LatencySummaryDelta>,
}

/// Latency-summary deltas, computed as `b - a`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencySummaryDelta {
    pub sample_count: i64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    pub p999_ms: i64,
    pub max_ms: i64,
}

/// Assertion status or measurement transition between two runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertionTransition {
    pub name: String,
    pub status_a: Option<AssertionStatus>,
    pub status_b: Option<AssertionStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_b: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_a: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_b: Option<String>,
}

/// Chain-progress deltas, computed as `b - a`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainProgressDelta {
    pub first_height: i64,
    pub last_height: i64,
    pub max_height: i64,
    pub total_observations: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_stall_ms: Option<i64>,
}

/// Numeric metric delta for metrics observed on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricNumericDelta {
    pub metric: String,
    pub value: i64,
}

/// Floating-point metric delta for metrics observed on both sides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricFloatDelta {
    pub metric: String,
    pub value: f64,
}

/// Numeric metric comparison skipped because at least one side has no value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NumericSkip {
    pub metric: String,
    pub status_a: String,
    pub status_b: String,
}

struct MetricDiffs {
    deltas: Vec<MetricNumericDelta>,
    float_deltas: Vec<MetricFloatDelta>,
    skips: Vec<NumericSkip>,
}

fn workload_count_deltas(
    a: &SimResult,
    b: &SimResult,
) -> BTreeMap<WorkloadKind, WorkloadCountDelta> {
    workload_keys(&a.workload_counts, &b.workload_counts)
        .into_iter()
        .filter_map(|workload| {
            let a_counts = a
                .workload_counts
                .get(&workload)
                .cloned()
                .unwrap_or_default();
            let b_counts = b
                .workload_counts
                .get(&workload)
                .cloned()
                .unwrap_or_default();
            let delta = count_delta(&a_counts, &b_counts);
            if delta.completed == 0 && delta.failed == 0 && delta.skipped == 0 {
                None
            } else {
                Some((workload, delta))
            }
        })
        .collect()
}

fn workload_error_deltas(
    a: &SimResult,
    b: &SimResult,
) -> BTreeMap<WorkloadKind, BTreeMap<String, i64>> {
    workload_metric_keys(a, b)
        .into_iter()
        .filter_map(|workload| {
            let a_errors = a
                .workload_metrics
                .get(&workload)
                .map(|metrics| &metrics.error_counts);
            let b_errors = b
                .workload_metrics
                .get(&workload)
                .map(|metrics| &metrics.error_counts);
            let deltas = error_deltas(a_errors, b_errors);
            if deltas.is_empty() {
                None
            } else {
                Some((workload, deltas))
            }
        })
        .collect()
}

fn latency_deltas(a: &SimResult, b: &SimResult) -> BTreeMap<WorkloadKind, WorkloadLatencyDelta> {
    workload_metric_keys(a, b)
        .into_iter()
        .filter_map(|workload| {
            let a_metrics = a.workload_metrics.get(&workload);
            let b_metrics = b.workload_metrics.get(&workload);
            let delta = workload_latency_delta(a_metrics, b_metrics);
            if delta.latency_ms.is_none()
                && delta.success_latency_ms.is_none()
                && delta.submit_to_receipt_latency_ms.is_none()
            {
                None
            } else {
                Some((workload, delta))
            }
        })
        .collect()
}

fn assertion_transitions(a: &SimResult, b: &SimResult) -> Vec<AssertionTransition> {
    let a_assertions: BTreeMap<_, _> = a
        .assertion_results
        .iter()
        .map(|assertion| (assertion.name.as_str(), assertion))
        .collect();
    let b_assertions: BTreeMap<_, _> = b
        .assertion_results
        .iter()
        .map(|assertion| (assertion.name.as_str(), assertion))
        .collect();
    a_assertions
        .keys()
        .chain(b_assertions.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            assertion_transition(name, a_assertions.get(name), b_assertions.get(name))
        })
        .collect()
}

fn assertion_transition(
    name: &str,
    a: Option<&&AssertionResult>,
    b: Option<&&AssertionResult>,
) -> Option<AssertionTransition> {
    if a.map(|assertion| assertion.status) == b.map(|assertion| assertion.status)
        && a.and_then(|assertion| assertion.observed.as_ref())
            == b.and_then(|assertion| assertion.observed.as_ref())
        && a.and_then(|assertion| assertion.threshold.as_ref())
            == b.and_then(|assertion| assertion.threshold.as_ref())
    {
        return None;
    }

    Some(AssertionTransition {
        name: name.into(),
        status_a: a.map(|assertion| assertion.status),
        status_b: b.map(|assertion| assertion.status),
        observed_a: a.and_then(|assertion| assertion.observed.clone()),
        observed_b: b.and_then(|assertion| assertion.observed.clone()),
        threshold_a: a.and_then(|assertion| assertion.threshold.clone()),
        threshold_b: b.and_then(|assertion| assertion.threshold.clone()),
    })
}

fn chain_progress_delta(
    a: Option<&ChainProgressSummary>,
    b: Option<&ChainProgressSummary>,
) -> Option<ChainProgressDelta> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let delta = ChainProgressDelta {
                first_height: delta_i64(a.first_height, b.first_height),
                last_height: delta_i64(a.last_height, b.last_height),
                max_height: delta_i64(a.max_height, b.max_height),
                total_observations: delta_u64(a.total_observations, b.total_observations),
                max_stall_ms: match (a.max_stall_ms, b.max_stall_ms) {
                    (Some(a), Some(b)) => Some(delta_u64(a, b)),
                    _ => None,
                },
            };
            (!delta.is_zero()).then_some(delta)
        }
        _ => None,
    }
}

fn metric_diffs(a: &SimResult, b: &SimResult) -> MetricDiffs {
    let mut deltas = Vec::new();
    let mut float_deltas = Vec::new();
    let mut skips = Vec::new();
    collect_metric_diff(
        "storageTokensConsumed",
        &a.metric_availability.storage_tokens_consumed,
        &b.metric_availability.storage_tokens_consumed,
        &mut deltas,
        &mut skips,
    );
    collect_metric_diff(
        "nnintComputeUsed",
        &a.metric_availability.nnint_compute_used,
        &b.metric_availability.nnint_compute_used,
        &mut deltas,
        &mut skips,
    );
    collect_rate_diff(
        "backpressureRate",
        a.backpressure_rate,
        b.backpressure_rate,
        &mut float_deltas,
    );
    collect_rate_diff(
        "simulatorOverloadRate",
        a.simulator_overload_rate,
        b.simulator_overload_rate,
        &mut float_deltas,
    );
    collect_resource_metric_diffs(
        &a.resource_timeline,
        &b.resource_timeline,
        &mut deltas,
        &mut skips,
    );
    MetricDiffs {
        deltas,
        float_deltas,
        skips,
    }
}

fn collect_metric_diff(
    metric: &'static str,
    a: &MetricAvailability<u64>,
    b: &MetricAvailability<u64>,
    deltas: &mut Vec<MetricNumericDelta>,
    skips: &mut Vec<NumericSkip>,
) {
    match (a, b) {
        (MetricAvailability::Observed { value: a }, MetricAvailability::Observed { value: b }) => {
            deltas.push(MetricNumericDelta {
                metric: metric.into(),
                value: delta_u64(*a, *b),
            });
        }
        _ => skips.push(NumericSkip {
            metric: metric.into(),
            status_a: availability_status(a),
            status_b: availability_status(b),
        }),
    }
}

fn collect_rate_diff(metric: &'static str, a: f64, b: f64, deltas: &mut Vec<MetricFloatDelta>) {
    let value = b - a;
    if value != 0.0 {
        deltas.push(MetricFloatDelta {
            metric: metric.into(),
            value,
        });
    }
}

fn collect_resource_metric_diffs(
    a: &MetricAvailability<ResourceTimeline>,
    b: &MetricAvailability<ResourceTimeline>,
    deltas: &mut Vec<MetricNumericDelta>,
    skips: &mut Vec<NumericSkip>,
) {
    match (a, b) {
        (MetricAvailability::Observed { value: a }, MetricAvailability::Observed { value: b }) => {
            collect_optional_i64_delta(
                "resourceTimeline.maxSegmentRssGrowthBytesPerMinute",
                a.max_segment_rss_growth_bytes_per_minute,
                b.max_segment_rss_growth_bytes_per_minute,
                "insufficient-samples",
                deltas,
                skips,
            );
            collect_optional_u64_delta(
                "resourceTimeline.maxRssBytes",
                a.max_rss_bytes,
                b.max_rss_bytes,
                "no-data",
                deltas,
                skips,
            );
            collect_optional_u64_delta(
                "resourceTimeline.totalCpuMs",
                a.total_cpu_ms,
                b.total_cpu_ms,
                "no-data",
                deltas,
                skips,
            );
        }
        _ if resource_timeline_observed(a) || resource_timeline_observed(b) => {
            for metric in [
                "resourceTimeline.maxSegmentRssGrowthBytesPerMinute",
                "resourceTimeline.maxRssBytes",
                "resourceTimeline.totalCpuMs",
            ] {
                skips.push(NumericSkip {
                    metric: metric.into(),
                    status_a: availability_status(a),
                    status_b: availability_status(b),
                });
            }
        }
        _ => {}
    }
}

fn collect_optional_i64_delta(
    metric: &'static str,
    a: Option<i64>,
    b: Option<i64>,
    missing_status: &'static str,
    deltas: &mut Vec<MetricNumericDelta>,
    skips: &mut Vec<NumericSkip>,
) {
    match (a, b) {
        (Some(a), Some(b)) => deltas.push(MetricNumericDelta {
            metric: metric.into(),
            value: b - a,
        }),
        _ => skips.push(NumericSkip {
            metric: metric.into(),
            status_a: option_status(a.is_some(), missing_status),
            status_b: option_status(b.is_some(), missing_status),
        }),
    }
}

fn collect_optional_u64_delta(
    metric: &'static str,
    a: Option<u64>,
    b: Option<u64>,
    missing_status: &'static str,
    deltas: &mut Vec<MetricNumericDelta>,
    skips: &mut Vec<NumericSkip>,
) {
    match (a, b) {
        (Some(a), Some(b)) => deltas.push(MetricNumericDelta {
            metric: metric.into(),
            value: delta_u64(a, b),
        }),
        _ => skips.push(NumericSkip {
            metric: metric.into(),
            status_a: option_status(a.is_some(), missing_status),
            status_b: option_status(b.is_some(), missing_status),
        }),
    }
}

fn option_status(observed: bool, missing_status: &'static str) -> String {
    if observed {
        "observed".into()
    } else {
        missing_status.into()
    }
}

fn resource_timeline_observed(timeline: &MetricAvailability<ResourceTimeline>) -> bool {
    matches!(timeline, MetricAvailability::Observed { .. })
}

fn workload_keys(
    a: &BTreeMap<WorkloadKind, WorkloadCounts>,
    b: &BTreeMap<WorkloadKind, WorkloadCounts>,
) -> BTreeSet<WorkloadKind> {
    a.keys().chain(b.keys()).copied().collect()
}

fn workload_metric_keys(a: &SimResult, b: &SimResult) -> BTreeSet<WorkloadKind> {
    a.workload_metrics
        .keys()
        .chain(b.workload_metrics.keys())
        .copied()
        .collect()
}

fn count_delta(a: &WorkloadCounts, b: &WorkloadCounts) -> WorkloadCountDelta {
    WorkloadCountDelta {
        completed: delta_u64(a.completed, b.completed),
        failed: delta_u64(a.failed, b.failed),
        skipped: delta_u64(a.skipped, b.skipped),
    }
}

fn error_deltas(
    a: Option<&BTreeMap<String, u64>>,
    b: Option<&BTreeMap<String, u64>>,
) -> BTreeMap<String, i64> {
    let keys: BTreeSet<_> = a
        .into_iter()
        .flat_map(BTreeMap::keys)
        .chain(b.into_iter().flat_map(BTreeMap::keys))
        .collect();
    keys.into_iter()
        .filter_map(|key| {
            let delta = delta_u64(
                a.and_then(|errors| errors.get(key))
                    .copied()
                    .unwrap_or_default(),
                b.and_then(|errors| errors.get(key))
                    .copied()
                    .unwrap_or_default(),
            );
            (delta != 0).then(|| (key.clone(), delta))
        })
        .collect()
}

fn workload_latency_delta(
    a: Option<&WorkloadMetrics>,
    b: Option<&WorkloadMetrics>,
) -> WorkloadLatencyDelta {
    WorkloadLatencyDelta {
        latency_ms: latency_summary_delta(
            a.and_then(|metrics| metrics.latency_ms.as_ref()),
            b.and_then(|metrics| metrics.latency_ms.as_ref()),
        ),
        success_latency_ms: latency_summary_delta(
            a.and_then(|metrics| metrics.success_latency_ms.as_ref()),
            b.and_then(|metrics| metrics.success_latency_ms.as_ref()),
        ),
        submit_to_receipt_latency_ms: latency_summary_delta(
            a.and_then(|metrics| metrics.submit_to_receipt_latency_ms.as_ref()),
            b.and_then(|metrics| metrics.submit_to_receipt_latency_ms.as_ref()),
        ),
    }
}

fn latency_summary_delta(
    a: Option<&LatencySummary>,
    b: Option<&LatencySummary>,
) -> Option<LatencySummaryDelta> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let delta = LatencySummaryDelta {
                sample_count: delta_u64(a.sample_count, b.sample_count),
                p50_ms: delta_u64(a.p50_ms, b.p50_ms),
                p95_ms: delta_u64(a.p95_ms, b.p95_ms),
                p99_ms: delta_u64(a.p99_ms, b.p99_ms),
                p999_ms: delta_u64(a.p999_ms, b.p999_ms),
                max_ms: delta_u64(a.max_ms, b.max_ms),
            };
            (!delta.is_zero()).then_some(delta)
        }
        _ => None,
    }
}

fn availability_status<T>(availability: &MetricAvailability<T>) -> String {
    match availability {
        MetricAvailability::Observed { .. } => "observed",
        MetricAvailability::Unsupported { .. } => "unsupported",
        MetricAvailability::Unavailable { .. } => "unavailable",
    }
    .into()
}

fn delta_u64(a: u64, b: u64) -> i64 {
    saturating_delta(i128::from(b) - i128::from(a))
}

fn delta_i64(a: i64, b: i64) -> i64 {
    saturating_delta(i128::from(b) - i128::from(a))
}

fn saturating_delta(delta: i128) -> i64 {
    i64::try_from(delta).unwrap_or_else(|_| {
        if delta.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

impl LatencySummaryDelta {
    fn is_zero(&self) -> bool {
        self.sample_count == 0
            && self.p50_ms == 0
            && self.p95_ms == 0
            && self.p99_ms == 0
            && self.p999_ms == 0
            && self.max_ms == 0
    }
}

impl ChainProgressDelta {
    fn is_zero(&self) -> bool {
        self.first_height == 0
            && self.last_height == 0
            && self.max_height == 0
            && self.total_observations == 0
            && self.max_stall_ms.unwrap_or_default() == 0
    }
}
