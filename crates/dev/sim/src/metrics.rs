use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

use hdrhistogram::Histogram;

use crate::manifest::WorkloadKind;
use crate::result::{
    ChainProgressSummary, ChainTipObservation, LatencySummary, ReceiptPollSummary,
    ResourceSamplePoint, ResourceSampleScope, ResourceSegment, ResourceTimeline, WorkloadCounts,
    WorkloadMetrics,
};

const MAX_CHAIN_PROGRESS_OBSERVATIONS: usize = 1_024;
const MAX_RESOURCE_SAMPLES: usize = 1_024;

#[derive(Debug, Default)]
pub struct RunMetricsBuilder {
    workloads: BTreeMap<WorkloadKind, WorkloadMetricsBuilder>,
    chain_progress: ChainProgressBuilder,
    resource_timeline: ResourceTimelineBuilder,
    simulator_overload_count: u64,
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

    pub fn record_chain_tip(&mut self, observation: &ChainTipObservation) {
        self.chain_progress.record(observation);
    }

    pub fn set_resource_sample_interval_ms(&mut self, interval_ms: u64) {
        self.resource_timeline.set_sample_interval_ms(interval_ms);
    }

    pub fn record_resource_sample(&mut self, sample: &ResourceSamplePoint) {
        self.resource_timeline.record(sample);
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

    pub fn record_simulator_overload(&mut self) {
        self.simulator_overload_count += 1;
    }

    pub fn simulator_overload_count(&self) -> u64 {
        self.simulator_overload_count
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
        let resource_timeline = self.resource_timeline.finish();

        RunMetrics {
            workload_metrics,
            chain_progress,
            resource_timeline,
            simulator_overload_count: self.simulator_overload_count,
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
    pub resource_timeline: Option<ResourceTimeline>,
    pub simulator_overload_count: u64,
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
    progress_points: BTreeMap<u64, i64>,
    last_observation_time_ms: Option<u64>,
    total_observations: u64,
}

impl ChainProgressBuilder {
    fn record(&mut self, observation: &ChainTipObservation) {
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
        self.record_for_stall(observation);
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
            max_stall_ms: max_chain_stall_ms(self.progress_points, self.last_observation_time_ms),
            observations: self.sampled,
        })
    }

    fn record_for_stall(&mut self, observation: &ChainTipObservation) {
        self.last_observation_time_ms = Some(
            self.last_observation_time_ms
                .map_or(observation.simulation_time_ms, |time| {
                    time.max(observation.simulation_time_ms)
                }),
        );

        let previous_height = self
            .progress_points
            .range(..observation.simulation_time_ms)
            .next_back()
            .map(|(_, height)| *height);
        if previous_height.is_some_and(|height| observation.height <= height) {
            return;
        }
        if self
            .progress_points
            .get(&observation.simulation_time_ms)
            .is_some_and(|height| observation.height <= *height)
        {
            return;
        }

        self.progress_points
            .insert(observation.simulation_time_ms, observation.height);

        let dominated: Vec<_> = self
            .progress_points
            .range((Excluded(observation.simulation_time_ms), Unbounded))
            .take_while(|(_, height)| **height <= observation.height)
            .map(|(time, _)| *time)
            .collect();
        for time in dominated {
            self.progress_points.remove(&time);
        }
    }
}

fn max_chain_stall_ms(
    progress_points: BTreeMap<u64, i64>,
    last_observation_time_ms: Option<u64>,
) -> Option<u64> {
    let mut progress = progress_points.into_iter();
    let (mut last_progress_time, _first_height) = progress.next()?;
    let mut max_stall = 0_u64;

    for (time, _height) in progress {
        max_stall = max_stall.max(time - last_progress_time);
        last_progress_time = time;
    }
    let last_time = last_observation_time_ms
        .expect("bug: progress point existed, so last observation must exist");
    Some(max_stall.max(last_time - last_progress_time))
}

#[derive(Debug)]
struct ResourceTimelineBuilder {
    sample_interval_ms: u64,
    sample_count: u64,
    first: Option<ResourceSamplePoint>,
    last: Option<ResourceSamplePoint>,
    samples: Vec<ResourceSamplePoint>,
    max_rss_bytes: Option<u64>,
    peak_threads: Option<u64>,
    peak_open_fds: Option<u64>,
    segments: Vec<ResourceSegment>,
    current_segment: Option<ResourceSegmentBuilder>,
}

impl Default for ResourceTimelineBuilder {
    fn default() -> Self {
        Self {
            sample_interval_ms: crate::scheduler::DEFAULT_RESOURCE_SAMPLE_INTERVAL_MS,
            sample_count: 0,
            first: None,
            last: None,
            samples: Vec::new(),
            max_rss_bytes: None,
            peak_threads: None,
            peak_open_fds: None,
            segments: Vec::new(),
            current_segment: None,
        }
    }
}

impl ResourceTimelineBuilder {
    fn set_sample_interval_ms(&mut self, interval_ms: u64) {
        self.sample_interval_ms = interval_ms;
    }

