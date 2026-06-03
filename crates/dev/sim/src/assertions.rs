use std::collections::BTreeMap;

use crate::manifest::{SimulationManifest, WorkloadKind};
use crate::result::{
    AssertionResult, AssertionStatus, ChainProgressSummary, ResourceTimeline, SimRunOutcome,
    WorkloadMetrics,
};

/// Determine the final run outcome from workload and assertion results.
pub fn determine_outcome(
    assertion_results: &[AssertionResult],
    failed_workloads: u64,
) -> SimRunOutcome {
    if failed_workloads > 0
        || assertion_results
            .iter()
            .any(|assertion| assertion.status == AssertionStatus::Failed)
    {
        SimRunOutcome::Failed
    } else {
        SimRunOutcome::Passed
    }
}

/// Evaluate all supported Phase 1 assertions.
pub fn evaluate_assertions(
    manifest: &SimulationManifest,
    input: AssertionEvaluation<'_>,
) -> Vec<AssertionResult> {
    let mut results = Vec::new();

    if let Some(max_error_rate) = manifest.assertions.max_error_rate {
        results.push(evaluate_max_error_rate(
            max_error_rate,
            input.completed_workloads,
            input.failed_workloads,
        ));
    }

    if let Some(max_chain_stall) = &manifest.assertions.max_chain_stall {
        results.push(evaluate_max_chain_stall(
            max_chain_stall,
            input.chain_progress,
        ));
    }

    if let Some(max_backpressure_rate) = manifest.assertions.max_backpressure_rate {
        results.push(evaluate_max_backpressure_rate(
            max_backpressure_rate,
            input.scheduled_actor_workloads,
            input.backpressure_workloads,
        ));
    }

    if let Some(max_simulator_overload_rate) = manifest.assertions.max_simulator_overload_rate {
        results.push(evaluate_max_simulator_overload_rate(
            max_simulator_overload_rate,
            input.scheduled_actor_workloads,
            input.simulator_overload_count,
        ));
    }

    if let Some(threshold) = manifest.assertions.max_rss_growth_bytes_per_minute {
        results.push(evaluate_max_rss_growth_bytes_per_minute(
            threshold,
            input.resource_timeline,
        ));
    }

    if let Some(threshold) = manifest.assertions.p99_submit_latency_ms {
        results.push(evaluate_p99_submit_latency(
            threshold,
            input.workload_metrics,
        ));
    }

    if let Some(max_timeout_rate) = manifest.assertions.max_receipt_timeout_rate {
        results.push(evaluate_receipt_timeout_rate(
            max_timeout_rate,
            input.workload_metrics,
        ));
    }

    results
}

#[derive(Clone, Copy)]
pub struct AssertionEvaluation<'a> {
    pub completed_workloads: u64,
    pub failed_workloads: u64,
    pub scheduled_actor_workloads: u64,
    pub backpressure_workloads: u64,
    pub simulator_overload_count: u64,
    pub chain_progress: Option<&'a ChainProgressSummary>,
    pub resource_timeline: Option<&'a ResourceTimeline>,
    pub workload_metrics: &'a BTreeMap<WorkloadKind, WorkloadMetrics>,
}

#[allow(clippy::cast_precision_loss)]
pub fn workload_event_rate(count: u64, scheduled_actor_workloads: u64) -> f64 {
    if scheduled_actor_workloads == 0 {
        0.0
    } else {
        count as f64 / scheduled_actor_workloads as f64
    }
}

#[allow(clippy::cast_precision_loss)]
fn evaluate_max_error_rate(
    max_error_rate: f64,
    completed_workloads: u64,
    failed_workloads: u64,
) -> AssertionResult {
    let total = completed_workloads + failed_workloads;
    let observed = if total == 0 {
        0.0
    } else {
        failed_workloads as f64 / total as f64
    };
    AssertionResult {
        name: "maxErrorRate".into(),
        status: if observed <= max_error_rate {
            AssertionStatus::Passed
        } else {
            AssertionStatus::Failed
        },
        observed: Some(format!("{observed:.6}")),
        threshold: Some(format!("{max_error_rate:.6}")),
        reason: None,
    }
}

