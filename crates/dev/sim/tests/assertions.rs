use serde_json::json;

use groundhog_sim::assertions::{AssertionEvaluation, determine_outcome, evaluate_assertions};
use groundhog_sim::manifest::{SimulationManifest, WorkloadKind};
use groundhog_sim::metrics::RunMetricsBuilder;
use groundhog_sim::result::{
    AssertionResult, AssertionStatus, ChainProgressSummary, ResourceTimeline, SimRunOutcome,
    WorkloadMetrics,
};
use groundhog_sim::result::{
    ChainTipObservation, ResourceSamplePoint, ResourceSampleScope, ResourceSegment,
};
use std::collections::BTreeMap;

#[test]
fn evaluates_max_error_rate_assertion() {
    let manifest = manifest_with_assertions(&json!({"maxErrorRate": 0.25}));
    let results = evaluate_test_assertions(&manifest, 3, 1, None, None, &BTreeMap::new());

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "maxErrorRate");
    assert_eq!(results[0].status, AssertionStatus::Passed);
}

#[test]
fn workload_failures_fail_run_even_when_assertions_pass() {
    let assertion_results = vec![AssertionResult {
        name: "maxChainStall".into(),
        status: AssertionStatus::Passed,
        observed: Some("0ms".into()),
        threshold: Some("1000ms".into()),
        reason: None,
    }];

    assert_eq!(
        determine_outcome(&assertion_results, 1),
        SimRunOutcome::Failed
    );
}

#[test]
fn fails_max_chain_stall_assertion() {
    let manifest = manifest_with_assertions(&json!({"maxChainStall": "1s"}));
    let chain_progress = ChainProgressSummary {
        first_height: 1,
        last_height: 1,
        max_height: 1,
        total_observations: 2,
        max_stall_ms: Some(2_000),
        observations: vec![
            ChainTipObservation {
                simulation_time_ms: 0,
                height: 1,
                block_hash: "0x01".into(),
            },
            ChainTipObservation {
                simulation_time_ms: 2_000,
                height: 1,
                block_hash: "0x01".into(),
            },
        ],
    };
    let results = evaluate_test_assertions(
        &manifest,
        2,
        0,
        Some(&chain_progress),
        None,
        &BTreeMap::new(),
    );

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "maxChainStall");
    assert_eq!(results[0].status, AssertionStatus::Failed);
}

#[test]
fn max_chain_stall_uses_uncapped_summary_when_available() {
    let manifest = manifest_with_assertions(&json!({"maxChainStall": "1s"}));
    let chain_progress = ChainProgressSummary {
        first_height: 1,
        last_height: 2,
        max_height: 2,
        total_observations: 1_025,
        max_stall_ms: Some(5_000),
        observations: vec![
            ChainTipObservation {
                simulation_time_ms: 0,
                height: 1,
                block_hash: "0x01".into(),
            },
            ChainTipObservation {
                simulation_time_ms: 1,
                height: 2,
                block_hash: "0x02".into(),
            },
        ],
    };

    let results = evaluate_test_assertions(
        &manifest,
        2,
        0,
        Some(&chain_progress),
        None,
        &BTreeMap::new(),
    );

    assert_eq!(results[0].status, AssertionStatus::Failed);
    assert_eq!(results[0].observed.as_deref(), Some("5000ms"));
}

#[test]
fn evaluates_p99_submit_latency_assertion() {
    let manifest = manifest_with_assertions(&json!({"p99SubmitLatencyMs": 20}));
    let mut metrics = RunMetricsBuilder::default();
    metrics.record_success(WorkloadKind::WalletSubmitSmoke, 10);
    metrics.record_success(WorkloadKind::WalletSubmitSmoke, 20);
    let finished = metrics.finish();

    let results = evaluate_test_assertions(&manifest, 2, 0, None, None, &finished.workload_metrics);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "p99SubmitLatencyMs");
    assert_eq!(results[0].status, AssertionStatus::Passed);
    assert_eq!(results[0].observed.as_deref(), Some("20ms"));
}

#[test]
fn p99_submit_latency_assertion_ignores_failed_submit_latency() {
    let manifest = manifest_with_assertions(&json!({"p99SubmitLatencyMs": 10}));
    let mut metrics = RunMetricsBuilder::default();
    metrics.record_success(WorkloadKind::WalletSubmitSmoke, 10);
    metrics.record_failure(WorkloadKind::WalletSubmitSmoke, "rpc-error", 100);
    let finished = metrics.finish();

    let results = evaluate_test_assertions(&manifest, 1, 1, None, None, &finished.workload_metrics);

    assert_eq!(results[0].status, AssertionStatus::Passed);
    assert_eq!(results[0].observed.as_deref(), Some("10ms"));
}

