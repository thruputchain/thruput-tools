//! Groundhog JSON-RPC benchmark harness.
//!
//! Multi-worker load generator. In the default open-loop mode, each worker owns
//! its own HTTP connection pool and fires requests on a pre-computed schedule.
//! If the harness cannot sustain the target rate it fails rather than silently
//! degrading to closed-loop. `--unthrottled` instead dispatches as fast as the
//! configured worker and in-flight caps allow.
//!
//! Usage:
//!   # Against a local JSON-RPC endpoint (smoke):
//!   groundhog-bench --url http://127.0.0.1:9545 --rate 1000 --duration 5
//!
//!   # Against real gateway (load):
//!   groundhog-bench --url http://127.0.0.1:8545 --rate 100000 --duration 60 --workers 16

use std::time::{Duration, Instant};
use std::{path::Path, process, sync::Arc};

use clap::Parser;
use groundhog_rpc_api_types::runtime_manifest::{
    BootstrapAccount, RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest,
};
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount,
    SignTransactionParams, Signer, build_dev_wallet_do_action_transaction,
    derive_default_dev_account_signer, derive_dev_account_signer, encode_signed_transaction_xdr,
    sign_transaction,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;
use hdrhistogram::Histogram;
use tokio::io::AsyncReadExt;

const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
enum BenchError {
    #[error("--workers must be >= 1")]
    InvalidWorkers,
    #[error("--rate must be >= 1")]
    InvalidRate,
    #[error("--duration must be >= 1")]
    InvalidDuration,
    #[error("--accounts must be >= 1 for sendRawTransaction")]
    MissingAccounts,
    #[error("unknown method `{method}`")]
    UnknownMethod { method: String },
    #[error("failed to read runtime manifest {path}: {source}")]
    ManifestRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime manifest {path} exceeds {max_bytes} bytes")]
    ManifestTooLarge { path: String, max_bytes: u64 },
    #[error("failed to parse runtime manifest {path}: {source}")]
    ManifestParse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported runtime manifest schemaVersion {actual}")]
    UnsupportedManifestSchema { actual: u32 },
    #[error("unsupported bootstrap account derivation scheme `{scheme}`")]
    UnsupportedDerivationScheme { scheme: String },
    #[error("runtime manifest bootstrap account {index} public key does not match derivation seed")]
    BootstrapPublicKeyMismatch { index: u32 },
    #[error("runtime manifest bootstrap account index {index} does not fit this platform")]
    BootstrapAccountIndexTooLarge { index: u32 },
    #[error("runtime manifest devWallet.doActionEntrypoint is not valid hex: {source}")]
    EntrypointHex {
        #[source]
        source: hex::FromHexError,
    },
    #[error("runtime manifest devWallet.doActionEntrypoint must be 8 bytes, got {len}")]
    EntrypointLength { len: usize },
    #[error("failed to derive default dev account {index} address: {reason}")]
    DefaultAccountAddress { index: usize, reason: String },
    #[error("failed to encode wallet action calldata: {reason}")]
    WalletCalldata { reason: String },
    #[error("failed to sign benchmark transaction: {reason}")]
    SignTransaction { reason: String },
    #[error("failed to encode signed transaction: {reason}")]
    EncodeTransaction { reason: String },
    #[error("failed to build HTTP client: {source}")]
    HttpClient {
        #[source]
        source: reqwest::Error,
    },
    #[error("calibration failed: no successful responses from {url}")]
    Calibration { url: String },
    #[error("worker task failed: {source}")]
    WorkerJoin {
        #[source]
        source: tokio::task::JoinError,
    },
}

/// Groundhog JSON-RPC benchmark harness.
#[derive(Parser)]
#[command(name = "groundhog-bench")]
struct Cli {
    /// JSON-RPC server URL.
    #[arg(long, default_value = "http://127.0.0.1:9545")]
    url: String,

    /// Runtime manifest used to derive benchmark account signers.
    #[arg(long)]
    runtime_manifest: Option<std::path::PathBuf>,

    /// Target requests per second (open-loop).
    #[arg(long, default_value = "100")]
    rate: u64,

    /// Dispatch as fast as possible instead of using the open-loop rate schedule.
    #[arg(long)]
    unthrottled: bool,

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

    /// Pre-generated transaction batch size for sendRawTransaction.
    ///
    /// Defaults to the scheduled request count in rate-limited mode and 100000
    /// in unthrottled mode. Increase this if an unthrottled submit run exhausts
    /// the batch and starts reusing signed transactions.
    #[arg(long)]
    tx_batch_size: Option<usize>,

