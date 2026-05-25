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
#[allow(clippy::cast_precision_loss)]
pub fn evaluate_assertions(
    manifest: &SimulationManifest,
    completed_workloads: u64,
    failed_workloads: u64,
    chain_progress: Option<&ChainProgressSummary>,
    workload_metrics: &BTreeMap<WorkloadKind, WorkloadMetrics>,
) -> Vec<AssertionResult> {
    let mut results = Vec::new();

    if let Some(max_error_rate) = manifest.assertions.max_error_rate {
        let total = completed_workloads + failed_workloads;
        let observed = if total == 0 {
            0.0
        } else {
            failed_workloads as f64 / total as f64
        };
        results.push(AssertionResult {
            name: "maxErrorRate".into(),
            status: if observed <= max_error_rate {
                AssertionStatus::Passed
            } else {
                AssertionStatus::Failed
            },
            observed: Some(format!("{observed:.6}")),
            threshold: Some(format!("{max_error_rate:.6}")),
            reason: None,
        });
    }

    if let Some(max_chain_stall) = &manifest.assertions.max_chain_stall {
        match (
            max_chain_stall.parse_millis(),
            max_chain_stall_ms(chain_progress),
        ) {
            (Ok(threshold_ms), Some(observed_ms)) => {
                results.push(AssertionResult {
                    name: "maxChainStall".into(),
                    status: if observed_ms <= threshold_ms {
                        AssertionStatus::Passed
                    } else {
                        AssertionStatus::Failed
                    },
                    observed: Some(format!("{observed_ms}ms")),
                    threshold: Some(format!("{threshold_ms}ms")),
                    reason: None,
                });
            }
            (Ok(threshold_ms), None) => {
                results.push(AssertionResult {
                    name: "maxChainStall".into(),
                    status: AssertionStatus::Unsupported,
                    observed: None,
                    threshold: Some(format!("{threshold_ms}ms")),
                    reason: Some("no chain-tip observations were recorded".into()),
                });
            }
            (Err(error), _) => {
                results.push(AssertionResult {
                    name: "maxChainStall".into(),
                    status: AssertionStatus::Failed,
                    observed: None,
                    threshold: None,
                    reason: Some(error.to_string()),
                });
            }
        }
    }

    if let Some(threshold) = manifest.assertions.p99_submit_latency_ms {
        results.push(AssertionResult {
            name: "p99SubmitLatencyMs".into(),
            status: AssertionStatus::Unsupported,
            observed: workload_metrics
                .get(&WorkloadKind::WalletSubmitSmoke)
                .and_then(|metrics| metrics.latency_ms.as_ref())
                .map(|latency| format!("{}ms", latency.p99_ms)),
            threshold: Some(format!("{threshold}ms")),
            reason: Some("wallet-submit-smoke execution is not implemented yet".into()),
        });
    }

    if let Some(max_timeout_rate) = manifest.assertions.max_receipt_timeout_rate {
        results.push(AssertionResult {
            name: "maxReceiptTimeoutRate".into(),
            status: AssertionStatus::Unsupported,
            observed: None,
            threshold: Some(format!("{max_timeout_rate:.6}")),
            reason: Some("receipt-poll execution is not implemented yet".into()),
        });
    }

    results
}

fn max_chain_stall_ms(chain_progress: Option<&ChainProgressSummary>) -> Option<u64> {
    let observations = &chain_progress?.observations;
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