#[test]
fn fails_p99_submit_latency_assertion() {
    let manifest = manifest_with_assertions(&json!({"p99SubmitLatencyMs": 5}));
    let mut metrics = RunMetricsBuilder::default();
    metrics.record_success(WorkloadKind::WalletSubmitSmoke, 10);
    let finished = metrics.finish();

    let results = evaluate_test_assertions(&manifest, 1, 0, None, None, &finished.workload_metrics);

    assert_eq!(results[0].status, AssertionStatus::Failed);
    assert_eq!(results[0].observed.as_deref(), Some("10ms"));
}

#[test]
fn receipt_timeout_rate_ignores_transient_absent_polls() {
    let manifest = manifest_with_assertions(&json!({"maxReceiptTimeoutRate": 0.0}));
    let mut metrics = RunMetricsBuilder::default();
    metrics.record_success(WorkloadKind::ReceiptPoll, 1);
    metrics.record_receipt_poll(true);
    metrics.record_success(WorkloadKind::ReceiptPoll, 1);
    metrics.record_receipt_poll(false);
    let finished = metrics.finish();

    let results = evaluate_test_assertions(&manifest, 2, 0, None, None, &finished.workload_metrics);

    assert_eq!(results[0].name, "maxReceiptTimeoutRate");
    assert_eq!(results[0].status, AssertionStatus::Passed);
    assert_eq!(results[0].observed.as_deref(), Some("0.000000"));
}

#[test]
fn fails_receipt_timeout_rate_assertion() {
    let manifest = manifest_with_assertions(&json!({"maxReceiptTimeoutRate": 0.0}));
    let mut metrics = RunMetricsBuilder::default();
    metrics.record_success(WorkloadKind::ReceiptPoll, 1);
    metrics.record_pending_receipts_at_shutdown(1);
    let finished = metrics.finish();

    let results = evaluate_test_assertions(&manifest, 1, 0, None, None, &finished.workload_metrics);

    assert_eq!(results[0].status, AssertionStatus::Failed);
    assert_eq!(results[0].observed.as_deref(), Some("1.000000"));
}

#[test]
fn max_backpressure_rate_passes_at_zero_and_below_threshold() {
    let manifest = manifest_with_assertions(&json!({"maxBackpressureRate": 0.25}));
    let results = evaluate_assertions(
        &manifest,
        assertion_evaluation(3, 0, 4, 1, 0, None, None, &BTreeMap::new()),
    );

    assert_eq!(results[0].name, "maxBackpressureRate");
    assert_eq!(results[0].status, AssertionStatus::Passed);
    assert_eq!(results[0].observed.as_deref(), Some("0.250000"));

    let zero_denominator = evaluate_assertions(
        &manifest,
        assertion_evaluation(0, 0, 0, 0, 0, None, None, &BTreeMap::new()),
    );
    assert_eq!(zero_denominator[0].status, AssertionStatus::Passed);
    assert_eq!(zero_denominator[0].observed.as_deref(), Some("0.000000"));
}

#[test]
fn max_backpressure_rate_fails_above_threshold() {
    let manifest = manifest_with_assertions(&json!({"maxBackpressureRate": 0.249}));
    let results = evaluate_assertions(
        &manifest,
        assertion_evaluation(3, 0, 4, 1, 0, None, None, &BTreeMap::new()),
    );

    assert_eq!(results[0].status, AssertionStatus::Failed);
    assert_eq!(results[0].observed.as_deref(), Some("0.250000"));
}

#[test]
fn max_simulator_overload_rate_passes_and_fails_against_scheduled_ticks() {
    let manifest = manifest_with_assertions(&json!({"maxSimulatorOverloadRate": 0.1}));
    let passed = evaluate_assertions(
        &manifest,
        assertion_evaluation(10, 0, 10, 0, 1, None, None, &BTreeMap::new()),
    );
    assert_eq!(passed[0].status, AssertionStatus::Passed);
    assert_eq!(passed[0].observed.as_deref(), Some("0.100000"));

    let failed = evaluate_assertions(
        &manifest,
        assertion_evaluation(10, 0, 10, 0, 2, None, None, &BTreeMap::new()),
    );
    assert_eq!(failed[0].status, AssertionStatus::Failed);
    assert_eq!(failed[0].observed.as_deref(), Some("0.200000"));
}

#[test]
fn max_rss_growth_is_unsupported_without_two_rss_points() {
    let manifest = manifest_with_assertions(&json!({"maxRssGrowthBytesPerMinute": 100}));
    let timeline = resource_timeline(None, None);
    let results =
        evaluate_test_assertions(&manifest, 1, 0, None, Some(&timeline), &BTreeMap::new());

    assert_eq!(results[0].name, "maxRssGrowthBytesPerMinute");
    assert_eq!(results[0].status, AssertionStatus::Unsupported);
}

