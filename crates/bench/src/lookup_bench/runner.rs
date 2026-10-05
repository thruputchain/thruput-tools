use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hdrhistogram::Histogram;
use serde::Serialize;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::transport::{self, Outcome};
use super::{Args, Mode};

type Error = Box<dyn std::error::Error>;

#[derive(Default, Serialize)]
struct Counts {
    scheduled: u64,
    started: u64,
    completed: u64,
    completed_during_drain: u64,
    skipped_capacity: u64,
    skipped_schedule: u64,
    max_in_flight: usize,
    outcomes: BTreeMap<String, u64>,
}

#[derive(Serialize)]
struct Latency {
    count: u64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

fn latency(histogram: &Histogram<u64>) -> Option<Latency> {
    (!histogram.is_empty()).then(|| Latency {
        count: histogram.len(),
        mean_ms: histogram.mean() / 1000.0,
        p50_ms: histogram.value_at_quantile(0.50) as f64 / 1000.0,
        p95_ms: histogram.value_at_quantile(0.95) as f64 / 1000.0,
        p99_ms: histogram.value_at_quantile(0.99) as f64 / 1000.0,
        max_ms: histogram.max() as f64 / 1000.0,
    })
}

fn timestamp_ms() -> Result<u128, Error> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

async fn metrics(client: &reqwest::Client, url: Option<&str>) -> Option<String> {
    let url = url?;
    tokio::time::timeout(Duration::from_secs(2), async {
        let response = client.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = transport::bounded_body(response, 4 * 1024 * 1024)
            .await
            .ok()?;
        String::from_utf8(body).ok()
    })
    .await
    .ok()
    .flatten()
}

fn lookup_values(raw: Option<&str>) -> BTreeMap<String, f64> {
    raw.into_iter()
        .flat_map(str::lines)
        .filter(|line| {
            line.starts_with("groundhog_gateway_scheduler_lookup_")
                || (line.starts_with("groundhog_gateway_backend_")
                    && line.contains("operation=\"schedgateway_lookup_transaction_outcomes\""))
        })
        .filter_map(|line| {
            let (name, value) = line.rsplit_once(' ')?;
            Some((name.to_owned(), value.parse::<f64>().ok()?))
        })
        .collect()
}

fn counter_deltas(before: Option<&str>, after: Option<&str>) -> BTreeMap<String, Option<f64>> {
    let before = lookup_values(before);
    lookup_values(after)
        .into_iter()
        .filter(|(name, _)| !name.contains("_in_flight{"))
        .map(|(name, value)| {
            let delta = before
                .get(&name)
                .and_then(|start| (value >= *start).then_some(value - start));
            (name, delta)
        })
        .collect()
}

// Count ticks strictly before the dispatch deadline, including a late final wake.
fn remaining_ticks(next: Instant, deadline: Instant, period: Duration) -> u64 {
    if next >= deadline {
        return 0;
    }
    deadline
        .duration_since(next)
        .as_nanos()
        .div_ceil(period.as_nanos()) as u64
}

struct Completion {
    outcome: Outcome,
    micros: u64,
    finished: Instant,
}

fn record(
    completion: Completion,
    deadline: Instant,
    counts: &mut Counts,
    success: &mut Histogram<u64>,
    overloaded: &mut Histogram<u64>,
) -> Result<(), Error> {
    counts.completed += 1;
    counts.completed_during_drain += u64::from(completion.finished >= deadline);
    let key = match completion.outcome {
        Outcome::Unknown => {
            success.record(completion.micros)?;
            "unknown".to_owned()
        }
        Outcome::Overloaded(code) => {
            overloaded.record(completion.micros)?;
            format!("overloaded:{code}")
        }
        Outcome::RpcError(code) => format!("rpc_error:{code}"),
        other => serde_json::to_value(other)?["kind"]
            .as_str()
            .unwrap_or("invalid_response")
            .into(),
    };
    *counts.outcomes.entry(key).or_default() += 1;
    Ok(())
}

pub(super) async fn run(args: Args) -> Result<(), Error> {
    // Reserve the artifact before sending any requests.
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.output)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(u64::from(args.timeout_ms)))
        .build()?;
    let corpus = Arc::new(transport::corpus(&args.seed, args.corpus_size));
    let timeout = Duration::from_millis(u64::from(args.timeout_ms));
    let mut preflight = Vec::new();
    for (id, hash) in corpus.iter().take(3).enumerate() {
        preflight.push(transport::request(&client, &args.url, hash, id as u64, timeout).await);
    }
    if preflight.iter().any(|outcome| *outcome != Outcome::Unknown) {
        serde_json::to_writer_pretty(
            &mut output,
            &serde_json::json!({
                "schema_version":1,"config":args,"preflight":preflight,
                "error":"preflight expected Unknown; no load run started"
            }),
        )?;
        return Err("preflight failed; see result artifact".into());
    }
    let before = metrics(&client, args.metrics_url.as_deref()).await;
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let metrics_client = client.clone();
    let metrics_url = args.metrics_url.clone();
    let sampler = tokio::spawn(async move {
        let mut peaks: BTreeMap<String, f64> = BTreeMap::new();
        let mut samples = 0_u64;
        if metrics_url.is_none() {
            return (samples, peaks);
        }
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if let Some(raw) = metrics(&metrics_client, metrics_url.as_deref()).await {
                        samples += 1;
                        for (name, value) in lookup_values(Some(&raw)) {
                            if name.contains("_in_flight{") {
                                peaks.entry(name).and_modify(|peak| *peak = peak.max(value))
                                    .or_insert(value);
                            }
                        }
                    }
                }
            }
        }
        (samples, peaks)
    });
    let started_unix_ms = timestamp_ms()?;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(u64::from(args.duration_secs));
    let period = match args.mode {
        Mode::Paced => Duration::from_secs_f64(1.0 / f64::from(args.rate)),
        _ => Duration::from_millis(u64::from(args.burst_interval_ms)),
    };
    let mut next = start;
    let mut counts = Counts::default();
    let mut success = Histogram::<u64>::new(3)?;
    let mut overloaded = Histogram::<u64>::new(3)?;
    let mut tasks = JoinSet::new();
    loop {
        while let Some(completion) = tasks.try_join_next() {
            record(
                completion?,
                deadline,
                &mut counts,
                &mut success,
                &mut overloaded,
            )?;
        }
        let now = Instant::now();
        if now >= deadline {
            if args.mode != Mode::ClosedLoop {
                let batch = if args.mode == Mode::Burst {
                    u64::from(args.burst_size)
                } else {
                    1
                };
                let missed = remaining_ticks(next, deadline, period) * batch;
                counts.scheduled += missed;
                counts.skipped_schedule += missed;
            }
            break;
        }
        let available = args.concurrency as usize - tasks.len();
        let launch = if args.mode == Mode::ClosedLoop {
            counts.scheduled += available as u64;
            available
        } else if now >= next {
            let overdue = (now.duration_since(next).as_nanos() / period.as_nanos()) as u64;
            let batch = if args.mode == Mode::Burst {
                u64::from(args.burst_size)
            } else {
                1
            };
            counts.skipped_schedule += overdue * batch;
            counts.scheduled += (overdue + 1) * batch;
            // Drop missed ticks, never catch up by sending a delayed burst.
            next += period.mul_f64((overdue + 1) as f64);
            let launch = available.min(batch as usize);
            counts.skipped_capacity += batch - launch as u64;
            launch
        } else {
            0
        };
        for _ in 0..launch {
            let id = counts.started + 100;
            let hash = corpus[counts.started as usize % corpus.len()].clone();
            counts.started += 1;
            let client = client.clone();
            let url = args.url.clone();
            tasks.spawn(async move {
                let started = Instant::now();
                let outcome = transport::request(&client, &url, &hash, id, timeout).await;
                Completion {
                    outcome,
                    micros: started.elapsed().as_micros() as u64,
                    finished: Instant::now(),
                }
            });
        }
        counts.max_in_flight = counts.max_in_flight.max(tasks.len());
        let wake = if args.mode == Mode::ClosedLoop {
            deadline
        } else {
            next.min(deadline)
        };
        tokio::select! {
            completion = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(completion) = completion {
                    record(completion?, deadline, &mut counts, &mut success, &mut overloaded)?;
                }
            }
            _ = tokio::time::sleep_until(wake) => {}
        }
    }
    while let Some(completion) = tasks.join_next().await {
        record(
            completion?,
            deadline,
            &mut counts,
            &mut success,
            &mut overloaded,
        )?;
    }
    let total_elapsed_secs = start.elapsed().as_secs_f64();
    let ended_unix_ms = timestamp_ms()?;
    let _ = stop.send(true);
    let (samples, peaks) = sampler.await?;
    let after = metrics(&client, args.metrics_url.as_deref()).await;
    let report = serde_json::json!({
        "schema_version":1, "tool_version":env!("CARGO_PKG_VERSION"),
        "config":args, "preflight":preflight,
        "started_unix_ms":started_unix_ms, "ended_unix_ms":ended_unix_ms,
        "dispatch_duration_secs":args.duration_secs, "total_elapsed_secs":total_elapsed_secs,
        "started_per_dispatch_second":counts.started as f64 / f64::from(args.duration_secs),
        "completed_per_total_second":counts.completed as f64 / total_elapsed_secs,
        "counts":counts, "unknown_latency":latency(&success),
        "overloaded_latency":latency(&overloaded),
        "metrics": {"scope":"whole server; not attributable to this run on a shared target",
            "before":before,"after":after,"successful_samples":samples,
            "sampled_gauge_peaks":peaks,"counter_deltas":counter_deltas(
                before.as_deref(),after.as_deref())}
    });
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    eprintln!(
        "Saved {} ({} started, {} completed)",
        args.output.display(),
        counts.started,
        counts.completed
    );
    if counts
        .outcomes
        .keys()
        .any(|key| key != "unknown" && !key.starts_with("overloaded:"))
    {
        return Err("non-overload request failures; see result artifact".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn paced_run_bounds_concurrency_and_reports_skips_and_drain() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(AtomicUsize::new(0));
        let server_peak = peak.clone();
        let server_seen = seen.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let active = active.clone();
                let peak = server_peak.clone();
                let seen = server_seen.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    let request = loop {
                        let count = stream.read(&mut buffer).await.unwrap();
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(boundary) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..boundary]);
                            let length: usize = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse().unwrap())
                                })
                                .unwrap();
                            if bytes.len() >= boundary + 4 + length {
                                break serde_json::from_slice::<serde_json::Value>(
                                    &bytes[boundary + 4..boundary + 4 + length],
                                )
                                .unwrap();
                            }
                        }
                    };
                    assert_eq!(request["method"], "groundhog_getTransactionStatus");
                    assert_eq!(request["params"][0].as_str().unwrap().len(), 66);
                    peak.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    let index = seen.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(45)).await;
                    let mut response = serde_json::json!({"jsonrpc":"2.0", "id":request["id"]});
                    if index >= 3 && index.is_multiple_of(2) {
                        response["error"] =
                            serde_json::json!({"code":-32053,"message":"Overloaded"});
                    } else {
                        response["result"] = serde_json::json!({"status":"unknown"});
                    }
                    let body = response.to_string();
                    let wire = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                        Connection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    active.fetch_sub(1, Ordering::SeqCst);
                    stream.write_all(wire.as_bytes()).await.unwrap();
                });
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("result.json");
        let args = Args::parse_from([
            "lookup",
            "--url",
            &url,
            "--duration-secs",
            "1",
            "--concurrency",
            "2",
            "--rate",
            "200",
            "--output",
            output.to_str().unwrap(),
        ]);
        run(args).await.unwrap();
        server.abort();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        let counts = &report["counts"];
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert_eq!(counts["started"], counts["completed"]);
        assert!(counts["skipped_capacity"].as_u64().unwrap() > 0);
        assert!(counts["outcomes"]["overloaded:-32053"].as_u64().unwrap() > 0);
        assert_eq!(
            counts["scheduled"].as_u64().unwrap(),
            counts["started"].as_u64().unwrap()
                + counts["skipped_capacity"].as_u64().unwrap()
                + counts["skipped_schedule"].as_u64().unwrap()
        );
        assert_eq!(
            seen.load(Ordering::SeqCst) as u64,
            counts["started"].as_u64().unwrap() + 3
        );
        assert!(report["metrics"]["before"].is_null());
    }

    #[test]
    fn delayed_final_wake_counts_every_unserved_tick_before_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(1);
        let period = Duration::from_millis(10);
        assert_eq!(
            remaining_ticks(start + Duration::from_millis(970), deadline, period),
            3
        );
        assert_eq!(remaining_ticks(deadline, deadline, period), 0);
        assert_eq!(remaining_ticks(deadline + period, deadline, period), 0);
    }

    #[test]
    fn metric_deltas_include_scheduler_outcome_backend_measurements() {
        let request = "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedgateway_lookup_transaction_outcomes\",outcome=\"ok\"}";
        let duration_count = "groundhog_gateway_backend_request_duration_seconds_count{backend=\"scheduler\",operation=\"schedgateway_lookup_transaction_outcomes\",result=\"ok\"}";
        let duration_sum = "groundhog_gateway_backend_request_duration_seconds_sum{backend=\"scheduler\",operation=\"schedgateway_lookup_transaction_outcomes\",result=\"ok\"}";
        let admission = "groundhog_gateway_scheduler_lookup_admission_total{outcome=\"accepted\"}";
        let unrelated = "groundhog_gateway_backend_requests_total{backend=\"scheduler\",operation=\"schedgateway_get_tip\",outcome=\"ok\"}";
        let before = format!(
            "{request} 5\n{duration_count} 5\n{duration_sum} 1.5\n{admission} 5\n{unrelated} 10"
        );
        let after = format!(
            "{request} 8\n{duration_count} 8\n{duration_sum} 2.25\n{admission} 8\n{unrelated} 20"
        );
        assert_eq!(
            counter_deltas(Some(&before), Some(&after)),
            BTreeMap::from([
                (request.to_owned(), Some(3.0)),
                (duration_count.to_owned(), Some(3.0)),
                (duration_sum.to_owned(), Some(0.75)),
                (admission.to_owned(), Some(3.0)),
            ])
        );
    }

    #[test]
    fn metric_resets_and_missing_baselines_are_unavailable() {
        let name = "groundhog_gateway_scheduler_lookup_admission_total{outcome=\"full\"}";
        let before = format!("{name} 5");
        let after = format!("{name} 7");
        assert_eq!(counter_deltas(Some(&before), Some(&after))[name], Some(2.0));
        assert_eq!(counter_deltas(Some(&after), Some(&before))[name], None);
        assert_eq!(counter_deltas(None, Some(&after))[name], None);
    }
}
