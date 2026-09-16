//! Versioned benchmark results and human-readable reporting.

use hdrhistogram::Histogram;

use crate::request::RequestErrorKind;

pub(super) const BENCH_RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestErrorCounts {
    pub(super) transport: u64,
    pub(super) http_status: u64,
    pub(super) invalid_json_rpc_response: u64,
    pub(super) json_rpc: u64,
}

impl RequestErrorCounts {
    pub(super) fn record(&mut self, kind: RequestErrorKind) {
        match kind {
            RequestErrorKind::Transport => self.transport += 1,
            RequestErrorKind::HttpStatus => self.http_status += 1,
            RequestErrorKind::InvalidJsonRpcResponse => self.invalid_json_rpc_response += 1,
            RequestErrorKind::JsonRpc => self.json_rpc += 1,
        }
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.transport += other.transport;
        self.http_status += other.http_status;
        self.invalid_json_rpc_response += other.invalid_json_rpc_response;
        self.json_rpc += other.json_rpc;
    }

    pub(super) fn total(self) -> u64 {
        self.transport + self.http_status + self.invalid_json_rpc_response + self.json_rpc
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BenchResult {
    pub(super) schema_version: u32,
    pub(super) kind: &'static str,
    pub(super) generated_at_unix_ms: u128,
    pub(super) producer: ProducerMetadata,
    pub(super) platform: PlatformMetadata,
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
pub(super) struct PlatformMetadata {
    pub(super) os: &'static str,
    pub(super) arch: &'static str,
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
    pub(super) scenario: Option<String>,
    pub(super) target_label: Option<String>,
    pub(super) url: String,
    pub(super) method: String,
    pub(super) mode: &'static str,
    pub(super) target_requests_per_second: Option<u64>,
    pub(super) duration_seconds: u64,
    pub(super) warmup_seconds: u64,
    pub(super) workers: usize,
    pub(super) connections_per_worker: usize,
    pub(super) max_inflight_per_worker: usize,
    pub(super) accounts: usize,
    pub(super) transaction_batch_size: usize,
    pub(super) max_skew_seconds: f64,
    pub(super) request_timeout_ms: u64,
    pub(super) max_response_bytes: usize,
    pub(super) runtime_manifest: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CalibrationMetrics {
    pub(super) median_rtt_us: u64,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestMetrics {
    pub(super) total: u64,
    pub(super) successful: u64,
    pub(super) failed: u64,
    pub(super) errors: RequestErrorCounts,
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

    println!("\n=== Groundhog RPC Benchmark Results ===");
    println!("Method:      {}", config.method);
    if let Some(target) = config.target_requests_per_second {
        println!("Target:      {target} req/s");
        println!(
            "Achieved:    {:.0} req/s ({:.1}% of target)",
            rates.attempted_requests_per_second,
            rates.target_achievement_percent.unwrap_or_default(),
        );
    } else {
        println!("Target:      unthrottled");
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
            "Errors:      transport={} http={} invalid-json-rpc={} json-rpc={}",
            requests.errors.transport,
            requests.errors.http_status,
            requests.errors.invalid_json_rpc_response,
            requests.errors.json_rpc,
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
        })
        .expect("bug: error counts should serialize");

        assert_eq!(
            value,
            json!({
                "transport": 1,
                "httpStatus": 2,
                "invalidJsonRpcResponse": 3,
                "jsonRpc": 4,
            })
        );
    }

    #[test]
    fn benchmark_result_has_stable_json_shape() {
        let value = serde_json::to_value(BenchResult {
            schema_version: BENCH_RESULT_SCHEMA_VERSION,
            kind: "groundhog-bench-result",
            generated_at_unix_ms: 1_700_000_000_000,
            producer: ProducerMetadata {
                name: "groundhog-bench",
                version: "0.9.2",
                build_profile: "release",
                git_commit: Some("abcdef0".into()),
                git_dirty: Some(false),
            },
            platform: PlatformMetadata {
                os: "linux",
                arch: "x86_64",
            },
            runtime: Some(RuntimeIdentity {
                manifest_sha256: format!("0x{}", "11".repeat(32)),
                manifest_schema_version: 2,
                kind: "hogd-dev-runtime".into(),
                chain_state: Some("fresh".into()),
                network_hash: "22".repeat(32),
                protocol: "hogp/abcdef012345000".into(),
                producer: RuntimeProducerIdentity {
                    binary: "hog-devnode".into(),
                    version: "1.2.3".into(),
                    commit_hash: Some("7654321".into()),
                },
            }),
            configuration: BenchConfiguration {
                scenario: Some("gateway-baseline".into()),
                target_label: Some("standalone gateway baseline".into()),
                url: "http://127.0.0.1:9546".into(),
                method: "chainTip".into(),
                mode: "rate_limited",
                target_requests_per_second: Some(250),
                duration_seconds: 30,
                warmup_seconds: 2,
                workers: 4,
                connections_per_worker: 16,
                max_inflight_per_worker: 32,
                accounts: 10,
                transaction_batch_size: 0,
                max_skew_seconds: 1.0,
                request_timeout_ms: 10_000,
                max_response_bytes: 1_048_576,
                runtime_manifest: Some("/tmp/runtime-manifest.json".into()),
            },
            calibration: CalibrationMetrics { median_rtt_us: 800 },
            requests: RequestMetrics {
                total: 7_500,
                successful: 7_496,
                failed: 4,
                errors: RequestErrorCounts {
                    transport: 1,
                    http_status: 1,
                    invalid_json_rpc_response: 1,
                    json_rpc: 1,
                },
            },
            rates: RateMetrics {
                attempted_requests_per_second: 250.0,
                successful_requests_per_second: 249.866,
                target_achievement_percent: Some(100.0),
                run_wall_time_ms: 32_010.5,
            },
            successful_latency_us: Some(LatencyMetrics {
                samples: 7_496,
                min: 410,
                p50: 700,
                p90: 900,
                p95: 1_000,
                p99: 1_200,
                p999: 1_500,
                max: 2_000,
                mean: 725.5,
            }),
        })
        .expect("bug: benchmark result should serialize");

        assert_eq!(
            value,
            json!({
                "schemaVersion": 1,
                "kind": "groundhog-bench-result",
                "generatedAtUnixMs": 1_700_000_000_000_u64,
                "producer": {
                    "name": "groundhog-bench",
                    "version": "0.9.2",
                    "buildProfile": "release",
                    "gitCommit": "abcdef0",
                    "gitDirty": false,
                },
                "platform": {
                    "os": "linux",
                    "arch": "x86_64",
                },
                "runtime": {
                    "manifestSha256": format!("0x{}", "11".repeat(32)),
                    "manifestSchemaVersion": 2,
                    "kind": "hogd-dev-runtime",
                    "chainState": "fresh",
                    "networkHash": "22".repeat(32),
                    "protocol": "hogp/abcdef012345000",
                    "producer": {
                        "binary": "hog-devnode",
                        "version": "1.2.3",
                        "commitHash": "7654321",
                    },
                },
                "configuration": {
                    "scenario": "gateway-baseline",
                    "targetLabel": "standalone gateway baseline",
                    "url": "http://127.0.0.1:9546",
                    "method": "chainTip",
                    "mode": "rate_limited",
                    "targetRequestsPerSecond": 250,
                    "durationSeconds": 30,
                    "warmupSeconds": 2,
                    "workers": 4,
                    "connectionsPerWorker": 16,
                    "maxInflightPerWorker": 32,
                    "accounts": 10,
                    "transactionBatchSize": 0,
                    "maxSkewSeconds": 1.0,
                    "requestTimeoutMs": 10_000,
                    "maxResponseBytes": 1_048_576,
                    "runtimeManifest": "/tmp/runtime-manifest.json",
                },
                "calibration": {
                    "medianRttUs": 800,
                },
                "requests": {
                    "total": 7_500,
                    "successful": 7_496,
                    "failed": 4,
                    "errors": {
                        "transport": 1,
                        "httpStatus": 1,
                        "invalidJsonRpcResponse": 1,
                        "jsonRpc": 1,
                    },
                },
                "rates": {
                    "attemptedRequestsPerSecond": 250.0,
                    "successfulRequestsPerSecond": 249.866,
                    "targetAchievementPercent": 100.0,
                    "runWallTimeMs": 32_010.5,
                },
                "successfulLatencyUs": {
                    "samples": 7_496,
                    "min": 410,
                    "p50": 700,
                    "p90": 900,
                    "p95": 1_000,
                    "p99": 1_200,
                    "p999": 1_500,
                    "max": 2_000,
                    "mean": 725.5,
                },
            })
        );
    }

    #[test]
    fn producer_metadata_preserves_unknown_git_provenance() {
        let value = serde_json::to_value(ProducerMetadata {
            name: "groundhog-bench",
            version: "0.9.2",
            build_profile: "release",
            git_commit: None,
            git_dirty: None,
        })
        .expect("bug: producer metadata should serialize");

        assert_eq!(
            value,
            json!({
                "name": "groundhog-bench",
                "version": "0.9.2",
                "buildProfile": "release",
                "gitCommit": null,
                "gitDirty": null,
            })
        );
    }
}