fn evaluate_max_chain_stall(
    max_chain_stall: &crate::manifest::DurationSpec,
    chain_progress: Option<&ChainProgressSummary>,
) -> AssertionResult {
    match (
        max_chain_stall.parse_millis(),
        max_chain_stall_ms(chain_progress),
    ) {
        (Ok(threshold_ms), Some(observed_ms)) => AssertionResult {
            name: "maxChainStall".into(),
            status: if observed_ms <= threshold_ms {
                AssertionStatus::Passed
            } else {
                AssertionStatus::Failed
            },
            observed: Some(format!("{observed_ms}ms")),
            threshold: Some(format!("{threshold_ms}ms")),
            reason: None,
        },
        (Ok(threshold_ms), None) => AssertionResult {
            name: "maxChainStall".into(),
            status: AssertionStatus::Unsupported,
            observed: None,
            threshold: Some(format!("{threshold_ms}ms")),
            reason: Some("no chain-tip observations were recorded".into()),
        },
        (Err(error), _) => AssertionResult {
            name: "maxChainStall".into(),
            status: AssertionStatus::Failed,
            observed: None,
            threshold: None,
            reason: Some(error.to_string()),
        },
    }
}

fn evaluate_p99_submit_latency(
    threshold: u64,
    workload_metrics: &BTreeMap<WorkloadKind, WorkloadMetrics>,
) -> AssertionResult {
    match workload_metrics
        .get(&WorkloadKind::WalletSubmitSmoke)
        .and_then(|metrics| metrics.success_latency_ms.as_ref())
    {
        Some(latency) => AssertionResult {
            name: "p99SubmitLatencyMs".into(),
            status: if latency.p99_ms <= threshold {
                AssertionStatus::Passed
            } else {
                AssertionStatus::Failed
            },
            observed: Some(format!("{}ms", latency.p99_ms)),
            threshold: Some(format!("{threshold}ms")),
            reason: None,
        },
        None => AssertionResult {
            name: "p99SubmitLatencyMs".into(),
            status: AssertionStatus::Unsupported,
            observed: None,
            threshold: Some(format!("{threshold}ms")),
            reason: Some("no successful wallet-submit-smoke latency samples were recorded".into()),
        },
    }
}

#[allow(clippy::cast_precision_loss)]
fn evaluate_receipt_timeout_rate(
    max_timeout_rate: f64,
    workload_metrics: &BTreeMap<WorkloadKind, WorkloadMetrics>,
) -> AssertionResult {
    match workload_metrics
        .get(&WorkloadKind::ReceiptPoll)
        .and_then(|metrics| metrics.receipt_poll.as_ref())
    {
        Some(receipts)
            if receipts
                .present
                .saturating_add(receipts.pending_at_shutdown)
                > 0 =>
        {
            let total = receipts
                .present
                .saturating_add(receipts.pending_at_shutdown);
            let observed = receipts.pending_at_shutdown as f64 / total as f64;
            AssertionResult {
                name: "maxReceiptTimeoutRate".into(),
                status: if observed <= max_timeout_rate {
                    AssertionStatus::Passed
                } else {
                    AssertionStatus::Failed
                },
                observed: Some(format!("{observed:.6}")),
                threshold: Some(format!("{max_timeout_rate:.6}")),
                reason: None,
            }
        }
        _ => AssertionResult {
            name: "maxReceiptTimeoutRate".into(),
            status: AssertionStatus::Unsupported,
            observed: None,
            threshold: Some(format!("{max_timeout_rate:.6}")),
            reason: Some("no submitted transactions were tracked for receipt completion".into()),
        },
    }
}

fn evaluate_max_backpressure_rate(
    max_backpressure_rate: f64,
    scheduled_actor_workloads: u64,
    backpressure_workloads: u64,
) -> AssertionResult {
    let observed = workload_event_rate(backpressure_workloads, scheduled_actor_workloads);
    AssertionResult {
        name: "maxBackpressureRate".into(),
        status: if observed <= max_backpressure_rate {
            AssertionStatus::Passed
        } else {
            AssertionStatus::Failed
        },
        observed: Some(format!("{observed:.6}")),
        threshold: Some(format!("{max_backpressure_rate:.6}")),
        reason: None,
    }
}

fn evaluate_max_simulator_overload_rate(
    max_simulator_overload_rate: f64,
    scheduled_actor_workloads: u64,
    simulator_overload_count: u64,
) -> AssertionResult {
    let observed = workload_event_rate(simulator_overload_count, scheduled_actor_workloads);
    AssertionResult {
        name: "maxSimulatorOverloadRate".into(),
        status: if observed <= max_simulator_overload_rate {
            AssertionStatus::Passed
        } else {
            AssertionStatus::Failed
        },
        observed: Some(format!("{observed:.6}")),
        threshold: Some(format!("{max_simulator_overload_rate:.6}")),
        reason: None,
    }
}

