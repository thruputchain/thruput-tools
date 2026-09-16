//! Open-loop and unthrottled scheduling with per-worker accounting.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

use crate::request::{RequestErrorKind, RequestOutcome, classify_request};
use crate::results::RequestErrorCounts;
use crate::workload::make_request;

/// Per-worker state. Each worker owns its own histogram to avoid lock
/// contention on the hot path.
pub(super) struct WorkerResult {
    pub(super) histogram: Histogram<u64>,
    pub(super) successes: u64,
    pub(super) errors: RequestErrorCounts,
}

/// Parameters shared across all workers.
pub(super) struct WorkerConfig {
    pub(super) url: String,
    pub(super) method: String,
    pub(super) tx_batch: Arc<Vec<String>>,
    pub(super) start: Instant,
    pub(super) warmup_duration: Duration,
    pub(super) total_run_duration: Duration,
    pub(super) interval: Duration,
    pub(super) max_skew: Duration,
    pub(super) request_timeout: Duration,
    pub(super) max_response_bytes: usize,
    pub(super) num_workers: usize,
    pub(super) unthrottled: bool,
}

/// Latency sample sent from a spawned request task back to its worker.
enum Sample {
    Success(u64), // latency in microseconds
    Error(RequestErrorKind),
}

/// Fold a sample into the per-worker counters.
fn record_sample(
    sample: Sample,
    histogram: &mut Histogram<u64>,
    successes: &mut u64,
    errors: &mut RequestErrorCounts,
) {
    match sample {
        Sample::Success(us) => {
            histogram.record(us).ok();
            *successes += 1;
        }
        Sample::Error(kind) => errors.record(kind),
    }
}

/// A single worker task. Fires requests on its slice of the global
/// schedule and panics if it falls behind by more than `max_skew`.
pub(super) async fn run_worker(
    worker_id: usize,
    cfg: &WorkerConfig,
    client: reqwest::Client,
    inflight_sem: Arc<tokio::sync::Semaphore>,
) -> WorkerResult {
    if cfg.unthrottled {
        return run_unthrottled_worker(worker_id, cfg, client, inflight_sem).await;
    }

    let mut histogram =
        Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).expect("bug: histogram");
    let mut successes = 0u64;
    let mut errors = RequestErrorCounts::default();

    // Spawned tasks send latency samples back over this channel so the
    // worker can record them into its own histogram without a shared
    // mutex on the hot path. Unbounded — sends are non-blocking and
    // infallible. In steady state the queue stays near-empty because
    // we drain at the top of every tick and while awaiting the
    // inflight semaphore (see the select below). The overall bench
    // rate is bounded by the inflight semaphore and the schedule, so
    // sample accumulation is bounded in practice even without a
    // channel cap.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Sample>();

    // Each worker fires every `num_workers` intervals, offset by worker_id.
    let worker_interval = cfg.interval * cfg.num_workers as u32;
    let mut next_fire = cfg.start + cfg.interval * worker_id as u32;
    let end_time = cfg.start + cfg.total_run_duration;
    let mut dispatched = 0u64;

    // Don't check skew until connections are established and JIT is warm.
    let skew_check_after = cfg.start + Duration::from_secs(1).min(cfg.warmup_duration);

    loop {
        // Drain any completed samples without blocking the dispatch loop.
        while let Ok(sample) = rx.try_recv() {
            record_sample(sample, &mut histogram, &mut successes, &mut errors);
        }

        if Instant::now() >= end_time {
            break;
        }

        let now = Instant::now();
        if now < next_fire {
            tokio::time::sleep(next_fire - now).await;
        } else if now > skew_check_after {
            let skew = now - next_fire;
            if skew > cfg.max_skew {
                let elapsed = (now - cfg.start).as_secs_f64();
                let dispatch_rate = dispatched as f64 / elapsed * cfg.num_workers as f64;
                panic!(
                    "worker {worker_id}: overloaded — {skew:.1?} behind schedule \
                     (limit: {:.1?}). Dispatched ~{dispatch_rate:.0} req/s across \
                     all workers. Reduce --rate or increase --workers.",
                    cfg.max_skew,
                );
            }
        }

        if next_fire >= end_time {
            break;
        }

        let scheduled_time = next_fire;
        next_fire += worker_interval;

        let in_warmup = scheduled_time < cfg.start + cfg.warmup_duration;

        let tick = ((scheduled_time - cfg.start).as_nanos() / cfg.interval.as_nanos()) as u64;

        let tx_hex: String = if cfg.tx_batch.is_empty() {
            String::new()
        } else {
            cfg.tx_batch[tick as usize % cfg.tx_batch.len()].clone()
        };
        let body = make_request(&cfg.method, &tx_hex, tick);

        // Acquire a permit, but keep draining samples while we wait so
        // the channel stays shallow under bursty completion timing.
        // The inflight semaphore is the only real backpressure point;
        // the select is pure telemetry hygiene.
        let permit = loop {
            tokio::select! {
                biased;
                sample = rx.recv() => {
                    if let Some(s) = sample {
                        record_sample(s, &mut histogram, &mut successes, &mut errors);
                    }
                }
                permit = inflight_sem.clone().acquire_owned() => {
                    break permit.expect("bug: semaphore closed");
                }
            }
        };

        let client = client.clone();
        let url = cfg.url.clone();
        let tx = tx.clone();
        let request_timeout = cfg.request_timeout;
        let max_response_bytes = cfg.max_response_bytes;

        tokio::spawn(async move {
            let req_start = Instant::now();
            let request = client.post(&url).json(&body).send();
            let outcome =
                classify_request(request, tick, request_timeout, max_response_bytes).await;
            let latency_us = req_start.elapsed().as_micros() as u64;
            drop(permit);

            if in_warmup {
                return;
            }

            let sample = match outcome {
                RequestOutcome::Success => Sample::Success(latency_us),
                RequestOutcome::Error(kind) => Sample::Error(kind),
            };
            // Unbounded send — infallible unless the worker has dropped
            // its receiver (which only happens after the dispatch loop
            // exits, at which point in-flight tasks we spawned are
            // still valid; any stragglers that return here after that
            // simply lose their sample, which is fine).
            let _ = tx.send(sample);
        });

        dispatched += 1;
    }

    // Drop the sender so rx.recv() will return None once all spawned
    // tasks complete (they hold clones of tx).
    drop(tx);

    // Drain remaining in-flight samples.
    while let Some(sample) = rx.recv().await {
        record_sample(sample, &mut histogram, &mut successes, &mut errors);
    }

    WorkerResult {
        histogram,
        successes,
        errors,
    }
}

