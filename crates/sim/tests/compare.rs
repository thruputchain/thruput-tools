use serde_json::json;
use tempfile::{NamedTempFile, tempdir};

use thruput_sim::compare::{COMPARE_DIFF_SCHEMA_VERSION, ResultDiff, load_result};
use thruput_sim::error::SimError;
use thruput_sim::manifest::WorkloadKind;
use thruput_sim::result::{AssertionStatus, SIM_RESULT_SCHEMA_VERSION, SimResult, SimRunOutcome};

#[tokio::test]
async fn load_result_rejects_future_schema_version() {
    let file = NamedTempFile::new().expect("bug: temp file should be created");
    tokio::fs::write(
        file.path(),
        serde_json::to_vec(&minimal_result_json(99)).unwrap(),
    )
    .await
    .expect("bug: result should write");

    let err = load_result(file.path())
        .await
        .expect_err("future schema should reject");

    assert!(matches!(
        err,
        SimError::UnsupportedResultSchema { actual: 99, .. }
    ));
}

#[tokio::test]
async fn load_result_rejects_legacy_wasm_result_by_schema_version() {
    let file = NamedTempFile::new().expect("bug: temp file should be created");
    let mut value = minimal_result_json(3);
    value["contracts"] = json!([{"artifactKind": "wasm"}]);
    tokio::fs::write(file.path(), serde_json::to_vec(&value).unwrap())
        .await
        .expect("bug: result should write");

    let error = load_result(file.path())
        .await
        .expect_err("legacy result schema should reject before payload decoding");

    assert!(matches!(
        error,
        SimError::UnsupportedResultSchema { actual: 3, .. }
    ));
}

#[tokio::test]
async fn load_result_accepts_legacy_target_ownership_without_process() {
    let file = NamedTempFile::new().expect("bug: temp file should be created");
    tokio::fs::write(
        file.path(),
        serde_json::to_vec(&minimal_result_json(SIM_RESULT_SCHEMA_VERSION)).unwrap(),
    )
    .await
    .expect("bug: result should write");

    let result = load_result(file.path())
        .await
        .expect("bug: legacy ownership result should load");

    assert!(result.target_ownership.process.is_none());
}

#[tokio::test]
async fn load_result_accepts_augmented_baseline_diff_results() {
    let dir = tempdir().expect("bug: temp dir should be created");
    let baseline = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let current = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut current_json = serde_json::to_value(&current).expect("bug: result should serialize");
    current_json["baselineDiff"] = json!({
        "path": "baselines/result.json",
        "diff": ResultDiff::from_results(&baseline, &current)
    });
    current_json["baselineDiff"]["diff"]["schemaVersion"] = json!(1);
    current_json["baselineDiff"]["diff"]
        .as_object_mut()
        .expect("bug: diff should be an object")
        .remove("simulationConfigMatch");
    current_json["baselineDiff"]["diff"]
        .as_object_mut()
        .expect("bug: diff should be an object")
        .remove("runtimeConfigMatch");
    tokio::fs::write(
        dir.path().join("result.json"),
        serde_json::to_vec(&current_json).expect("bug: result should encode"),
    )
    .await
    .expect("bug: result should write");

    let loaded = load_result(dir.path())
        .await
        .expect("bug: augmented result should load");
    let loaded_baseline_diff = loaded
        .baseline_diff
        .as_ref()
        .expect("bug: baseline diff should deserialize");
    assert_eq!(loaded_baseline_diff.path, "baselines/result.json");
    assert_eq!(loaded_baseline_diff.diff.schema_version, 1);
    assert!(loaded_baseline_diff.diff.simulation_config_match.is_none());
    assert!(loaded_baseline_diff.diff.runtime_config_match.is_none());

    // A loaded augmented result must itself re-serialize without losing the
    // embedded baselineDiff, so it can serve as a baseline for a later run.
    let reserialized_json =
        serde_json::to_value(&loaded).expect("bug: loaded result should re-serialize");
    assert!(
        reserialized_json["baselineDiff"]["diff"]
            .get("simulationConfigMatch")
            .is_none()
    );
    assert!(
        reserialized_json["baselineDiff"]["diff"]
            .get("runtimeConfigMatch")
            .is_none()
    );
    let reserialized: SimResult = serde_json::from_value(reserialized_json)
        .expect("bug: re-serialized result should deserialize");
    assert_eq!(reserialized, loaded);
}

