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
    use super::*;
    use crate::result::ChainTipObservation;

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
}
