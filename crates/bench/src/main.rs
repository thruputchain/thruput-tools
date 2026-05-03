//! Groundhog JSON-RPC benchmark harness.
//!
//! Multi-worker open-loop load generator. Each worker owns its own HTTP
//! connection pool and fires requests on a pre-computed schedule.  If
//! the harness cannot sustain the target rate it panics rather than
//! silently degrading to closed-loop.
//!
//! Usage:
//!   # Against dev-node (smoke):
//!   groundhog-bench --url http://127.0.0.1:9545 --rate 1000 --duration 5
//!
//!   # Against real gateway (load):
//!   groundhog-bench --url http://127.0.0.1:8545 --rate 100000 --duration 60 --workers 16

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use groundhog_signing::{
    BuildCallTransaction, DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount,
    SignTransactionParams, Signer, build_call_transaction, encode_signed_transaction_xdr,
    sign_transaction,
};
use groundhog_xdr::contract::ContractEntrypointID;
use hdrhistogram::Histogram;
use sha2::{Digest, Sha256};

/// Groundhog JSON-RPC benchmark harness.
#[derive(Parser)]
#[command(name = "groundhog-bench")]
struct Cli {
    /// JSON-RPC server URL.
    #[arg(long, default_value = "http://127.0.0.1:9545")]
    url: String,

    /// Target requests per second (open-loop).
    #[arg(long, default_value = "100")]
    rate: u64,

    /// Duration in seconds.
    #[arg(long, default_value = "10")]
    duration: u64,

    /// Number of accounts to cycle through.
    #[arg(long, default_value = "10")]
    accounts: usize,

    /// Method to benchmark.
    #[arg(long, default_value = "sendRawTransaction")]
    method: String,

    /// Warmup seconds (results discarded).
    #[arg(long, default_value = "2")]
    warmup: u64,

    /// Number of worker tasks (each with its own connection pool).
    #[arg(long, default_value = "1")]
    workers: usize,

    /// Max idle connections per worker. Auto-sized from calibration if 0.
    #[arg(long, default_value = "0")]
    connections_per_worker: usize,

    /// Max accumulated scheduling skew before aborting (seconds).
    #[arg(long, default_value = "1.0")]
    max_skew: f64,
}

/// Derive a dev-node account signer (matches dev-node's derivation).
fn dev_signer(index: usize) -> LocalSigner {
    let mut hasher = Sha256::new();
    hasher.update(b"groundhog-dev-accounts:");
    hasher.update(format!("account-{index}").as_bytes());
    let secret: [u8; 32] = hasher.finalize().into();
    LocalSigner::from_secret_key(secret)
}

/// Pre-generate a batch of hex-encoded signed transactions.
fn generate_tx_batch(count: usize, num_accounts: usize) -> Vec<String> {
    let signers: Vec<_> = (0..num_accounts).map(dev_signer).collect();
    let accounts: Vec<_> = signers
        .iter()
        .map(|s| PubkeyAccount::new(s.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE))
        .collect();
    let addresses: Vec<_> = accounts
        .iter()
        .map(|a| a.address().expect("bug: address derivation"))
        .collect();

    (0..count)
        .map(|i| {
            let idx = i % num_accounts;
            let tx = build_call_transaction(BuildCallTransaction {
                invoked_address: addresses[idx],
                entrypoint: ContractEntrypointID::from([0u8; 8]),
                calldata: vec![],
                gas_limit: 10_000_000,
                block_expiry_height: 10_000_000,
                nonce: i as u64,
                auth_claim: accounts[idx].auth_claim(),
            });

            let signed = sign_transaction(tx, &signers[idx], SignTransactionParams::default())
                .expect("bug: sign");
            let xdr = encode_signed_transaction_xdr(&signed).expect("bug: encode");
            format!("0x{}", hex::encode(&xdr))
        })
        .collect()
}

/// Build a JSON-RPC request body.
fn make_request(method: &str, tx_hex: &str, id: u64) -> serde_json::Value {
    match method {
        "sendRawTransaction" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_sendRawTransaction",
            "params": [tx_hex]
        }),
        "chainTip" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_chainTip",
            "params": []
        }),
        "getTransactionStatus" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "groundhog_getTransactionStatus",
            "params": ["0x0000000000000000000000000000000000000000000000000000000000000000"]
        }),
        _ => panic!("unknown method: {method}"),
    }
}

