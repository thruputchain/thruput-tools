use std::collections::BTreeMap;

use hdrhistogram::Histogram;

use crate::manifest::WorkloadKind;
use crate::result::{
    ChainProgressSummary, ChainTipObservation, LatencySummary, WorkloadCounts, WorkloadMetrics,
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
}

impl WorkloadMetricsBuilder {
    fn record_success(&mut self, latency_ms: u64) {
        self.counts.completed += 1;
        self.latency_histogram_ms
            .record(latency_ms.max(1))
            .expect("bug: latency histogram should accept u64 millisecond samples");
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

    fn finish(self) -> WorkloadMetrics {
        WorkloadMetrics {
            counts: self.counts,
            error_counts: self.error_counts,
            latency_ms: LatencySummary::from_histogram(&self.latency_histogram_ms),
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
        }
    }
}

#[derive(Debug, Default)]
struct ChainProgressBuilder {
    first: Option<ChainTipObservation>,
    last: Option<ChainTipObservation>,
    max_height: Option<i64>,
    sampled: Vec<ChainTipObservation>,
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
            self.sampled.push(observation);
        } else if let Some(last) = self.sampled.last_mut() {
            *last = observation;
        }
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
            observations: self.sampled,
        })
    }
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
                p99_9_ms: 100,
                max_ms: 100,
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
        assert_eq!(progress.observations.len(), 2);
    }
}