/// A single unthrottled worker task. Dispatches as fast as the per-worker
/// in-flight cap allows and records completions outside the warmup window.
async fn run_unthrottled_worker(
    worker_id: usize,
    cfg: &WorkerConfig,
    client: reqwest::Client,
    inflight_sem: Arc<tokio::sync::Semaphore>,
) -> WorkerResult {
    let mut histogram =
        Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).expect("bug: histogram");
    let mut successes = 0u64;
    let mut errors = RequestErrorCounts::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Sample>();

    let end_time = cfg.start + cfg.total_run_duration;
    let warmup_end = cfg.start + cfg.warmup_duration;
    let mut dispatched = 0u64;

    loop {
        while let Ok(sample) = rx.try_recv() {
            record_sample(sample, &mut histogram, &mut successes, &mut errors);
        }

        if Instant::now() >= end_time {
            break;
        }

        let permit = loop {
            tokio::select! {
                biased;
                sample = rx.recv() => {
                    if let Some(s) = sample {
                        record_sample(s, &mut histogram, &mut successes, &mut errors);
                    }
                }
                permit = inflight_sem.clone().acquire_owned() => {
                    break permit.expect("bug: semaphore closed");
                }
            }
        };

        if Instant::now() >= end_time {
            drop(permit);
            break;
        }

        let tick = worker_id as u64 + dispatched * cfg.num_workers as u64;
        let tx_hex: String = if cfg.tx_batch.is_empty() {
            String::new()
        } else {
            cfg.tx_batch[tick as usize % cfg.tx_batch.len()].clone()
        };
        let body = make_request(&cfg.method, &tx_hex, tick);
        let in_warmup = Instant::now() < warmup_end;
        let client = client.clone();
        let url = cfg.url.clone();
        let tx = tx.clone();
        let request_timeout = cfg.request_timeout;
        let max_response_bytes = cfg.max_response_bytes;

        tokio::spawn(async move {
            let req_start = Instant::now();
            let request = client.post(&url).json(&body).send();
            let outcome =
                classify_request(request, tick, request_timeout, max_response_bytes).await;
            let latency_us = req_start.elapsed().as_micros() as u64;
            drop(permit);

            if in_warmup {
                return;
            }

            let sample = match outcome {
                RequestOutcome::Success => Sample::Success(latency_us),
                RequestOutcome::Error(kind) => Sample::Error(kind),
            };
            let _ = tx.send(sample);
        });

        dispatched += 1;
    }

    drop(tx);
    while let Some(sample) = rx.recv().await {
        record_sample(sample, &mut histogram, &mut successes, &mut errors);
    }

    WorkerResult {
        histogram,
        successes,
        errors,
    }
}