    /// Max accumulated scheduling skew before aborting (seconds).
    #[arg(long, default_value = "1.0")]
    max_skew: f64,
}

/// Derive a deterministic local-node dev account signer.
///
/// The resulting transactions are only auth-valid against endpoints whose
/// genesis bootstraps matching dev accounts.
fn dev_signer(index: usize) -> LocalSigner {
    derive_default_dev_account_signer(index)
}

fn dev_signer_from_seed(seed: &str, index: usize) -> LocalSigner {
    derive_dev_account_signer(seed, index)
}

struct BenchAccount {
    signer: LocalSigner,
    wallet_address: ContractAddress,
}

/// Pre-generate a batch of hex-encoded signed transactions.
fn generate_tx_batch(
    count: usize,
    accounts: &[BenchAccount],
    wallet_entrypoint: ContractEntrypointID,
) -> Result<Vec<String>, BenchError> {
    (0..count)
        .map(|i| {
            let idx = i % accounts.len();
            let account = &accounts[idx];
            let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
                wallet_address: account.wallet_address,
                wallet_entrypoint,
                target_address: account.wallet_address,
                target_entrypoint: wallet_entrypoint,
                target_calldata: &[],
                gas_limit: 10_000_000,
                block_expiry_height: 10_000_000,
                nonce: i as u64,
            })
            .map_err(|source| BenchError::WalletCalldata {
                reason: source.to_string(),
            })?;

            let signed = sign_transaction(tx, &account.signer, SignTransactionParams::default())
                .map_err(|source| BenchError::SignTransaction {
                    reason: source.to_string(),
                })?;
            let xdr = encode_signed_transaction_xdr(&signed).map_err(|source| {
                BenchError::EncodeTransaction {
                    reason: source.to_string(),
                }
            })?;
            Ok(format!("0x{}", hex::encode(&xdr)))
        })
        .collect()
}

fn manifest_action_entrypoint(
    manifest: &RuntimeManifest,
) -> Result<ContractEntrypointID, BenchError> {
    let value = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint
        .strip_prefix("0x")
        .unwrap_or(&manifest.bootstrap.dev_wallet.do_action_entrypoint);
    let bytes = hex::decode(value).map_err(|source| BenchError::EntrypointHex { source })?;
    let len = bytes.len();
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| BenchError::EntrypointLength { len })?;
    Ok(ContractEntrypointID::from(bytes))
}

fn default_bench_accounts(num_accounts: usize) -> Result<Vec<BenchAccount>, BenchError> {
    (0..num_accounts)
        .map(|index| {
            let signer = dev_signer(index);
            let auth_account =
                PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE);
            let wallet_address =
                auth_account
                    .address()
                    .map_err(|source| BenchError::DefaultAccountAddress {
                        index,
                        reason: source.to_string(),
                    })?;
            Ok(BenchAccount {
                signer,
                wallet_address,
            })
        })
        .collect()
}

fn manifest_bench_accounts(manifest: &RuntimeManifest) -> Result<Vec<BenchAccount>, BenchError> {
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(BenchError::UnsupportedDerivationScheme {
            scheme: manifest.bootstrap_account_derivation.scheme.clone(),
        });
    }

    manifest
        .bootstrap_accounts
        .iter()
        .map(|account| manifest_bench_account(manifest, account))
        .collect()
}

fn manifest_bench_account(
    manifest: &RuntimeManifest,
    account: &BootstrapAccount,
) -> Result<BenchAccount, BenchError> {
    let index =
        usize::try_from(account.index).map_err(|_| BenchError::BootstrapAccountIndexTooLarge {
            index: account.index,
        })?;
    let signer = dev_signer_from_seed(&manifest.bootstrap_account_derivation.seed, index);
    let expected_public_key = signer.verifying_key_bytes();
    if account.public_key.as_ref() != expected_public_key.as_slice() {
        return Err(BenchError::BootstrapPublicKeyMismatch {
            index: account.index,
        });
    }
    Ok(BenchAccount {
        signer,
        wallet_address: account.wallet_address.0,
    })
}

fn transaction_inputs_for_method(
    method: &str,
    manifest: Option<&RuntimeManifest>,
    num_accounts: usize,
) -> Result<(Vec<BenchAccount>, ContractEntrypointID), BenchError> {
    if method != "sendRawTransaction" {
        return Ok((Vec::new(), ContractEntrypointID::from([0u8; 8])));
    }

    let accounts = match manifest {
        Some(manifest) => manifest_bench_accounts(manifest)?,
        None => default_bench_accounts(num_accounts)?,
    };
    let entrypoint = match manifest {
        Some(manifest) => manifest_action_entrypoint(manifest)?,
        None => ContractEntrypointID::from([0u8; 8]),
    };
    if accounts.is_empty() {
        return Err(BenchError::MissingAccounts);
    }
    Ok((accounts, entrypoint))
}