#[test]
fn compare_reports_workload_latency_assertion_and_chain_deltas() {
    let mut a = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a["workloadCounts"] = json!({
        "chain-tip": {"completed": 2, "failed": 1, "skipped": 0}
    });
    a["workloadMetrics"] = json!({
        "chain-tip": {
            "counts": {"completed": 2, "failed": 1, "skipped": 0},
            "errorCounts": {"rpc": 1},
            "latencyMs": latency_json(2, 10)
        }
    });
    a["chainProgress"] = json!({
        "firstHeight": 10,
        "lastHeight": 12,
        "maxHeight": 12,
        "totalObservations": 2,
        "maxStallMs": 1_000,
        "observations": []
    });
    a["assertionResults"] = json!([{
        "name": "maxChainStall",
        "status": "passed",
        "observed": "0ms",
        "threshold": "1000ms"
    }]);

    let mut b = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b["workloadCounts"] = json!({
        "chain-tip": {"completed": 5, "failed": 1, "skipped": 1}
    });
    b["workloadMetrics"] = json!({
        "chain-tip": {
            "counts": {"completed": 5, "failed": 1, "skipped": 1},
            "errorCounts": {"rpc": 3},
            "latencyMs": latency_json(5, 18)
        }
    });
    b["chainProgress"] = json!({
        "firstHeight": 11,
        "lastHeight": 20,
        "maxHeight": 21,
        "totalObservations": 5,
        "maxStallMs": 4_000,
        "observations": []
    });
    b["assertionResults"] = json!([{
        "name": "maxChainStall",
        "status": "failed",
        "observed": "2000ms",
        "threshold": "1000ms"
    }]);

    let diff = ResultDiff::from_results(&result_from_json(a), &result_from_json(b));

    let count_delta = diff
        .workload_count_deltas
        .get(&WorkloadKind::ChainTip)
        .expect("bug: chain-tip count delta should exist");
    assert_eq!(count_delta.completed, 3);
    assert_eq!(count_delta.skipped, 1);
    assert_eq!(
        diff.workload_error_deltas[&WorkloadKind::ChainTip]["rpc"],
        2
    );
    assert_eq!(
        diff.latency_deltas_ms[&WorkloadKind::ChainTip]
            .latency_ms
            .as_ref()
            .expect("bug: latency delta should exist")
            .p99_ms,
        8
    );
    assert_eq!(diff.assertion_transitions.len(), 1);
    assert_eq!(
        diff.assertion_transitions[0].status_b,
        Some(AssertionStatus::Failed)
    );
    assert_eq!(
        diff.chain_progress_delta
            .as_ref()
            .expect("bug: chain delta should exist")
            .last_height,
        8
    );
    assert_eq!(
        diff.chain_progress_delta
            .as_ref()
            .expect("bug: chain delta should exist")
            .max_stall_ms,
        Some(3_000)
    );
}

#[test]
fn compare_reports_cross_config_mismatch() {
    let a = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["manifestHashes"]["simulationManifestSha256"] = json!("different-sim");
    b_json["manifestHashes"]["simulationConfigSha256"] = json!("different-config");
    let b = result_from_json(b_json);

    let diff = ResultDiff::from_results(&a, &b);

    assert!(!diff.same_config);
    assert!(!diff.simulation_manifest_match);
    assert_eq!(diff.simulation_config_match, Some(false));
    assert!(diff.runtime_manifest_match);
}

#[test]
fn compare_treats_run_identity_difference_as_same_config() {
    let a = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["nonceNamespace"] = json!(2);
    b_json["manifestHashes"]["simulationManifestSha256"] = json!("different-sim");
    let b = result_from_json(b_json);

    let diff = ResultDiff::from_results(&a, &b);

    assert!(diff.same_config);
    assert!(!diff.simulation_manifest_match);
    assert_eq!(diff.simulation_config_match, Some(true));
}

#[test]
fn compare_falls_back_to_exact_manifest_match_for_legacy_results() {
    let mut a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a_json["manifestHashes"]
        .as_object_mut()
        .expect("bug: manifest hashes should be an object")
        .remove("simulationConfigSha256");
    b_json["manifestHashes"]
        .as_object_mut()
        .expect("bug: manifest hashes should be an object")
        .remove("simulationConfigSha256");

    let matching = ResultDiff::from_results(
        &result_from_json(a_json.clone()),
        &result_from_json(b_json.clone()),
    );
    assert_eq!(matching.simulation_config_match, Some(true));
    assert!(matching.same_config);

    b_json["manifestHashes"]["simulationManifestSha256"] = json!("different-sim");
    let different = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));
    assert_eq!(different.simulation_config_match, Some(false));
    assert!(!different.same_config);
}

