use std::collections::BTreeMap;

use hdrhistogram::Histogram;

use crate::manifest::WorkloadKind;
use crate::result::{
    ChainProgressSummary, ChainTipObservation, LatencySummary, ReceiptPollSummary, WorkloadCounts,
    WorkloadMetrics,
};

const MAX_CHAIN_PROGRESS_OBSERVATIONS: usize = 1_024;

#[derive(Debug, Default)]
pub struct RunMetricsBuilder {
    workloads: BTreeMap<WorkloadKind, WorkloadMetricsBuilder>,
    chain_progress: ChainProgressBuilder,
}

impl RunMetricsBuilder {
    pub fn record_success(&mut self, workload: WorkloadKind, latency_ms: u64) {
        self.workload_mut(workload).record_success(latency_ms);
    }

    pub fn record_failure(
        &mut self,
        workload: WorkloadKind,
        error_class: impl Into<String>,
        latency_ms: u64,
    ) {
        self.workload_mut(workload)
            .record_failure(error_class.into(), latency_ms);
    }

    pub fn record_skipped(&mut self, workload: WorkloadKind, error_class: impl Into<String>) {
        self.workload_mut(workload)
            .record_skipped(error_class.into());
    }

    pub fn record_chain_tip(&mut self, observation: ChainTipObservation) {
        self.chain_progress.record(observation);
    }

    pub fn record_submit_to_receipt_latency(&mut self, workload: WorkloadKind, latency_ms: u64) {
        self.workload_mut(workload)
            .record_submit_to_receipt_latency(latency_ms);
    }

    pub fn record_receipt_poll(&mut self, present: bool) {
        self.workload_mut(WorkloadKind::ReceiptPoll)
            .record_receipt_poll(present);
    }

    pub fn record_pending_receipts_at_shutdown(&mut self, count: u64) {
        self.workload_mut(WorkloadKind::ReceiptPoll)
            .record_pending_receipts_at_shutdown(count);
    }

    pub fn workload_counts(&self) -> BTreeMap<WorkloadKind, WorkloadCounts> {
        self.workloads
            .iter()
            .map(|(workload, metrics)| (*workload, metrics.counts.clone()))
            .collect()
    }

    pub fn completed_count(&self) -> u64 {
        self.workloads
            .values()
            .map(|metrics| metrics.counts.completed)
            .sum()
    }

    pub fn failed_count(&self) -> u64 {
        self.workloads
            .values()
            .map(|metrics| metrics.counts.failed)
            .sum()
    }

    pub fn skipped_count(&self) -> u64 {
        self.workloads
            .values()
            .map(|metrics| metrics.counts.skipped)
            .sum()
    }

    pub fn finish(self) -> RunMetrics {
        let workload_metrics = self
            .workloads
            .into_iter()
            .map(|(workload, metrics)| (workload, metrics.finish()))
            .collect();
        let chain_progress = self.chain_progress.finish();

        RunMetrics {
            workload_metrics,
            chain_progress,
        }
    }