/// Calibrate connection pool size by measuring RTT with a small burst.
///
/// Always probes with `chainTip` — it's cheap, read-only, always valid,
/// and avoids the trap of sending a dummy empty payload through
/// `sendRawTransaction`'s validator. The result is only a rough sizing
/// hint for Little's Law; server-side processing time on the real
/// benchmark method may be larger, which is why the caller inflates
/// the connection count before clamping. An HTTP/2 origin (or a
/// reverse proxy fronting the gateway) would obsolete this heuristic
/// entirely by making per-connection serialization a non-issue.
async fn calibrate_rtt(client: &reqwest::Client, url: &str) -> Duration {
    let body = make_request("chainTip", "", 0);
    let mut rtts = Vec::with_capacity(20);

    for _ in 0..20 {
        let start = Instant::now();
        let result = client.post(url).json(&body).send().await;
        let rtt = start.elapsed();
        if let Ok(resp) = result
            && resp.status().is_success()
        {
            rtts.push(rtt);
        }
    }

    if rtts.is_empty() {
        panic!("calibration failed: no successful responses from {url}");
    }

    rtts.sort();
    // Use p50 as the representative RTT
    rtts[rtts.len() / 2]
}

/// Per-worker state. Each worker owns its own histogram to avoid lock
/// contention on the hot path.
struct WorkerResult {
    histogram: Histogram<u64>,
    successes: u64,
    errors: u64,
}

/// Parameters shared across all workers.
struct WorkerConfig {
    url: String,
    method: String,
    tx_batch: Arc<Vec<String>>,
    start: Instant,
    warmup_duration: Duration,
    total_run_duration: Duration,
    interval: Duration,
    max_skew: Duration,
    num_workers: usize,
}

/// Latency sample sent from a spawned request task back to its worker.
enum Sample {
    Success(u64), // latency in microseconds
    Error,
}

/// Fold a sample into the per-worker counters.
fn record_sample(
    sample: Sample,
    histogram: &mut Histogram<u64>,
    successes: &mut u64,
    errors: &mut u64,
) {
    match sample {
        Sample::Success(us) => {
            histogram.record(us).ok();
            *successes += 1;
        }
        Sample::Error => *errors += 1,
    }
}