#[test]
fn max_rss_growth_uses_worst_restart_segment() {
    let manifest = manifest_with_assertions(&json!({"maxRssGrowthBytesPerMinute": 99}));
    let timeline = resource_timeline(Some(100), Some(-10));

    let results =
        evaluate_test_assertions(&manifest, 1, 0, None, Some(&timeline), &BTreeMap::new());

    assert_eq!(results[0].status, AssertionStatus::Failed);
    assert_eq!(results[0].observed.as_deref(), Some("100"));

    let passing_manifest = manifest_with_assertions(&json!({"maxRssGrowthBytesPerMinute": 100}));
    let passing = evaluate_test_assertions(
        &passing_manifest,
        1,
        0,
        None,
        Some(&timeline),
        &BTreeMap::new(),
    );
    assert_eq!(passing[0].status, AssertionStatus::Passed);
}

fn evaluate_test_assertions(
    manifest: &SimulationManifest,
    completed_workloads: u64,
    failed_workloads: u64,
    chain_progress: Option<&ChainProgressSummary>,
    resource_timeline: Option<&ResourceTimeline>,
    workload_metrics: &BTreeMap<WorkloadKind, WorkloadMetrics>,
) -> Vec<AssertionResult> {
    evaluate_assertions(
        manifest,
        assertion_evaluation(
            completed_workloads,
            failed_workloads,
            completed_workloads + failed_workloads,
            0,
            0,
            chain_progress,
            resource_timeline,
            workload_metrics,
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn assertion_evaluation<'a>(
    completed_workloads: u64,
    failed_workloads: u64,
    scheduled_actor_workloads: u64,
    backpressure_workloads: u64,
    simulator_overload_count: u64,
    chain_progress: Option<&'a ChainProgressSummary>,
    resource_timeline: Option<&'a ResourceTimeline>,
    workload_metrics: &'a BTreeMap<WorkloadKind, WorkloadMetrics>,
) -> AssertionEvaluation<'a> {
    AssertionEvaluation {
        completed_workloads,
        failed_workloads,
        scheduled_actor_workloads,
        backpressure_workloads,
        simulator_overload_count,
        chain_progress,
        resource_timeline,
        workload_metrics,
    }
}

fn resource_timeline(
    max_segment_growth: Option<i64>,
    last_segment_growth: Option<i64>,
) -> ResourceTimeline {
    let first = resource_sample(0, 1_000);
    let last = resource_sample(1_000, 1_100);
    ResourceTimeline {
        scope: ResourceSampleScope::ProcessGroup,
        sample_interval_ms: 1_000,
        sample_count: 2,
        first,
        last,
        max_rss_bytes: Some(1_100),
        max_segment_rss_growth_bytes_per_minute: max_segment_growth,
        last_segment_rss_growth_bytes_per_minute: last_segment_growth,
        peak_threads: Some(1),
        peak_open_fds: Some(1),
        total_cpu_ms: Some(1),
        segments: vec![ResourceSegment {
            scope: ResourceSampleScope::ProcessGroup,
            root_pid: 42,
            pgid: Some(42),
            sample_count: 2,
            started_at_unix_ms: "0".into(),
            ended_at_unix_ms: "60000".into(),
            first_rss_bytes: Some(1_000),
            last_rss_bytes: Some(1_100),
            max_rss_bytes: Some(1_100),
            rss_growth_bytes_per_minute: max_segment_growth,
            total_cpu_ms: Some(1),
        }],
        samples: Vec::new(),
    }
}

fn resource_sample(simulation_time_ms: u64, rss_bytes: u64) -> ResourceSamplePoint {
    ResourceSamplePoint {
        simulation_time_ms,
        observed_at_unix_ms: simulation_time_ms.to_string(),
        scope: ResourceSampleScope::ProcessGroup,
        root_pid: 42,
        pgid: Some(42),
        process_count: 1,
        rss_bytes: Some(rss_bytes),
        cpu_user_ms: Some(1),
        cpu_system_ms: Some(0),
        threads: Some(1),
        open_fds: Some(1),
    }
}

fn manifest_with_assertions(assertions: &serde_json::Value) -> SimulationManifest {
    serde_json::from_value(json!({
        "schemaVersion": 1,
        "kind": "groundhog-simulation",
        "name": "assertion-test",
        "seed": "seed",
        "target": {"provider": "existing"},
        "loadShape": [{"duration": "1s", "users": 1, "ratePerUserHz": 1.0}],
        "workloads": [{"name": "chain-tip", "weight": 1}],
        "assertions": assertions
    }))
    .expect("manifest fixture should parse")
}
