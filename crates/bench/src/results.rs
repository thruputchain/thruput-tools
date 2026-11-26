//! Versioned benchmark results and human-readable reporting.

use hdrhistogram::Histogram;

use crate::corpus::CorpusIdentity;
use crate::metadata::{HostMetadata, Observation, TargetMetadata};
use crate::request::{RequestErrorKind, RequestOutcome};

pub(super) const BENCH_RESULT_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestErrorCounts {
    pub(super) transport: u64,
    pub(super) http_status: u64,
    pub(super) invalid_json_rpc_response: u64,
    pub(super) json_rpc: u64,
    pub(super) semantic: u64,
}

impl RequestErrorCounts {
    pub(super) fn record(&mut self, kind: RequestErrorKind) {
        match kind {
            RequestErrorKind::Transport => self.transport += 1,
            RequestErrorKind::HttpStatus => self.http_status += 1,
            RequestErrorKind::InvalidJsonRpcResponse => self.invalid_json_rpc_response += 1,
            RequestErrorKind::JsonRpc => self.json_rpc += 1,
            RequestErrorKind::Semantic => self.semantic += 1,
        }
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.transport += other.transport;
        self.http_status += other.http_status;
        self.invalid_json_rpc_response += other.invalid_json_rpc_response;
        self.json_rpc += other.json_rpc;
        self.semantic += other.semantic;
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BenchResult {
    pub(super) schema_version: u32,
    pub(super) kind: &'static str,
    pub(super) outcome: &'static str,
    pub(super) generated_at_unix_ms: u128,
    pub(super) producer: ProducerMetadata,
    pub(super) platform: HostMetadata,
    pub(super) server_observations: crate::observations::ServerObservations,
    pub(super) benchmark_binary_sha256: Observation<String>,
    pub(super) runtime_worker_threads: usize,
    pub(super) target: TargetMetadata,
    pub(super) corpus: Option<CorpusIdentity>,
    pub(super) run: RunIdentity,
    pub(super) measurement: Measurement,
    pub(super) drained: RequestMetrics,
    pub(super) runtime: Option<RuntimeIdentity>,
    pub(super) configuration: BenchConfiguration,
    pub(super) calibration: CalibrationMetrics,
    pub(super) requests: RequestMetrics,
    pub(super) rates: RateMetrics,
    pub(super) successful_latency_us: Option<LatencyMetrics>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProducerMetadata {
    pub(super) name: &'static str,
    pub(super) version: &'static str,
    pub(super) build_profile: &'static str,
    pub(super) git_commit: Option<String>,
    pub(super) git_dirty: Option<bool>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RunIdentity {
    pub(super) scenario: Option<String>,
    pub(super) target_label: Option<String>,
    pub(super) url: String,
    pub(super) runtime_manifest: Option<String>,
    pub(super) hashes_file: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Measurement {
    pub(super) inclusion: &'static str,
    pub(super) window_ms: u64,
    pub(super) drain_ms: f64,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuntimeIdentity {
    pub(super) manifest_sha256: String,
    pub(super) manifest_schema_version: u32,
    pub(super) kind: String,
    pub(super) chain_state: Option<String>,
    pub(super) network_hash: String,
    pub(super) protocol: String,
    pub(super) producer: RuntimeProducerIdentity,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuntimeProducerIdentity {
    pub(super) binary: String,
    pub(super) version: String,
    pub(super) commit_hash: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BenchConfiguration {
    pub(super) method: String,
    pub(super) mode: &'static str,
    pub(super) target_requests_per_second: Option<u64>,
    pub(super) duration_seconds: u64,
    pub(super) warmup_seconds: u64,
    pub(super) workers: usize,
    pub(super) connections_per_worker: usize,
    pub(super) max_inflight_per_worker: Option<usize>,
    pub(super) outstanding: Option<usize>,
    pub(super) effective_max_inflight: usize,
    pub(super) accounts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) account_fixture_sha256: Option<String>,
    /// Expiry height signed into submitted transactions; it sets how long the
    /// chain keeps their replay-cache entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) block_expiry_height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tx_kind: Option<crate::cli::TxKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) transfer_amount_raw: Option<u64>,
    pub(super) transaction_batch_size: usize,
    pub(super) max_skew_seconds: f64,
    pub(super) request_timeout_ms: u64,
    pub(super) max_response_bytes: usize,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CalibrationMetrics {
    pub(super) median_rtt_us: u64,
}

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestMetrics {
    pub(super) total: u64,
    pub(super) successful: u64,
    pub(super) failed: u64,
    pub(super) errors: RequestErrorCounts,
}

impl RequestMetrics {
    pub(super) fn record(&mut self, outcome: RequestOutcome) {
        self.total += 1;
        match outcome {
            RequestOutcome::Success => self.successful += 1,
            RequestOutcome::Error(kind) => {
                self.failed += 1;
                self.errors.record(kind);
            }
        }
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.total += other.total;
        self.successful += other.successful;
        self.failed += other.failed;
        self.errors.merge(other.errors);
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RateMetrics {
    pub(super) attempted_requests_per_second: f64,
    pub(super) successful_requests_per_second: f64,
    pub(super) target_achievement_percent: Option<f64>,
    pub(super) run_wall_time_ms: f64,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct LatencyMetrics {
    pub(super) samples: u64,
    pub(super) min: u64,
    pub(super) p50: u64,
    pub(super) p90: u64,
    pub(super) p95: u64,
    pub(super) p99: u64,
    pub(super) p999: u64,
    pub(super) max: u64,
    pub(super) mean: f64,
}

pub(super) fn latency_metrics(histogram: &Histogram<u64>) -> Option<LatencyMetrics> {
    if histogram.is_empty() {
        return None;
    }
    Some(LatencyMetrics {
        samples: histogram.len(),
        min: histogram.min(),
        p50: histogram.value_at_percentile(50.0),
        p90: histogram.value_at_percentile(90.0),
        p95: histogram.value_at_percentile(95.0),
        p99: histogram.value_at_percentile(99.0),
        p999: histogram.value_at_percentile(99.9),
        max: histogram.max(),
        mean: histogram.mean(),
    })
}

pub(super) fn benchmark_git_commit() -> Option<String> {
    std::env::var("GROUNDHOG_BENCH_GIT_COMMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| option_env!("GIT_COMMIT").map(str::to_owned))
}

pub(super) fn benchmark_git_dirty() -> Option<bool> {
    match std::env::var("GROUNDHOG_BENCH_GIT_DIRTY").as_deref() {
        Ok("true") => Some(true),
        Ok("false") => Some(false),
        _ => None,
    }
}

pub(super) fn print_human_result(result: &BenchResult) {
    let config = &result.configuration;
    let requests = &result.requests;
    let rates = &result.rates;

    println!("\n=== Thruput RPC Benchmark Results ===");
    println!("Method:      {}", config.method);
    println!("Outcome:     {}", result.outcome);
    if let Some(target) = config.target_requests_per_second {
        println!("Target:      {target} req/s");
        println!(
            "Achieved:    {:.0} req/s ({:.1}% of target)",
            rates.attempted_requests_per_second,
            rates.target_achievement_percent.unwrap_or_default(),
        );
    } else {
        if let Some(n) = config.outstanding {
            println!("Target:      {n} global outstanding");
        } else {
            println!("Target:      unthrottled");
        }
        println!(
            "Attempted:   {:.0} req/s",
            rates.attempted_requests_per_second
        );
        println!(
            "Achieved:    {:.0} successful req/s",
            rates.successful_requests_per_second
        );
    }
    println!(
        "Duration:    {}s ({}s warmup)",
        config.duration_seconds, config.warmup_seconds
    );
    println!(
        "Workers:     {} x {} connections",
        config.workers, config.connections_per_worker,
    );
    println!(
        "Requests:    {} ({} errors, {:.1}%)",
        requests.total,
        requests.failed,
        if requests.total > 0 {
            requests.failed as f64 / requests.total as f64 * 100.0
        } else {
            0.0
        },
    );
    if requests.failed > 0 {
        println!(
            "Errors:      transport={} http={} invalid-json-rpc={} json-rpc={} semantic={}",
            requests.errors.transport,
            requests.errors.http_status,
            requests.errors.invalid_json_rpc_response,
            requests.errors.json_rpc,
            requests.errors.semantic,
        );
    }
    println!(
        "Throughput:  {:.0} successful req/s",
        rates.successful_requests_per_second
    );
    println!(
        "Median RTT:  {} us (from calibration)",
        result.calibration.median_rtt_us
    );
    println!();
    println!(
        "Drain:       {} completions excluded from throughput ({:.1} ms)",
        result.drained.total, result.measurement.drain_ms
    );
    println!("Latency (microseconds):");
    if let Some(latency) = &result.successful_latency_us {
        println!("  p50:  {}", latency.p50);
        println!("  p90:  {}", latency.p90);
        println!("  p95:  {}", latency.p95);
        println!("  p99:  {}", latency.p99);
        println!("  p999: {}", latency.p999);
        println!("  max:  {}", latency.max);
        println!("  mean: {:.0}", latency.mean);
    } else {
        println!("  (no samples recorded)");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn error_counts_have_stable_json_field_names() {
        let value = serde_json::to_value(RequestErrorCounts {
            transport: 1,
            http_status: 2,
            invalid_json_rpc_response: 3,
            json_rpc: 4,
            semantic: 5,
        })
        .expect("bug: error counts should serialize");

        assert_eq!(
            value,
            json!({
                "transport": 1,
                "httpStatus": 2,
                "invalidJsonRpcResponse": 3,
                "jsonRpc": 4,
                "semantic": 5,
            })
        );
    }

    #[test]
    fn producer_metadata_preserves_unknown_git_provenance() {
        let value = serde_json::to_value(ProducerMetadata {
            name: "thruput-bench",
            version: "0.9.2",
            build_profile: "release",
            git_commit: None,
            git_dirty: None,
        })
        .expect("bug: producer metadata should serialize");

        assert_eq!(
            value,
            json!({
                "name": "thruput-bench",
                "version": "0.9.2",
                "buildProfile": "release",
                "gitCommit": null,
                "gitDirty": null,
            })
        );
    }
}