    fn workload_mut(&mut self, workload: WorkloadKind) -> &mut WorkloadMetricsBuilder {
        self.workloads.entry(workload).or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMetrics {
    pub workload_metrics: BTreeMap<WorkloadKind, WorkloadMetrics>,
    pub chain_progress: Option<ChainProgressSummary>,
}

#[derive(Debug)]
struct WorkloadMetricsBuilder {
    counts: WorkloadCounts,
    error_counts: BTreeMap<String, u64>,
    latency_histogram_ms: Histogram<u64>,
    success_latency_histogram_ms: Histogram<u64>,
    submit_to_receipt_latency_histogram_ms: Histogram<u64>,
    receipt_poll: ReceiptPollSummary,
}

impl WorkloadMetricsBuilder {
    fn record_success(&mut self, latency_ms: u64) {
        self.counts.completed += 1;
        self.latency_histogram_ms
            .record(latency_ms.max(1))
            .expect("bug: latency histogram should accept u64 millisecond samples");
        self.success_latency_histogram_ms
            .record(latency_ms.max(1))
            .expect("bug: success latency histogram should accept u64 millisecond samples");
    }

    fn record_failure(&mut self, error_class: String, latency_ms: u64) {
        self.counts.failed += 1;
        self.latency_histogram_ms
            .record(latency_ms.max(1))
            .expect("bug: latency histogram should accept u64 millisecond samples");
        *self.error_counts.entry(error_class).or_default() += 1;
    }

    fn record_skipped(&mut self, error_class: String) {
        self.counts.skipped += 1;
        *self.error_counts.entry(error_class).or_default() += 1;
    }

    fn record_submit_to_receipt_latency(&mut self, latency_ms: u64) {
        self.submit_to_receipt_latency_histogram_ms
            .record(latency_ms.max(1))
            .expect("bug: receipt latency histogram should accept u64 millisecond samples");
    }

    fn record_receipt_poll(&mut self, present: bool) {
        if present {
            self.receipt_poll.present += 1;
        } else {
            self.receipt_poll.absent += 1;
        }
    }

    fn record_pending_receipts_at_shutdown(&mut self, count: u64) {
        self.receipt_poll.pending_at_shutdown += count;
    }

    fn finish(self) -> WorkloadMetrics {
        let receipt_poll = if self.receipt_poll.present == 0
            && self.receipt_poll.absent == 0
            && self.receipt_poll.pending_at_shutdown == 0
        {
            None
        } else {
            Some(self.receipt_poll)
        };
        WorkloadMetrics {
            counts: self.counts,
            error_counts: self.error_counts,
            latency_ms: LatencySummary::from_histogram(&self.latency_histogram_ms),
            success_latency_ms: LatencySummary::from_histogram(&self.success_latency_histogram_ms),
            submit_to_receipt_latency_ms: LatencySummary::from_histogram(
                &self.submit_to_receipt_latency_histogram_ms,
            ),
            receipt_poll,
        }
    }
}

impl Default for WorkloadMetricsBuilder {
    fn default() -> Self {
        Self {
            counts: WorkloadCounts::default(),
            error_counts: BTreeMap::new(),
            latency_histogram_ms: Histogram::new(3)
                .expect("bug: latency histogram precision should be valid"),
            success_latency_histogram_ms: Histogram::new(3)
                .expect("bug: success latency histogram precision should be valid"),
            submit_to_receipt_latency_histogram_ms: Histogram::new(3)
                .expect("bug: receipt latency histogram precision should be valid"),
            receipt_poll: ReceiptPollSummary::default(),
        }
    }
}

#[derive(Debug, Default)]
struct ChainProgressBuilder {
    first: Option<ChainTipObservation>,
    last: Option<ChainTipObservation>,
    max_height: Option<i64>,
    sampled: Vec<ChainTipObservation>,
    all_observations: Vec<ChainTipObservation>,
    total_observations: u64,
}

impl ChainProgressBuilder {
    fn record(&mut self, observation: ChainTipObservation) {
        self.total_observations += 1;
        if self.first.is_none() {
            self.first = Some(observation.clone());
        }
        self.max_height = Some(
            self.max_height
                .map_or(observation.height, |height| height.max(observation.height)),
        );
        self.last = Some(observation.clone());

        if self.sampled.len() < MAX_CHAIN_PROGRESS_OBSERVATIONS {
            self.sampled.push(observation.clone());
        }
        self.all_observations.push(observation);
    }

    fn finish(self) -> Option<ChainProgressSummary> {
        let first = self.first?;
        let last = self
            .last
            .expect("bug: first observation existed, so last must exist");
        let max_height = self
            .max_height
            .expect("bug: first observation existed, so max height must exist");

        Some(ChainProgressSummary {
            first_height: first.height,
            last_height: last.height,
            max_height,
            total_observations: self.total_observations,
            max_stall_ms: max_chain_stall_ms(self.all_observations),
            observations: self.sampled,
        })
    }
}

fn max_chain_stall_ms(mut observations: Vec<ChainTipObservation>) -> Option<u64> {
    if observations.is_empty() {
        return None;
    }
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

    #[test]
    fn aggregates_workload_counts_errors_and_latency() {
        let mut metrics = RunMetricsBuilder::default();
        metrics.record_success(WorkloadKind::ChainTip, 10);
        metrics.record_success(WorkloadKind::ChainTip, 20);
        metrics.record_failure(WorkloadKind::ChainTip, "rpc-error", 100);

        let finished = metrics.finish();
        let chain_tip = finished
            .workload_metrics
            .get(&WorkloadKind::ChainTip)
            .expect("chain-tip metrics should exist");

        assert_eq!(chain_tip.counts.completed, 2);
        assert_eq!(chain_tip.counts.failed, 1);
        assert_eq!(chain_tip.error_counts["rpc-error"], 1);
        assert_eq!(
            chain_tip.latency_ms,
            Some(LatencySummary {
                sample_count: 3,
                p50_ms: 20,
                p95_ms: 100,
                p99_ms: 100,
                p999_ms: 100,
                max_ms: 100,
            })
        );
        assert_eq!(
            chain_tip.success_latency_ms,
            Some(LatencySummary {
                sample_count: 2,
                p50_ms: 10,
                p95_ms: 20,
                p99_ms: 20,
                p999_ms: 20,
                max_ms: 20,
            })
        );
    }

    #[test]
    fn aggregates_receipt_poll_presence_and_submit_to_receipt_latency() {
        let mut metrics = RunMetricsBuilder::default();
        metrics.record_success(WorkloadKind::ReceiptPoll, 2);
        metrics.record_receipt_poll(true);
        metrics.record_submit_to_receipt_latency(WorkloadKind::ReceiptPoll, 12);
        metrics.record_success(WorkloadKind::ReceiptPoll, 3);
        metrics.record_receipt_poll(false);
        metrics.record_pending_receipts_at_shutdown(1);

        let finished = metrics.finish();
        let receipt_poll = finished
            .workload_metrics
            .get(&WorkloadKind::ReceiptPoll)
            .expect("receipt-poll metrics should exist");

        assert_eq!(
            receipt_poll
                .receipt_poll
                .as_ref()
                .expect("receipt-poll summary should exist"),
            &ReceiptPollSummary {
                present: 1,
                absent: 1,
                pending_at_shutdown: 1,
            }
        );
        assert_eq!(
            receipt_poll.submit_to_receipt_latency_ms,
            Some(LatencySummary {
                sample_count: 1,
                p50_ms: 12,
                p95_ms: 12,
                p99_ms: 12,
                p999_ms: 12,
                max_ms: 12,
            })
        );
    }

    #[test]
    fn summarizes_chain_progress() {
        let mut metrics = RunMetricsBuilder::default();
        metrics.record_chain_tip(ChainTipObservation {
            simulation_time_ms: 0,
            height: 7,
            block_hash: "0x07".into(),
        });
        metrics.record_chain_tip(ChainTipObservation {
            simulation_time_ms: 1_000,
            height: 9,
            block_hash: "0x09".into(),
        });

        let progress = metrics
            .finish()
            .chain_progress
            .expect("progress should exist");
        assert_eq!(progress.first_height, 7);
        assert_eq!(progress.last_height, 9);
        assert_eq!(progress.max_height, 9);
        assert_eq!(progress.total_observations, 2);
        assert_eq!(progress.max_stall_ms, Some(1_000));
        assert_eq!(progress.observations.len(), 2);
    }

    #[test]
    fn chain_progress_sample_cap_drops_new_samples_without_rewriting_history() {
        let mut metrics = RunMetricsBuilder::default();
        for height in 0..=MAX_CHAIN_PROGRESS_OBSERVATIONS {
            let height = i64::try_from(height).expect("test height should fit i64");
            metrics.record_chain_tip(ChainTipObservation {
                simulation_time_ms: u64::try_from(height).expect("test height should fit u64"),
                height,
                block_hash: format!("0x{height:04x}"),
            });
        }

        let progress = metrics
            .finish()
            .chain_progress
            .expect("progress should exist");
        assert_eq!(progress.total_observations, 1_025);
        assert_eq!(progress.last_height, 1_024);
        assert_eq!(progress.max_stall_ms, Some(1));
        assert_eq!(progress.observations.len(), MAX_CHAIN_PROGRESS_OBSERVATIONS);
        assert_eq!(
            progress
                .observations
                .last()
                .expect("sample should contain entries")
                .height,
            1_023
        );
    }

    #[test]
    fn chain_progress_max_stall_includes_observations_after_sample_cap() {
        let mut metrics = RunMetricsBuilder::default();
        for index in 0..MAX_CHAIN_PROGRESS_OBSERVATIONS {
            let height = i64::try_from(index).expect("test height should fit i64");
            metrics.record_chain_tip(ChainTipObservation {
                simulation_time_ms: u64::try_from(index).expect("test time should fit u64"),
                height,
                block_hash: format!("0x{height:04x}"),
            });
        }
        let capped_height =
            i64::try_from(MAX_CHAIN_PROGRESS_OBSERVATIONS - 1).expect("test height should fit i64");
        metrics.record_chain_tip(ChainTipObservation {
            simulation_time_ms: u64::try_from(MAX_CHAIN_PROGRESS_OBSERVATIONS + 10)
                .expect("test time should fit u64"),
            height: capped_height,
            block_hash: format!("0x{capped_height:04x}"),
        });

        let progress = metrics
            .finish()
            .chain_progress
            .expect("progress should exist");
        assert_eq!(progress.observations.len(), MAX_CHAIN_PROGRESS_OBSERVATIONS);
        assert_eq!(progress.max_stall_ms, Some(11));
    }
}