#[test]
fn compare_falls_back_to_exact_runtime_match_for_legacy_results() {
    let mut a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a_json["manifestHashes"]
        .as_object_mut()
        .expect("bug: manifest hashes should be an object")
        .remove("runtimeConfigSha256");
    b_json["manifestHashes"]
        .as_object_mut()
        .expect("bug: manifest hashes should be an object")
        .remove("runtimeConfigSha256");

    let matching = ResultDiff::from_results(
        &result_from_json(a_json.clone()),
        &result_from_json(b_json.clone()),
    );
    assert_eq!(matching.runtime_config_match, Some(true));
    assert!(matching.same_config);

    b_json["manifestHashes"]["runtimeManifestSha256"] = json!("different-runtime");
    let different = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));
    assert_eq!(different.runtime_config_match, Some(false));
    assert!(!different.same_config);
}

#[test]
fn compare_treats_runtime_dial_address_difference_as_same_config() {
    let a = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["manifestHashes"]["runtimeManifestSha256"] = json!("different-runtime");
    let b = result_from_json(b_json);

    let diff = ResultDiff::from_results(&a, &b);

    assert!(diff.same_config);
    assert!(!diff.runtime_manifest_match);
    assert_eq!(diff.runtime_config_match, Some(true));
}

#[test]
fn compare_reports_runtime_config_mismatch() {
    let a = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["manifestHashes"]["runtimeManifestSha256"] = json!("different-runtime");
    b_json["manifestHashes"]["runtimeConfigSha256"] = json!("different-runtime-config");
    let b = result_from_json(b_json);

    let diff = ResultDiff::from_results(&a, &b);

    assert!(!diff.same_config);
    assert!(!diff.runtime_manifest_match);
    assert_eq!(diff.runtime_config_match, Some(false));
}

#[test]
fn compare_reports_metric_availability_transitions_and_numeric_skips() {
    let a = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["metricAvailability"]["storageTokensConsumed"] = json!({
        "status": "unavailable",
        "reason": "collector failed"
    });
    b_json["metricAvailability"]["nnintComputeUsed"] = json!({
        "status": "observed",
        "value": 9
    });
    let b = result_from_json(b_json);

    let diff = ResultDiff::from_results(&a, &b);

    assert_eq!(diff.storage_tokens_a, "unsupported");
    assert_eq!(diff.storage_tokens_b, "unavailable");
    assert!(diff.metric_numeric_deltas.is_empty());
    assert_eq!(diff.numeric_skips.len(), 2);
    assert!(diff.numeric_skips.iter().any(|skip| {
        skip.metric == "nnintComputeUsed"
            && skip.status_a == "unsupported"
            && skip.status_b == "observed"
    }));
}

#[test]
fn compare_reports_observed_metric_delta() {
    let mut a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a_json["metricAvailability"]["storageTokensConsumed"] = json!({
        "status": "observed",
        "value": 10
    });
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["metricAvailability"]["storageTokensConsumed"] = json!({
        "status": "observed",
        "value": 14
    });

    let diff = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));

    assert!(
        diff.metric_numeric_deltas
            .iter()
            .any(|delta| { delta.metric == "storageTokensConsumed" && delta.value == 4 })
    );
}

#[test]
fn compare_reports_rate_float_deltas() {
    let mut a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a_json["backpressureRate"] = json!(0.1);
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["backpressureRate"] = json!(0.25);
    b_json["simulatorOverloadRate"] = json!(0.01);

    let diff = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));

    assert!(
        diff.metric_float_deltas.iter().any(|delta| {
            delta.metric == "backpressureRate" && (delta.value - 0.15).abs() < 1e-12
        })
    );
    assert!(diff.metric_float_deltas.iter().any(|delta| {
        delta.metric == "simulatorOverloadRate" && (delta.value - 0.01).abs() < 1e-12
    }));
}

#[test]
fn compare_reports_resource_timeline_numeric_deltas() {
    let mut a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a_json["resourceTimeline"] = resource_timeline_json(100, 1_000, 5);
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["resourceTimeline"] = resource_timeline_json(250, 1_500, 7);

    let diff = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));

    assert!(diff.metric_numeric_deltas.iter().any(|delta| {
        delta.metric == "resourceTimeline.maxSegmentRssGrowthBytesPerMinute" && delta.value == 150
    }));
    assert!(
        diff.metric_numeric_deltas
            .iter()
            .any(|delta| { delta.metric == "resourceTimeline.maxRssBytes" && delta.value == 500 })
    );
    assert!(
        diff.metric_numeric_deltas
            .iter()
            .any(|delta| { delta.metric == "resourceTimeline.totalCpuMs" && delta.value == 2 })
    );
}

