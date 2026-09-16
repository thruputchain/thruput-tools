//! Bounded scheduling and measurement-window accounting.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::{
    BenchError,
    corpus::ConfirmedRecord,
    request::{ExpectedResult, RequestOutcome, classify_request},
    results::RequestMetrics,
    workload::make_request,
};

pub(super) struct WorkerResult {
    pub(super) histogram: Histogram<u64>,
    pub(super) requests: RequestMetrics,
    pub(super) drained: RequestMetrics,
}

pub(super) struct WorkerConfig {
    pub(super) url: String,
    pub(super) method: String,
    pub(super) tx_batch: Vec<String>,
    pub(super) records: Vec<ConfirmedRecord>,
    pub(super) start: Instant,
    pub(super) warmup_duration: Duration,
    pub(super) measured_duration: Duration,
    pub(super) interval: Option<Duration>,
    pub(super) max_skew: Duration,
    pub(super) request_timeout: Duration,
    pub(super) max_response_bytes: usize,
    pub(super) num_workers: usize,
}

struct Sample {
    started: Instant,
    completed: Instant,
    outcome: RequestOutcome,
}

impl WorkerResult {
    fn new() -> Self {
        Self {
            histogram: Histogram::<u64>::new(3).expect("bug: histogram"),
            requests: RequestMetrics::default(),
            drained: RequestMetrics::default(),
        }
    }

    fn record(&mut self, sample: Sample, warmup_end: Instant, end: Instant) {
        if sample.started < warmup_end || sample.started >= end {
            return;
        }
        let counts = if sample.completed < end {
            &mut self.requests
        } else {
            &mut self.drained
        };
        counts.record(sample.outcome);
        if sample.completed < end && sample.outcome == RequestOutcome::Success {
            let micros = sample.completed.duration_since(sample.started).as_micros() as u64;
            self.histogram.saturating_record(micros);
        }
    }
}