fn evaluate_max_rss_growth_bytes_per_minute(
    threshold: i64,
    resource_timeline: Option<&ResourceTimeline>,
) -> AssertionResult {
    match resource_timeline.and_then(|timeline| timeline.max_segment_rss_growth_bytes_per_minute) {
        Some(observed) => AssertionResult {
            name: "maxRssGrowthBytesPerMinute".into(),
            status: if observed <= threshold {
                AssertionStatus::Passed
            } else {
                AssertionStatus::Failed
            },
            observed: Some(observed.to_string()),
            threshold: Some(threshold.to_string()),
            reason: None,
        },
        None => AssertionResult {
            name: "maxRssGrowthBytesPerMinute".into(),
            status: AssertionStatus::Unsupported,
            observed: None,
            threshold: Some(threshold.to_string()),
            reason: Some(
                "no restart segment has at least two RSS observations for growth calculation"
                    .into(),
            ),
        },
    }
}

fn max_chain_stall_ms(chain_progress: Option<&ChainProgressSummary>) -> Option<u64> {
    let chain_progress = chain_progress?;
    if let Some(max_stall_ms) = chain_progress.max_stall_ms {
        return Some(max_stall_ms);
    }
    let observations = &chain_progress.observations;
    if observations.is_empty() {
        return None;
    }
    let mut observations: Vec<_> = observations.iter().collect();
    observations.sort_by_key(|observation| observation.simulation_time_ms);

    let first = observations
        .first()
        .expect("bug: observations was checked non-empty");
    let mut last_progress_time = first.simulation_time_ms;
    let mut last_height = first.height;
    let mut max_stall = 0_u64;

    for observation in observations.iter().skip(1) {
        if observation.height > last_height {
            max_stall = max_stall.max(observation.simulation_time_ms - last_progress_time);
            last_progress_time = observation.simulation_time_ms;
            last_height = observation.height;
        }
    }
    let last_time = observations
        .last()
        .expect("bug: observations was checked non-empty")
        .simulation_time_ms;
    Some(max_stall.max(last_time - last_progress_time))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::metrics::RunMetricsBuilder;
    use crate::result::{
        ChainTipObservation, ResourceSamplePoint, ResourceSampleScope, ResourceSegment,
    };

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
    fn max_chain_stall_handles_out_of_order_observations() {
        let chain_progress = ChainProgressSummary {
            first_height: 1,
            last_height: 1,
            max_height: 2,
            total_observations: 3,
            max_stall_ms: None,
            observations: vec![
                ChainTipObservation {
                    simulation_time_ms: 2_000,
                    height: 2,
                    block_hash: "0x02".into(),
                },
                ChainTipObservation {
                    simulation_time_ms: 0,
                    height: 1,
                    block_hash: "0x01".into(),
                },
                ChainTipObservation {
                    simulation_time_ms: 1_000,
                    height: 1,
                    block_hash: "0x01".into(),
                },
            ],
        };

        assert_eq!(max_chain_stall_ms(Some(&chain_progress)), Some(2_000));
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

        let results =
            evaluate_test_assertions(&manifest, 2, 0, None, None, &finished.workload_metrics);

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

        let results =
            evaluate_test_assertions(&manifest, 1, 1, None, None, &finished.workload_metrics);

        assert_eq!(results[0].status, AssertionStatus::Passed);
        assert_eq!(results[0].observed.as_deref(), Some("10ms"));
    }

    #[test]
    fn fails_p99_submit_latency_assertion() {
        let manifest = manifest_with_assertions(&json!({"p99SubmitLatencyMs": 5}));
        let mut metrics = RunMetricsBuilder::default();
        metrics.record_success(WorkloadKind::WalletSubmitSmoke, 10);
        let finished = metrics.finish();

        let results =
            evaluate_test_assertions(&manifest, 1, 0, None, None, &finished.workload_metrics);

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

        let results =
            evaluate_test_assertions(&manifest, 2, 0, None, None, &finished.workload_metrics);

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

        let results =
            evaluate_test_assertions(&manifest, 1, 0, None, None, &finished.workload_metrics);

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

        let passing_manifest =
            manifest_with_assertions(&json!({"maxRssGrowthBytesPerMinute": 100}));
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
}
