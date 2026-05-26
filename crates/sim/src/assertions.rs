use std::collections::BTreeMap;

use crate::manifest::{SimulationManifest, WorkloadKind};
use crate::result::{
    AssertionResult, AssertionStatus, ChainProgressSummary, SimRunOutcome, WorkloadMetrics,
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
    completed_workloads: u64,
    failed_workloads: u64,
    chain_progress: Option<&ChainProgressSummary>,
    workload_metrics: &BTreeMap<WorkloadKind, WorkloadMetrics>,
) -> Vec<AssertionResult> {
    let mut results = Vec::new();

    if let Some(max_error_rate) = manifest.assertions.max_error_rate {
        results.push(evaluate_max_error_rate(
            max_error_rate,
            completed_workloads,
            failed_workloads,
        ));
    }

    if let Some(max_chain_stall) = &manifest.assertions.max_chain_stall {
        results.push(evaluate_max_chain_stall(max_chain_stall, chain_progress));
    }

    if let Some(threshold) = manifest.assertions.p99_submit_latency_ms {
        results.push(evaluate_p99_submit_latency(threshold, workload_metrics));
    }

    if let Some(max_timeout_rate) = manifest.assertions.max_receipt_timeout_rate {
        results.push(evaluate_receipt_timeout_rate(
            max_timeout_rate,
            workload_metrics,
        ));
    }

    results
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
        Some(receipts) if receipts.present + receipts.pending_at_shutdown > 0 => {
            let total = receipts.present + receipts.pending_at_shutdown;
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
    use crate::result::ChainTipObservation;

    #[test]
    fn evaluates_max_error_rate_assertion() {
        let manifest = manifest_with_assertions(&json!({"maxErrorRate": 0.25}));
        let results = evaluate_assertions(&manifest, 3, 1, None, &BTreeMap::new());

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
        let results = evaluate_assertions(&manifest, 2, 0, Some(&chain_progress), &BTreeMap::new());

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

        let results = evaluate_assertions(&manifest, 2, 0, Some(&chain_progress), &BTreeMap::new());

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

        let results = evaluate_assertions(&manifest, 2, 0, None, &finished.workload_metrics);

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

        let results = evaluate_assertions(&manifest, 1, 1, None, &finished.workload_metrics);

        assert_eq!(results[0].status, AssertionStatus::Passed);
        assert_eq!(results[0].observed.as_deref(), Some("10ms"));
    }

    #[test]
    fn fails_p99_submit_latency_assertion() {
        let manifest = manifest_with_assertions(&json!({"p99SubmitLatencyMs": 5}));
        let mut metrics = RunMetricsBuilder::default();
        metrics.record_success(WorkloadKind::WalletSubmitSmoke, 10);
        let finished = metrics.finish();

        let results = evaluate_assertions(&manifest, 1, 0, None, &finished.workload_metrics);

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

        let results = evaluate_assertions(&manifest, 2, 0, None, &finished.workload_metrics);

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

        let results = evaluate_assertions(&manifest, 1, 0, None, &finished.workload_metrics);

        assert_eq!(results[0].status, AssertionStatus::Failed);
        assert_eq!(results[0].observed.as_deref(), Some("1.000000"));
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
