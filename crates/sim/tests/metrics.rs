use thruput_sim::manifest::WorkloadKind;
use thruput_sim::metrics::{RunMetricsBuilder, SIMULATOR_BACKPRESSURE_ERROR_CLASS};
use thruput_sim::result::{
    ChainTipObservation, LatencySummary, ReceiptPollSummary, ResourceSamplePoint,
    ResourceSampleScope,
};

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
        .expect("bug: chain-tip metrics should exist");

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
fn simulator_backpressure_count_ignores_other_skips() {
    let mut metrics = RunMetricsBuilder::default();
    metrics.record_skipped(WorkloadKind::ReceiptPoll, "no-submitted-transaction");
    metrics.record_skipped(
        WorkloadKind::WalletSubmitSmoke,
        SIMULATOR_BACKPRESSURE_ERROR_CLASS,
    );

    assert_eq!(metrics.skipped_count(), 2);
    assert_eq!(metrics.simulator_backpressure_count(), 1);
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
        .expect("bug: receipt-poll metrics should exist");

    assert_eq!(
        receipt_poll
            .receipt_poll
            .as_ref()
            .expect("bug: receipt-poll summary should exist"),
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
        .expect("bug: progress should exist");
    assert_eq!(progress.first_height, 7);
    assert_eq!(progress.last_height, 9);
    assert_eq!(progress.max_height, 9);
    assert_eq!(progress.total_observations, 2);
    assert_eq!(progress.max_stall_ms, Some(1_000));
    assert_eq!(progress.observations.len(), 2);
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
        .expect("bug: resource timeline should exist");

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
fn diagnostics_summarize_error_classes_and_resource_failures() {
    let mut metrics = RunMetricsBuilder::default();
    metrics.record_error_diagnostic(
        WorkloadKind::ChainTip,
        "groundhog_chainTip",
        2_000,
        "rpc-error",
        Some("Request rejected `429`"),
    );
    metrics.record_error_diagnostic(
        WorkloadKind::EventsQuery,
        "groundhog_getEvents",
        1_000,
        "rpc-error",
        Some("Request rejected `429`"),
    );
    metrics.record_error_diagnostic(
        WorkloadKind::StorageRead,
        "groundhog_getStorageAt",
        3_000,
        "storage-read-rpc-error",
        Some("client error"),
    );
    metrics.record_resource_sample_failure(25_000, "failed to read /proc");
    metrics.record_resource_sample_failure(30_000, "failed to read /proc");

    let diagnostics = metrics.finish().diagnostics;

    assert_eq!(diagnostics.error_classes.len(), 2);
    assert_eq!(diagnostics.error_classes[0].error_class, "rpc-error");
    assert_eq!(diagnostics.error_classes[0].count, 2);
    assert_eq!(diagnostics.error_classes[0].first_scheduled_time_ms, 1_000);
    assert_eq!(diagnostics.error_classes[0].last_scheduled_time_ms, 2_000);
    assert_eq!(
        diagnostics.error_classes[0]
            .workloads
            .get(&WorkloadKind::ChainTip),
        Some(&1)
    );
    assert_eq!(
        diagnostics.error_classes[0]
            .rpc_methods
            .get("groundhog_getEvents"),
        Some(&1)
    );
    assert_eq!(
        diagnostics.error_classes[0].sample_error.as_deref(),
        Some("Request rejected `429`")
    );
    assert_eq!(diagnostics.resource_sample_failures.len(), 1);
    assert_eq!(
        diagnostics.resource_sample_failures[0].reason,
        "failed to read /proc"
    );
    assert_eq!(diagnostics.resource_sample_failures[0].count, 2);
    assert_eq!(
        diagnostics.resource_sample_failures[0].first_scheduled_time_ms,
        25_000
    );
    assert_eq!(
        diagnostics.resource_sample_failures[0].last_scheduled_time_ms,
        30_000
    );
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