#[test]
fn compare_reports_insufficient_samples_for_missing_rss_growth() {
    let mut a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a_json["resourceTimeline"] = resource_timeline_json(100, 1_000, 5);
    a_json["resourceTimeline"]["value"]["maxSegmentRssGrowthBytesPerMinute"] = json!(null);
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["resourceTimeline"] = resource_timeline_json(250, 1_500, 7);
    b_json["resourceTimeline"]["value"]["maxSegmentRssGrowthBytesPerMinute"] = json!(null);

    let diff = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));

    assert!(
        diff.metric_numeric_deltas
            .iter()
            .all(|delta| { delta.metric != "resourceTimeline.maxSegmentRssGrowthBytesPerMinute" })
    );
    assert!(diff.numeric_skips.iter().any(|skip| {
        skip.metric == "resourceTimeline.maxSegmentRssGrowthBytesPerMinute"
            && skip.status_a == "insufficient-samples"
            && skip.status_b == "insufficient-samples"
    }));
}

#[test]
fn compare_skips_resource_deltas_when_only_one_side_is_available() {
    let a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["resourceTimeline"] = resource_timeline_json(250, 1_500, 7);

    let diff = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));

    assert!(
        diff.metric_numeric_deltas
            .iter()
            .all(|delta| { !delta.metric.starts_with("resourceTimeline.") })
    );
    assert!(diff.numeric_skips.iter().any(|skip| {
        skip.metric == "resourceTimeline.maxRssBytes"
            && skip.status_a == "unavailable"
            && skip.status_b == "observed"
    }));
}

#[test]
fn compare_diff_carries_schema_version_in_json() {
    let result = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));

    let diff = ResultDiff::from_results(&result, &result);
    let json = serde_json::to_value(&diff).expect("bug: diff should serialize");

    assert_eq!(json["schemaVersion"], json!(COMPARE_DIFF_SCHEMA_VERSION));
}

#[test]
fn compare_surfaces_outcome_transition() {
    let a = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["outcome"] = json!("aborted");
    let b = result_from_json(b_json);

    let diff = ResultDiff::from_results(&a, &b);

    assert_eq!(diff.outcome_a, SimRunOutcome::Passed);
    assert_eq!(diff.outcome_b, SimRunOutcome::Aborted);
    assert!(diff.render_text().contains("outcome: passed -> aborted"));
}

#[test]
fn compare_text_lists_all_match_flags() {
    let result = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));

    let text = ResultDiff::from_results(&result, &result).render_text();

    assert!(text.contains("simulation manifest match:"));
    assert!(text.contains("simulation config match:"));
    assert!(text.contains("runtime manifest match:"));
    assert!(text.contains("runtime config match:"));
    assert!(text.contains("runtime identity match:"));
    assert!(text.contains("producer identity match:"));
}

#[test]
fn chain_progress_delta_saturates_on_overflow() {
    let mut a_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    a_json["chainProgress"] = json!({
        "firstHeight": i64::MAX,
        "lastHeight": i64::MAX,
        "maxHeight": i64::MAX,
        "totalObservations": 1,
        "observations": []
    });
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["chainProgress"] = json!({
        "firstHeight": i64::MIN,
        "lastHeight": i64::MIN,
        "maxHeight": i64::MIN,
        "totalObservations": 1,
        "observations": []
    });

    let diff = ResultDiff::from_results(&result_from_json(a_json), &result_from_json(b_json));

    let delta = diff
        .chain_progress_delta
        .as_ref()
        .expect("bug: chain delta should exist");
    assert_eq!(delta.first_height, i64::MIN);
    assert_eq!(delta.last_height, i64::MIN);
    assert_eq!(delta.max_height, i64::MIN);
}

#[test]
fn compare_omits_zero_latency_and_chain_deltas() {
    let mut value = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    value["workloadMetrics"] = json!({
        "chain-tip": {
            "counts": {"completed": 2, "failed": 0, "skipped": 0},
            "latencyMs": latency_json(2, 10)
        }
    });
    value["chainProgress"] = json!({
        "firstHeight": 10,
        "lastHeight": 12,
        "maxHeight": 12,
        "totalObservations": 2,
        "observations": []
    });
    let result = result_from_json(value);

    let diff = ResultDiff::from_results(&result, &result);

    assert!(diff.latency_deltas_ms.is_empty());
    assert!(diff.chain_progress_delta.is_none());
}