async fn load_runtime_manifest(path: &Path) -> Result<RuntimeManifest, BenchError> {
    let path_display = path.display().to_string();
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|source| BenchError::ManifestRead {
            path: path_display.clone(),
            source,
        })?;
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_RUNTIME_MANIFEST_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| BenchError::ManifestRead {
            path: path_display.clone(),
            source,
        })?;
    if bytes.len() as u64 > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(BenchError::ManifestTooLarge {
            path: path_display,
            max_bytes: MAX_RUNTIME_MANIFEST_BYTES,
        });
    }
    let manifest: RuntimeManifest =
        serde_json::from_slice(&bytes).map_err(|source| BenchError::ManifestParse {
            path: path_display,
            source,
        })?;
    if manifest.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
        return Err(BenchError::UnsupportedManifestSchema {
            actual: manifest.schema_version,
        });
    }
    Ok(manifest)
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
        _ => unreachable!("bug: method validated before benchmark starts"),
    }
}

fn validate_method(method: &str) -> Result<(), BenchError> {
    match method {
        "sendRawTransaction" | "chainTip" | "getTransactionStatus" => Ok(()),
        method => Err(BenchError::UnknownMethod {
            method: method.to_string(),
        }),
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
async fn calibrate_rtt(client: &reqwest::Client, url: &str) -> Result<Duration, BenchError> {
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
        return Err(BenchError::Calibration {
            url: url.to_string(),
        });
    }

    rtts.sort();
    // Use p50 as the representative RTT
    Ok(rtts[rtts.len() / 2])
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
    unthrottled: bool,
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
    if cfg.unthrottled {
        return run_unthrottled_worker(worker_id, cfg, client, inflight_sem).await;
    }

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
    let mut errors = 0u64;
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

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

async fn run() -> Result<(), BenchError> {
    let cli = Cli::parse();

    if cli.workers == 0 {
        return Err(BenchError::InvalidWorkers);
    }
    if !cli.unthrottled && cli.rate == 0 {
        return Err(BenchError::InvalidRate);
    }
    if cli.duration == 0 {
        return Err(BenchError::InvalidDuration);
    }
    validate_method(&cli.method)?;

    let manifest = match &cli.runtime_manifest {
        Some(path) => Some(load_runtime_manifest(path).await?),
        None => None,
    };
    let needs_transactions = cli.method == "sendRawTransaction";
    let (accounts, entrypoint) =
        transaction_inputs_for_method(&cli.method, manifest.as_ref(), cli.accounts)?;

    let total_duration = Duration::from_secs(cli.duration);
    let warmup_duration = Duration::from_secs(cli.warmup);
    let total_run_duration = total_duration + warmup_duration;
    let interval = if cli.unthrottled {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(1.0 / cli.rate as f64)
    };
    let max_skew = Duration::from_secs_f64(cli.max_skew);

    // --- Transaction pre-generation ---
    let tx_batch = if needs_transactions {
        eprintln!("Generating transactions...");
        let batch_size = cli.tx_batch_size.unwrap_or_else(|| {
            if cli.unthrottled {
                100_000
            } else {
                (cli.rate as usize) * (cli.duration as usize + cli.warmup as usize + 1)
            }
        });
        let batch = generate_tx_batch(batch_size, &accounts, entrypoint)?;
        eprintln!("Generated {} transactions", batch.len());
        Arc::new(batch)
    } else {
        Arc::new(vec![])
    };

    // --- Calibrate RTT to size connection pools ---
    let probe_client = reqwest::Client::builder()
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|source| BenchError::HttpClient { source })?;

    eprintln!("Calibrating RTT (via chainTip)...");
    let median_rtt = calibrate_rtt(&probe_client, &cli.url).await?;
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
    } else if cli.unthrottled {
        256
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
    if cli.unthrottled {
        eprintln!(
            "Benchmarking {} unthrottled for {}s ({}s warmup)...",
            cli.method, cli.duration, cli.warmup
        );
    } else {
        eprintln!(
            "Benchmarking {} at {} req/s for {}s ({}s warmup)...",
            cli.method, cli.rate, cli.duration, cli.warmup
        );
    }

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
        unthrottled: cli.unthrottled,
    });

    let mut handles = Vec::with_capacity(cli.workers);

    for worker_id in 0..cli.workers {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(connections_per_worker)
            .build()
            .map_err(|source| BenchError::HttpClient { source })?;

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
        let result = handle
            .await
            .map_err(|source| BenchError::WorkerJoin { source })?;
        merged_hist.add(&result.histogram).ok();
        total_successes += result.successes;
        total_errors += result.errors;
    }

    let total_sent = total_successes + total_errors;
    let actual_duration = total_duration.as_secs_f64();

    println!("\n=== Groundhog RPC Benchmark Results ===");
    println!("Method:      {}", cli.method);
    if cli.unthrottled {
        println!("Target:      unthrottled");
        println!(
            "Attempted:   {:.0} req/s",
            total_sent as f64 / actual_duration
        );
        println!(
            "Achieved:    {:.0} successful req/s",
            total_successes as f64 / actual_duration
        );
    } else {
        println!("Target:      {} req/s", cli.rate);
        println!(
            "Achieved:    {:.0} req/s ({:.1}% of target)",
            total_sent as f64 / actual_duration,
            (total_sent as f64 / actual_duration) / cli.rate as f64 * 100.0,
        );
    }
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use groundhog_rpc_api_types::{hex::Hex0x, runtime_manifest::RuntimeManifest};
    use groundhog_xdr::types::{
        ContractAddress as XdrContractAddress, Ed25519PublicKey as XdrEd25519PublicKey, SHA256Hash,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn read_only_manifest_does_not_validate_bootstrap_accounts() {
        let mut manifest = manifest();
        manifest.bootstrap_account_derivation.scheme = "future-scheme".into();
        manifest.bootstrap_accounts.clear();

        let (accounts, entrypoint) = transaction_inputs_for_method("chainTip", Some(&manifest), 0)
            .expect("bug: read-only manifest should not validate accounts");

        assert!(accounts.is_empty());
        assert_eq!(entrypoint, ContractEntrypointID::from([0u8; 8]));
    }

    #[test]
    fn send_raw_transaction_rejects_unsupported_manifest_derivation_scheme() {
        let mut manifest = manifest();
        manifest.bootstrap_account_derivation.scheme = "future-scheme".into();

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 0) {
            Ok(_) => panic!("bug: unsupported derivation scheme should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            err,
            BenchError::UnsupportedDerivationScheme { .. }
        ));
    }

    #[test]
    fn send_raw_transaction_rejects_public_key_mismatch() {
        let mut manifest = manifest();
        manifest.bootstrap_accounts[0].public_key = Hex0x(XdrEd25519PublicKey::from([0x99; 32]));

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 0) {
            Ok(_) => panic!("bug: public key mismatch should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            err,
            BenchError::BootstrapPublicKeyMismatch { index: 0 }
        ));
    }

    fn manifest() -> RuntimeManifest {
        let seed = "groundhog-dev-accounts";
        let signer = derive_dev_account_signer(seed, 0);
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&signer.verifying_key_bytes());
        serde_json::from_value(json!({
            "schemaVersion": RUNTIME_MANIFEST_SCHEMA_VERSION,
            "kind": "hogd-dev-runtime",
            "networkId": "groundhog-local",
            "protocol": "hogp/test",
            "genesisHash": hash(0x42),
            "producer": {
                "binary": "hogd-dev",
                "version": "0.1.0",
                "commitHash": "abcdef0",
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
            },
            "bootstrapAccountDerivation": {
                "scheme": SUPPORTED_DERIVATION_SCHEME,
                "seed": seed,
            },
            "bootstrapAccounts": [{
                "index": 0,
                "role": "local_user",
                "publicKey": Hex0x(XdrEd25519PublicKey::from(public_key)),
                "walletAddress": Hex0x(XdrContractAddress::from([0x71; 32])),
                "initialStorageTokens": "1000000000",
            }],
            "bootstrap": {
                "devWallet": {
                    "contentHash": hash(0x51),
                    "deployedCodeHash": hash(0x52),
                    "artifactHint": "./dev-wallet.wasm",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "doActionEntrypoint": "0x646f416374696f6e",
                },
                "storageCostModel": {
                    "hash": hash(0x53),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234",
                },
            },
        }))
        .expect("bug: manifest JSON should parse")
    }

    fn hash(byte: u8) -> Hex0x<SHA256Hash> {
        Hex0x(SHA256Hash::from([byte; 32]))
    }
}