pub(super) async fn run_worker(
    worker_id: usize,
    cfg: Arc<WorkerConfig>,
    client: reqwest::Client,
    inflight: Arc<Semaphore>,
) -> Result<WorkerResult, BenchError> {
    let mut result = WorkerResult::new();
    let mut tasks = JoinSet::new();
    let warmup_end = cfg.start + cfg.warmup_duration;
    let end = warmup_end + cfg.measured_duration;
    let mut dispatched = 0u64;
    let skew_check_after = cfg.start + Duration::from_secs(1).min(cfg.warmup_duration);
    let mut next_fire = cfg.start + cfg.interval.unwrap_or_default() * worker_id as u32;
    let end_sleep = tokio::time::sleep_until(end.into());
    tokio::pin!(end_sleep);

    loop {
        while let Some(sample) = tasks.try_join_next() {
            result.record(
                sample.map_err(|source| BenchError::WorkerJoin { source })?,
                warmup_end,
                end,
            );
        }
        if Instant::now() >= end {
            break;
        }
        let scheduled = next_fire;
        if let Some(interval) = cfg.interval {
            if next_fire >= end {
                break;
            }
            tokio::time::sleep_until(next_fire.into()).await;
            let now = Instant::now();
            if now >= end {
                break;
            }
            if now > cfg.start + Duration::from_secs(1).min(cfg.warmup_duration)
                && now.saturating_duration_since(next_fire) > cfg.max_skew
            {
                return Err(BenchError::Setup(format!(
                    "worker {worker_id} cannot sustain the requested rate; reduce --rate or increase --workers"
                )));
            }
            next_fire += interval * cfg.num_workers as u32;
        }
        // Drain completed tasks while waiting; a global semaphore is shared in
        // --outstanding mode, and the deadline bounds admission in every mode.
        let skew_deadline = (scheduled + cfg.max_skew).max(skew_check_after);
        let skew_sleep = tokio::time::sleep_until(skew_deadline.into());
        tokio::pin!(skew_sleep);
        let permit = loop {
            tokio::select! {
                biased;
                () = &mut skew_sleep, if cfg.interval.is_some() => {
                    return Err(BenchError::Setup(format!("worker {worker_id} cannot sustain the requested rate while waiting for capacity; reduce --rate or increase --workers")));
                }
                () = &mut end_sleep => break None,
                sample = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(sample) = sample {
                        result.record(sample.map_err(|source| BenchError::WorkerJoin { source })?, warmup_end, end);
                    }
                }
                permit = inflight.clone().acquire_owned() => {
                    break Some(permit.expect("bug: semaphore remains open"));
                }
            }
        };
        let Some(permit) = permit else {
            break;
        };
        if cfg.interval.is_some() && Instant::now() > skew_deadline {
            return Err(BenchError::Setup(format!(
                "worker {worker_id} cannot sustain the requested rate; reduce --rate or increase --workers"
            )));
        }
        if Instant::now() >= end {
            break;
        }
        let tick = worker_id as u64 + dispatched * cfg.num_workers as u64;
        let (input, expected) = if cfg.records.is_empty() {
            let input = if cfg.tx_batch.is_empty() {
                String::new()
            } else {
                cfg.tx_batch[tick as usize % cfg.tx_batch.len()].clone()
            };
            (input, ExpectedResult::Any)
        } else {
            let record = cfg.records[tick as usize % cfg.records.len()];
            let expected = if cfg.method == "getTransactionStatus" {
                ExpectedResult::Committed {
                    height: record.height,
                }
            } else {
                ExpectedResult::Receipt {
                    hash: record.hash,
                    height: record.height,
                }
            };
            (
                serde_json::to_value(record.hash)
                    .expect("bug: hash serializes")
                    .as_str()
                    .expect("bug: hash is a string")
                    .to_owned(),
                expected,
            )
        };
        let body = make_request(&cfg.method, &input, tick);
        let cfg = cfg.clone();
        let client = client.clone();
        tasks.spawn(async move {
            let started = Instant::now();
            // A task scheduled after cutoff must not send late traffic.
            if started >= end {
                drop(permit);
                return Sample {
                    started,
                    completed: started,
                    outcome: RequestOutcome::Success,
                };
            }
            let outcome = classify_request(
                client.post(&cfg.url).json(&body).send(),
                tick,
                cfg.request_timeout,
                cfg.max_response_bytes,
                expected,
            )
            .await;
            let completed = Instant::now();
            drop(permit);
            Sample {
                started,
                completed,
                outcome,
            }
        });
        dispatched += 1;
    }
    // Every request has a complete-response timeout; joining does not extend
    // the throughput window. Warmup requests remain excluded even during drain.
    while let Some(sample) = tasks.join_next().await {
        result.record(
            sample.map_err(|source| BenchError::WorkerJoin { source })?,
            warmup_end,
            end,
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RequestErrorKind;

    #[test]
    fn window_excludes_warmup_and_late_starts_and_accounts_drain_separately() {
        let warmup_end = Instant::now() + Duration::from_secs(1);
        let end = warmup_end + Duration::from_secs(1);
        let mut result = WorkerResult::new();
        for (started, completed, outcome) in [
            (
                warmup_end - Duration::from_millis(1),
                warmup_end,
                RequestOutcome::Success,
            ),
            (
                warmup_end,
                end - Duration::from_millis(1),
                RequestOutcome::Success,
            ),
            (warmup_end, end, RequestOutcome::Success),
            (
                warmup_end,
                end + Duration::from_millis(1),
                RequestOutcome::Error(RequestErrorKind::Transport),
            ),
            (end, end, RequestOutcome::Success),
        ] {
            result.record(
                Sample {
                    started,
                    completed,
                    outcome,
                },
                warmup_end,
                end,
            );
        }
        assert_eq!(result.requests.total, 1);
        assert_eq!(result.histogram.len(), 1);
        assert_eq!(result.drained.total, 2);
        assert_eq!(result.drained.successful, 1);
        assert_eq!(result.drained.failed, 1);
    }
}