    fn record(&mut self, sample: &ResourceSamplePoint) {
        self.sample_count += 1;
        if self.first.is_none() {
            self.first = Some(sample.clone());
        }
        self.last = Some(sample.clone());
        if self.samples.len() < MAX_RESOURCE_SAMPLES {
            self.samples.push(sample.clone());
        }
        self.max_rss_bytes = max_option(self.max_rss_bytes, sample.rss_bytes);
        self.peak_threads = max_option(self.peak_threads, sample.threads);
        self.peak_open_fds = max_option(self.peak_open_fds, sample.open_fds);

        let segment_key = ResourceSegmentKey::from_sample(sample);
        if self
            .current_segment
            .as_ref()
            .is_some_and(|segment| segment.key != segment_key)
            && let Some(segment) = self.current_segment.take()
        {
            self.segments.push(segment.finish());
        }
        self.current_segment
            .get_or_insert_with(|| ResourceSegmentBuilder::new(segment_key, sample))
            .record(sample);
    }

    fn finish(mut self) -> Option<ResourceTimeline> {
        if let Some(segment) = self.current_segment.take() {
            self.segments.push(segment.finish());
        }
        let first = self.first?;
        let last = self
            .last
            .expect("bug: first resource sample existed, so last must exist");
        let scope = first.scope.clone();
        let segment_growth: Vec<_> = self
            .segments
            .iter()
            .filter_map(|segment| segment.rss_growth_bytes_per_minute)
            .collect();
        let max_segment_rss_growth_bytes_per_minute = segment_growth.iter().copied().max();
        let last_segment_rss_growth_bytes_per_minute = self
            .segments
            .last()
            .and_then(|segment| segment.rss_growth_bytes_per_minute);
        let total_cpu_ms = self
            .segments
            .iter()
            .filter_map(|segment| segment.total_cpu_ms)
            .try_fold(0_u64, u64::checked_add);

        Some(ResourceTimeline {
            scope,
            sample_interval_ms: self.sample_interval_ms,
            sample_count: self.sample_count,
            first,
            last,
            max_rss_bytes: self.max_rss_bytes,
            max_segment_rss_growth_bytes_per_minute,
            last_segment_rss_growth_bytes_per_minute,
            peak_threads: self.peak_threads,
            peak_open_fds: self.peak_open_fds,
            total_cpu_ms,
            segments: self.segments,
            samples: self.samples,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResourceSegmentKey {
    scope: ResourceSampleScope,
    root_pid: u32,
    pgid: Option<u32>,
}

impl ResourceSegmentKey {
    fn from_sample(sample: &ResourceSamplePoint) -> Self {
        Self {
            scope: sample.scope.clone(),
            root_pid: sample.root_pid,
            pgid: sample.pgid,
        }
    }
}

#[derive(Debug)]
struct ResourceSegmentBuilder {
    key: ResourceSegmentKey,
    sample_count: u64,
    started_at_unix_ms: String,
    ended_at_unix_ms: String,
    first_rss_sample: Option<(u64, u64)>,
    last_rss_sample: Option<(u64, u64)>,
    max_rss_bytes: Option<u64>,
    first_cpu_ms: Option<u64>,
    last_cpu_ms: Option<u64>,
}

impl ResourceSegmentBuilder {
    fn new(key: ResourceSegmentKey, sample: &ResourceSamplePoint) -> Self {
        Self {
            key,
            sample_count: 0,
            started_at_unix_ms: sample.observed_at_unix_ms.clone(),
            ended_at_unix_ms: sample.observed_at_unix_ms.clone(),
            first_rss_sample: None,
            last_rss_sample: None,
            max_rss_bytes: None,
            first_cpu_ms: None,
            last_cpu_ms: None,
        }
    }

    fn record(&mut self, sample: &ResourceSamplePoint) {
        self.sample_count += 1;
        self.ended_at_unix_ms
            .clone_from(&sample.observed_at_unix_ms);
        if let Some(rss_bytes) = sample.rss_bytes {
            let observed_at_ms = unix_ms(&sample.observed_at_unix_ms);
            if self.first_rss_sample.is_none() {
                self.first_rss_sample = observed_at_ms.map(|time| (time, rss_bytes));
            }
            self.last_rss_sample = observed_at_ms.map(|time| (time, rss_bytes));
            self.max_rss_bytes = max_option(self.max_rss_bytes, Some(rss_bytes));
        }
        if let Some(cpu_ms) = sample_cpu_ms(sample) {
            if self.first_cpu_ms.is_none() {
                self.first_cpu_ms = Some(cpu_ms);
            }
            self.last_cpu_ms = Some(cpu_ms);
        }
    }

    fn finish(self) -> ResourceSegment {
        let rss_growth_bytes_per_minute = match (self.first_rss_sample, self.last_rss_sample) {
            (Some((first_time, first_rss)), Some((last_time, last_rss)))
                if last_time > first_time =>
            {
                let rss_delta = i128::from(last_rss) - i128::from(first_rss);
                let elapsed_ms = i128::from(last_time - first_time);
                i64::try_from((rss_delta * 60_000) / elapsed_ms).ok()
            }
            _ => None,
        };
        let total_cpu_ms = match (self.first_cpu_ms, self.last_cpu_ms) {
            (Some(first), Some(last)) if last >= first => Some(last - first),
            _ => None,
        };

        ResourceSegment {
            scope: self.key.scope,
            root_pid: self.key.root_pid,
            pgid: self.key.pgid,
            sample_count: self.sample_count,
            started_at_unix_ms: self.started_at_unix_ms,
            ended_at_unix_ms: self.ended_at_unix_ms,
            first_rss_bytes: self.first_rss_sample.map(|(_, rss)| rss),
            last_rss_bytes: self.last_rss_sample.map(|(_, rss)| rss),
            max_rss_bytes: self.max_rss_bytes,
            rss_growth_bytes_per_minute,
            total_cpu_ms,
        }
    }
}

fn sample_cpu_ms(sample: &ResourceSamplePoint) -> Option<u64> {
    sample.cpu_user_ms?.checked_add(sample.cpu_system_ms?)
}

fn unix_ms(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn max_option(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
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
        metrics.record_chain_tip(&ChainTipObservation {
            simulation_time_ms: 0,
            height: 7,
            block_hash: "0x07".into(),
        });
        metrics.record_chain_tip(&ChainTipObservation {
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
            metrics.record_chain_tip(&ChainTipObservation {
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
            metrics.record_chain_tip(&ChainTipObservation {
                simulation_time_ms: u64::try_from(index).expect("test time should fit u64"),
                height,
                block_hash: format!("0x{height:04x}"),
            });
        }
        let capped_height =
            i64::try_from(MAX_CHAIN_PROGRESS_OBSERVATIONS - 1).expect("test height should fit i64");
        metrics.record_chain_tip(&ChainTipObservation {
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

    #[test]
    fn resource_timeline_segments_restart_sawtooth_and_sums_cpu_per_segment() {
        let mut metrics = RunMetricsBuilder::default();
        metrics.set_resource_sample_interval_ms(1_000);
        metrics.record_resource_sample(&resource_sample(
            0,
            "1000",
            10,
            Some(100),
            Some(10),
            Some(5),
        ));
        metrics.record_resource_sample(&resource_sample(
            1_000,
            "61000",
            10,
            Some(160),
            Some(20),
            Some(8),
        ));
        metrics.record_resource_sample(&resource_sample(
            2_000,
            "62000",
            11,
            Some(90),
            Some(1),
            Some(1),
        ));
        metrics.record_resource_sample(&resource_sample(
            3_000,
            "122000",
            11,
            Some(150),
            Some(7),
            Some(3),
        ));

        let timeline = metrics
            .finish()
            .resource_timeline
            .expect("resource timeline should exist");

        assert_eq!(timeline.sample_interval_ms, 1_000);
        assert_eq!(timeline.sample_count, 4);
        assert_eq!(timeline.max_rss_bytes, Some(160));
        assert_eq!(timeline.segments.len(), 2);
        assert_eq!(timeline.segments[0].root_pid, 10);
        assert_eq!(timeline.segments[0].rss_growth_bytes_per_minute, Some(60));
        assert_eq!(timeline.segments[0].total_cpu_ms, Some(13));
        assert_eq!(timeline.segments[1].root_pid, 11);
        assert_eq!(timeline.segments[1].rss_growth_bytes_per_minute, Some(60));
        assert_eq!(timeline.segments[1].total_cpu_ms, Some(8));
        assert_eq!(timeline.total_cpu_ms, Some(21));
    }

    #[test]
    fn resource_timeline_cap_preserves_aggregates_and_partial_fields() {
        let mut metrics = RunMetricsBuilder::default();
        for index in 0..=MAX_RESOURCE_SAMPLES {
            let pid = 10;
            metrics.record_resource_sample(&ResourceSamplePoint {
                simulation_time_ms: u64::try_from(index).expect("index should fit"),
                observed_at_unix_ms: (1_000_u64 + u64::try_from(index).expect("index should fit"))
                    .to_string(),
                scope: ResourceSampleScope::ProcessGroup,
                root_pid: pid,
                pgid: Some(pid),
                process_count: 2,
                rss_bytes: Some(u64::try_from(index).expect("index should fit")),
                cpu_user_ms: Some(u64::try_from(index).expect("index should fit")),
                cpu_system_ms: Some(0),
                threads: Some(4),
                open_fds: None,
            });
        }

        let timeline = metrics
            .finish()
            .resource_timeline
            .expect("resource timeline should exist");

        assert_eq!(timeline.sample_count, 1_025);
        assert_eq!(timeline.samples.len(), MAX_RESOURCE_SAMPLES);
        assert_eq!(timeline.max_rss_bytes, Some(1_024));
        assert_eq!(timeline.peak_threads, Some(4));
        assert_eq!(timeline.peak_open_fds, None);
        assert_eq!(timeline.last.rss_bytes, Some(1_024));
    }

    fn resource_sample(
        simulation_time_ms: u64,
        observed_at_unix_ms: &str,
        root_pid: u32,
        rss_bytes: Option<u64>,
        cpu_user_ms: Option<u64>,
        cpu_system_ms: Option<u64>,
    ) -> ResourceSamplePoint {
        ResourceSamplePoint {
            simulation_time_ms,
            observed_at_unix_ms: observed_at_unix_ms.into(),
            scope: ResourceSampleScope::ProcessGroup,
            root_pid,
            pgid: Some(root_pid),
            process_count: 2,
            rss_bytes,
            cpu_user_ms,
            cpu_system_ms,
            threads: Some(8),
            open_fds: Some(16),
        }
    }
}