#[test]
fn compare_ignores_contract_record_differences_for_now() {
    let a = result_from_json(minimal_result_json(SIM_RESULT_SCHEMA_VERSION));
    let mut b_json = minimal_result_json(SIM_RESULT_SCHEMA_VERSION);
    b_json["contracts"] = json!([{
        "name": "fibonacci",
        "status": "completed",
        "artifactKind": "rv32",
        "artifactPath": "/tmp/fibonacci.rv32a",
        "artifactSha256": format!("0x{}", "11".repeat(32)),
        "contractHash": format!("0x{}", "22".repeat(32)),
        "nonce": "123",
        "deployAddressNonce": format!("0x{}", "33".repeat(32)),
        "txHash": format!("0x{}", "44".repeat(32)),
        "address": format!("0x{}", "55".repeat(32)),
        "addressSource": "derived",
        "height": 42
    }]);
    let b = result_from_json(b_json);

    let diff = ResultDiff::from_results(&a, &b);

    assert!(diff.same_config);
    assert!(diff.workload_count_deltas.is_empty());
    assert!(diff.workload_error_deltas.is_empty());
    assert!(diff.latency_deltas_ms.is_empty());
    assert!(diff.assertion_transitions.is_empty());
    assert!(diff.chain_progress_delta.is_none());
}

fn result_from_json(value: serde_json::Value) -> SimResult {
    serde_json::from_value(value).expect("bug: result fixture should parse")
}

fn latency_json(sample_count: u64, base: u64) -> serde_json::Value {
    json!({
        "sampleCount": sample_count,
        "p50Ms": base,
        "p95Ms": base,
        "p99Ms": base,
        "p999Ms": base,
        "maxMs": base
    })
}

fn resource_timeline_json(
    max_growth: i64,
    max_rss_bytes: u64,
    total_cpu_ms: u64,
) -> serde_json::Value {
    json!({
        "status": "observed",
        "value": {
            "scope": "process-group",
            "sampleIntervalMs": 1000,
            "sampleCount": 2,
            "first": resource_sample_json(0, 42, 900),
            "last": resource_sample_json(1000, 42, max_rss_bytes),
            "maxRssBytes": max_rss_bytes,
            "maxSegmentRssGrowthBytesPerMinute": max_growth,
            "lastSegmentRssGrowthBytesPerMinute": max_growth,
            "peakThreads": 1,
            "peakOpenFds": 2,
            "totalCpuMs": total_cpu_ms,
            "segments": [{
                "scope": "process-group",
                "rootPid": 42,
                "pgid": 42,
                "sampleCount": 2,
                "startedAtUnixMs": "0",
                "endedAtUnixMs": "1000",
                "firstRssBytes": 900,
                "lastRssBytes": max_rss_bytes,
                "maxRssBytes": max_rss_bytes,
                "rssGrowthBytesPerMinute": max_growth,
                "totalCpuMs": total_cpu_ms
            }],
            "samples": []
        }
    })
}

fn resource_sample_json(
    simulation_time_ms: u64,
    root_pid: u32,
    rss_bytes: u64,
) -> serde_json::Value {
    json!({
        "simulationTimeMs": simulation_time_ms,
        "observedAtUnixMs": simulation_time_ms.to_string(),
        "scope": "process-group",
        "rootPid": root_pid,
        "pgid": root_pid,
        "processCount": 1,
        "rssBytes": rss_bytes,
        "cpuUserMs": 1,
        "cpuSystemMs": 0,
        "threads": 1,
        "openFds": 2
    })
}

fn minimal_result_json(schema_version: u32) -> serde_json::Value {
    json!({
        "schemaVersion": schema_version,
        "runId": "result-test-run",
        "name": "result-test",
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
            "simulationConfigSha256": "sim-config",
            "runtimeManifestSha256": "runtime",
            "runtimeConfigSha256": "runtime-config",
            "bootstrapManifest": {
                "status": "unsupported",
                "reason": "existing target"
            }
        },
        "metricAvailability": {
            "storageTokensConsumed": {
                "status": "unsupported",
                "reason": "not surfaced"
            },
            "nnintComputeUsed": {
                "status": "unsupported",
                "reason": "not surfaced"
            }
        }
    })
}