/// A single worker task. Fires requests on its slice of the global
/// schedule and panics if it falls behind by more than `max_skew`.
async fn run_worker(
    worker_id: usize,
    cfg: &WorkerConfig,
    client: reqwest::Client,
    inflight_sem: Arc<tokio::sync::Semaphore>,
) -> WorkerResult {
    let mut histogram =
        Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).expect("bug: histogram");
    let mut successes = 0u64;
    let mut errors = 0u64;

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

        tokio::spawn(async move {
            let req_start = Instant::now();
            let result = client.post(&url).json(&body).send().await;
            let latency_us = req_start.elapsed().as_micros() as u64;
            drop(permit);

            if in_warmup {
                return;
            }

            let sample = match result {
                Ok(resp) if resp.status().is_success() => Sample::Success(latency_us),
                _ => Sample::Error,
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    assert!(cli.workers > 0, "--workers must be >= 1");
    assert!(cli.rate > 0, "--rate must be >= 1");
    assert!(cli.duration > 0, "--duration must be >= 1");

    let total_duration = Duration::from_secs(cli.duration);
    let warmup_duration = Duration::from_secs(cli.warmup);
    let total_run_duration = total_duration + warmup_duration;
    let interval = Duration::from_secs_f64(1.0 / cli.rate as f64);
    let max_skew = Duration::from_secs_f64(cli.max_skew);

    // --- Transaction pre-generation ---
    let tx_batch = if cli.method == "sendRawTransaction" {
        eprintln!("Generating transactions...");
        let batch = generate_tx_batch(
            (cli.rate as usize) * (cli.duration as usize + cli.warmup as usize + 1),
            cli.accounts,
        );
        eprintln!("Generated {} transactions", batch.len());
        Arc::new(batch)
    } else {
        Arc::new(vec![])
    };

    // --- Calibrate RTT to size connection pools ---
    let probe_client = reqwest::Client::builder()
        .pool_max_idle_per_host(4)
        .build()
        .expect("bug: reqwest client");

    eprintln!("Calibrating RTT (via chainTip)...");
    let median_rtt = calibrate_rtt(&probe_client, &cli.url).await;
    eprintln!("Median RTT: {median_rtt:.1?}");
    drop(probe_client);

    // Little's Law: connections = rate * rtt / workers. We calibrate
    // with chainTip, which is cheaper than the benchmark method, so
    // the naive result undersizes the pool (HTTP/1.1 serializes one
    // request per connection, and the real method's server-side
    // processing adds latency the calibration doesn't see). Inflate
    // by 4x to cover that gap; floor of 16 prevents pathological
    // undersizing at very low rates.
    let connections_per_worker = if cli.connections_per_worker > 0 {
        cli.connections_per_worker
    } else {
        let per_worker_rate = cli.rate as f64 / cli.workers as f64;
        let needed = (per_worker_rate * median_rtt.as_secs_f64() * 4.0).ceil() as usize;
        needed.clamp(16, 2048)
    };

    // Bound total in-flight requests to avoid OOM. Each worker gets its
    // own semaphore with capacity = 2x its connection pool (allow some
    // queuing inside reqwest but not unbounded growth).
    let max_inflight_per_worker = connections_per_worker * 2;

    eprintln!(
        "Config: {} workers x {} connections ({} max in-flight each)",
        cli.workers, connections_per_worker, max_inflight_per_worker,
    );
    eprintln!(
        "Benchmarking {} at {} req/s for {}s ({}s warmup)...",
        cli.method, cli.rate, cli.duration, cli.warmup
    );

    // --- Launch workers ---
    let start = Instant::now();
    let cfg = Arc::new(WorkerConfig {
        url: cli.url.clone(),
        method: cli.method.clone(),
        tx_batch,
        start,
        warmup_duration,
        total_run_duration,
        interval,
        max_skew,
        num_workers: cli.workers,
    });

    let mut handles = Vec::with_capacity(cli.workers);

    for worker_id in 0..cli.workers {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(connections_per_worker)
            .build()
            .expect("bug: reqwest client");

        let sem = Arc::new(tokio::sync::Semaphore::new(max_inflight_per_worker));
        let cfg = cfg.clone();

        handles.push(tokio::spawn(async move {
            run_worker(worker_id, &cfg, client, sem).await
        }));
    }

    // --- Collect results ---
    let mut merged_hist =
        Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).expect("bug: histogram");
    let mut total_successes = 0u64;
    let mut total_errors = 0u64;

    for handle in handles {
        let result = handle.await.expect("worker panicked");
        merged_hist.add(&result.histogram).ok();
        total_successes += result.successes;
        total_errors += result.errors;
    }

    let total_sent = total_successes + total_errors;
    let actual_duration = total_duration.as_secs_f64();

    println!("\n=== Groundhog RPC Benchmark Results ===");
    println!("Method:      {}", cli.method);
    println!("Target:      {} req/s", cli.rate);
    println!(
        "Achieved:    {:.0} req/s ({:.1}% of target)",
        total_sent as f64 / actual_duration,
        (total_sent as f64 / actual_duration) / cli.rate as f64 * 100.0,
    );
    println!("Duration:    {}s ({}s warmup)", cli.duration, cli.warmup);
    println!(
        "Workers:     {} x {} connections",
        cli.workers, connections_per_worker,
    );
    println!(
        "Requests:    {} ({} errors, {:.1}%)",
        total_sent,
        total_errors,
        if total_sent > 0 {
            total_errors as f64 / total_sent as f64 * 100.0
        } else {
            0.0
        },
    );
    println!(
        "Throughput:  {:.0} successful req/s",
        total_successes as f64 / actual_duration,
    );
    println!("Median RTT:  {median_rtt:.1?} (from calibration)");
    println!();
    println!("Latency (microseconds):");
    if merged_hist.is_empty() {
        println!("  (no samples recorded)");
    } else {
        println!("  p50:  {}", merged_hist.value_at_percentile(50.0));
        println!("  p90:  {}", merged_hist.value_at_percentile(90.0));
        println!("  p95:  {}", merged_hist.value_at_percentile(95.0));
        println!("  p99:  {}", merged_hist.value_at_percentile(99.0));
        println!("  p999: {}", merged_hist.value_at_percentile(99.9));
        println!("  max:  {}", merged_hist.max());
        println!("  mean: {:.0}", merged_hist.mean());
    }
}
