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
//!   groundhog-bench --url http://127.0.0.1:9545 --method chainTip --rate 1000 --duration 5
//!
//!   # Against real gateway (load):
//!   groundhog-bench --url http://127.0.0.1:8545 --runtime-manifest /path/to/runtime-manifest.json --rate 100000 --duration 60 --workers 16

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{path::Path, process, sync::Arc};

use clap::Parser;
#[cfg(test)]
use groundhog_json_rpc_types::runtime_manifest::RUNTIME_MANIFEST_SCHEMA_VERSION;
use groundhog_json_rpc_types::runtime_manifest::{
    BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, BootstrapAccount, RuntimeManifest,
    RuntimeManifestEntrypointError, RuntimeManifestParseError, parse_runtime_manifest,
};
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, LocalSigner, SignTransactionParams, SigningError,
    build_dev_wallet_do_action_transaction, derive_dev_account_signer,
    sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;
use hdrhistogram::Histogram;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;
const BENCH_RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestOutcome {
    Success,
    Error(RequestErrorKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestErrorKind {
    Transport,
    HttpStatus,
    InvalidJsonRpcResponse,
    JsonRpc,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestErrorCounts {
    transport: u64,
    http_status: u64,
    invalid_json_rpc_response: u64,
    json_rpc: u64,
}

impl RequestErrorCounts {
    fn record(&mut self, kind: RequestErrorKind) {
        match kind {
            RequestErrorKind::Transport => self.transport += 1,
            RequestErrorKind::HttpStatus => self.http_status += 1,
            RequestErrorKind::InvalidJsonRpcResponse => self.invalid_json_rpc_response += 1,
            RequestErrorKind::JsonRpc => self.json_rpc += 1,
        }
    }

    fn merge(&mut self, other: Self) {
        self.transport += other.transport;
        self.http_status += other.http_status;
        self.invalid_json_rpc_response += other.invalid_json_rpc_response;
        self.json_rpc += other.json_rpc;
    }

    fn total(self) -> u64 {
        self.transport + self.http_status + self.invalid_json_rpc_response + self.json_rpc
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchResult {
    schema_version: u32,
    kind: &'static str,
    generated_at_unix_ms: u128,
    producer: ProducerMetadata,
    platform: PlatformMetadata,
    runtime: Option<RuntimeIdentity>,
    configuration: BenchConfiguration,
    calibration: CalibrationMetrics,
    requests: RequestMetrics,
    rates: RateMetrics,
    successful_latency_us: Option<LatencyMetrics>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProducerMetadata {
    name: &'static str,
    version: &'static str,
    build_profile: &'static str,
    git_commit: Option<String>,
    git_dirty: Option<bool>,
}

#[derive(Debug, serde::Serialize)]
struct PlatformMetadata {
    os: &'static str,
    arch: &'static str,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeIdentity {
    manifest_sha256: String,
    manifest_schema_version: u32,
    kind: String,
    chain_state: Option<String>,
    network_hash: String,
    protocol: String,
    genesis_hash: String,
    producer: RuntimeProducerIdentity,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeProducerIdentity {
    binary: String,
    version: String,
    commit_hash: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchConfiguration {
    scenario: Option<String>,
    target_label: Option<String>,
    url: String,
    method: String,
    mode: &'static str,
    target_requests_per_second: Option<u64>,
    duration_seconds: u64,
    warmup_seconds: u64,
    workers: usize,
    connections_per_worker: usize,
    max_inflight_per_worker: usize,
    accounts: usize,
    transaction_batch_size: usize,
    max_skew_seconds: f64,
    runtime_manifest: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CalibrationMetrics {
    median_rtt_us: u64,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestMetrics {
    total: u64,
    successful: u64,
    failed: u64,
    errors: RequestErrorCounts,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RateMetrics {
    attempted_requests_per_second: f64,
    successful_requests_per_second: f64,
    target_achievement_percent: Option<f64>,
    run_wall_time_ms: f64,
}

#[derive(Debug, serde::Serialize)]
struct LatencyMetrics {
    samples: u64,
    min: u64,
    p50: u64,
    p90: u64,
    p95: u64,
    p99: u64,
    p999: u64,
    max: u64,
    mean: f64,
}

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
    #[error(
        "--accounts requested {requested} accounts, but the runtime manifest only contains {available}"
    )]
    NotEnoughAccounts { requested: usize, available: usize },
    #[error("--runtime-manifest is required for sendRawTransaction")]
    MissingRuntimeManifest,
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
    #[error("failed to parse runtime manifest devWallet.doActionEntrypoint: {source}")]
    Entrypoint {
        #[source]
        source: RuntimeManifestEntrypointError,
    },
    #[error("failed to encode wallet action calldata: {source}")]
    WalletCalldata {
        #[source]
        source: SigningError,
    },
    #[error("failed to sign and encode benchmark transaction: {source}")]
    SignAndEncodeTransaction {
        #[source]
        source: SigningError,
    },
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
#[command(name = "groundhog-bench", version)]
struct Cli {
    /// JSON-RPC server URL.
    #[arg(long, default_value = "http://127.0.0.1:9545")]
    url: String,

    /// Runtime manifest used to derive benchmark account signers. Required for sendRawTransaction.
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

    /// Stable name for the enclosing scenario, when run by an orchestrator.
    #[arg(long)]
    scenario: Option<String>,

    /// Human-readable identity for the endpoint under test.
    #[arg(long)]
    target_label: Option<String>,

    /// Emit a versioned machine-readable result on stdout.
    #[arg(long)]
    json: bool,
}

fn dev_signer_from_seed(seed: &str, index: usize) -> LocalSigner {
    derive_dev_account_signer(seed, index)
}

struct BenchAccount {
    signer: LocalSigner,
    wallet_address: ContractAddress,
}

struct LoadedRuntimeManifest {
    manifest: RuntimeManifest,
    sha256: String,
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
                target_gas_limit: 10_000_000,
                block_expiry_height: 10_000_000,
                nonce: i as u64,
            })
            .map_err(|source| BenchError::WalletCalldata { source })?;

            sign_and_encode_transaction_hex(tx, &account.signer, SignTransactionParams::default())
                .map_err(|source| BenchError::SignAndEncodeTransaction { source })
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
        .filter(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
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
    if num_accounts == 0 {
        return Err(BenchError::MissingAccounts);
    }

    let manifest = manifest.ok_or(BenchError::MissingRuntimeManifest)?;
    let mut accounts = manifest_bench_accounts(manifest)?;
    if accounts.len() < num_accounts {
        return Err(BenchError::NotEnoughAccounts {
            requested: num_accounts,
            available: accounts.len(),
        });
    }
    accounts.truncate(num_accounts);
    let entrypoint = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|source| BenchError::Entrypoint { source })?;
    if accounts.is_empty() {
        return Err(BenchError::MissingAccounts);
    }
    Ok((accounts, entrypoint))
}

async fn load_runtime_manifest(path: &Path) -> Result<LoadedRuntimeManifest, BenchError> {
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
    let manifest = parse_runtime_manifest(&bytes).map_err(|error| match error {
        RuntimeManifestParseError::Json(source) => BenchError::ManifestParse {
            path: path_display,
            source,
        },
        RuntimeManifestParseError::UnsupportedSchema { actual, .. } => {
            BenchError::UnsupportedManifestSchema { actual }
        }
    })?;
    Ok(LoadedRuntimeManifest {
        manifest,
        sha256: format!("0x{}", hex::encode(Sha256::digest(&bytes))),
    })
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

fn classify_json_rpc_value(value: &serde_json::Value, expected_id: u64) -> RequestOutcome {
    let Some(object) = value.as_object() else {
        return RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse);
    };
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || object.get("id").and_then(serde_json::Value::as_u64) != Some(expected_id)
    {
        return RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse);
    }
    match (object.get("result"), object.get("error")) {
        (Some(_), None) => RequestOutcome::Success,
        (None, Some(serde_json::Value::Object(error)))
            if error
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .is_some()
                && error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .is_some() =>
        {
            RequestOutcome::Error(RequestErrorKind::JsonRpc)
        }
        _ => RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse),
    }
}

async fn classify_response(
    response: Result<reqwest::Response, reqwest::Error>,
    expected_id: u64,
) -> RequestOutcome {
    let response = match response {
        Ok(response) => response,
        Err(_) => return RequestOutcome::Error(RequestErrorKind::Transport),
    };
    let status = response.status();
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(_) => return RequestOutcome::Error(RequestErrorKind::Transport),
    };
    if !status.is_success() {
        return RequestOutcome::Error(RequestErrorKind::HttpStatus);
    }
    match serde_json::from_slice(&body) {
        Ok(value) => classify_json_rpc_value(&value, expected_id),
        Err(_) => RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse),
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
        if classify_response(result, 0).await == RequestOutcome::Success {
            rtts.push(start.elapsed());
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
    errors: RequestErrorCounts,
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

        tokio::spawn(async move {
            let req_start = Instant::now();
            let result = client.post(&url).json(&body).send().await;
            let outcome = classify_response(result, tick).await;
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

        tokio::spawn(async move {
            let req_start = Instant::now();
            let result = client.post(&url).json(&body).send().await;
            let outcome = classify_response(result, tick).await;
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

fn latency_metrics(histogram: &Histogram<u64>) -> Option<LatencyMetrics> {
    if histogram.is_empty() {
        return None;
    }
    Some(LatencyMetrics {
        samples: histogram.len(),
        min: histogram.min(),
        p50: histogram.value_at_percentile(50.0),
        p90: histogram.value_at_percentile(90.0),
        p95: histogram.value_at_percentile(95.0),
        p99: histogram.value_at_percentile(99.0),
        p999: histogram.value_at_percentile(99.9),
        max: histogram.max(),
        mean: histogram.mean(),
    })
}

fn benchmark_git_commit() -> Option<String> {
    std::env::var("GROUNDHOG_BENCH_GIT_COMMIT")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| option_env!("GIT_COMMIT").map(str::to_owned))
}

fn benchmark_git_dirty() -> Option<bool> {
    match std::env::var("GROUNDHOG_BENCH_GIT_DIRTY").as_deref() {
        Ok("true") => Some(true),
        Ok("false") => Some(false),
        _ => None,
    }
}

fn print_human_result(result: &BenchResult) {
    let config = &result.configuration;
    let requests = &result.requests;
    let rates = &result.rates;

    println!("\n=== Groundhog RPC Benchmark Results ===");
    println!("Method:      {}", config.method);
    if let Some(target) = config.target_requests_per_second {
        println!("Target:      {target} req/s");
        println!(
            "Achieved:    {:.0} req/s ({:.1}% of target)",
            rates.attempted_requests_per_second,
            rates.target_achievement_percent.unwrap_or_default(),
        );
    } else {
        println!("Target:      unthrottled");
        println!(
            "Attempted:   {:.0} req/s",
            rates.attempted_requests_per_second
        );
        println!(
            "Achieved:    {:.0} successful req/s",
            rates.successful_requests_per_second
        );
    }
    println!(
        "Duration:    {}s ({}s warmup)",
        config.duration_seconds, config.warmup_seconds
    );
    println!(
        "Workers:     {} x {} connections",
        config.workers, config.connections_per_worker,
    );
    println!(
        "Requests:    {} ({} errors, {:.1}%)",
        requests.total,
        requests.failed,
        if requests.total > 0 {
            requests.failed as f64 / requests.total as f64 * 100.0
        } else {
            0.0
        },
    );
    if requests.failed > 0 {
        println!(
            "Errors:      transport={} http={} invalid-json-rpc={} json-rpc={}",
            requests.errors.transport,
            requests.errors.http_status,
            requests.errors.invalid_json_rpc_response,
            requests.errors.json_rpc,
        );
    }
    println!(
        "Throughput:  {:.0} successful req/s",
        rates.successful_requests_per_second
    );
    println!(
        "Median RTT:  {} us (from calibration)",
        result.calibration.median_rtt_us
    );
    println!();
    println!("Latency (microseconds):");
    if let Some(latency) = &result.successful_latency_us {
        println!("  p50:  {}", latency.p50);
        println!("  p90:  {}", latency.p90);
        println!("  p95:  {}", latency.p95);
        println!("  p99:  {}", latency.p99);
        println!("  p999: {}", latency.p999);
        println!("  max:  {}", latency.max);
        println!("  mean: {:.0}", latency.mean);
    } else {
        println!("  (no samples recorded)");
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

    let loaded_manifest = match &cli.runtime_manifest {
        Some(path) => Some(load_runtime_manifest(path).await?),
        None => None,
    };
    let manifest = loaded_manifest.as_ref().map(|loaded| &loaded.manifest);
    let needs_transactions = cli.method == "sendRawTransaction";
    let (accounts, entrypoint) =
        transaction_inputs_for_method(&cli.method, manifest, cli.accounts)?;

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
    let transaction_batch_size = tx_batch.len();

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
    let mut errors = RequestErrorCounts::default();

    for handle in handles {
        let result = handle
            .await
            .map_err(|source| BenchError::WorkerJoin { source })?;
        merged_hist.add(&result.histogram).ok();
        total_successes += result.successes;
        errors.merge(result.errors);
    }

    let total_errors = errors.total();
    let total_sent = total_successes + total_errors;
    let actual_duration = total_duration.as_secs_f64();
    let attempted_requests_per_second = total_sent as f64 / actual_duration;
    let result = BenchResult {
        schema_version: BENCH_RESULT_SCHEMA_VERSION,
        kind: "groundhog-bench-result",
        generated_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        producer: ProducerMetadata {
            name: "groundhog-bench",
            version: env!("CARGO_PKG_VERSION"),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            git_commit: benchmark_git_commit(),
            git_dirty: benchmark_git_dirty(),
        },
        platform: PlatformMetadata {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        },
        runtime: loaded_manifest.as_ref().map(|loaded| RuntimeIdentity {
            manifest_sha256: loaded.sha256.clone(),
            manifest_schema_version: loaded.manifest.schema_version,
            kind: loaded.manifest.kind.clone(),
            chain_state: loaded
                .manifest
                .chain_state
                .map(|chain_state| chain_state.to_string()),
            network_hash: loaded.manifest.network_hash.to_string(),
            protocol: loaded.manifest.protocol.clone(),
            genesis_hash: format!("0x{}", hex::encode(loaded.manifest.genesis_hash.as_ref())),
            producer: RuntimeProducerIdentity {
                binary: loaded.manifest.producer.binary.clone(),
                version: loaded.manifest.producer.version.clone(),
                commit_hash: loaded.manifest.producer.commit_hash.clone(),
            },
        }),
        configuration: BenchConfiguration {
            scenario: cli.scenario,
            target_label: cli.target_label,
            url: cli.url,
            method: cli.method,
            mode: if cli.unthrottled {
                "unthrottled"
            } else {
                "rate_limited"
            },
            target_requests_per_second: (!cli.unthrottled).then_some(cli.rate),
            duration_seconds: cli.duration,
            warmup_seconds: cli.warmup,
            workers: cli.workers,
            connections_per_worker,
            max_inflight_per_worker,
            accounts: cli.accounts,
            transaction_batch_size,
            max_skew_seconds: cli.max_skew,
            runtime_manifest: cli.runtime_manifest.map(|path| path.display().to_string()),
        },
        calibration: CalibrationMetrics {
            median_rtt_us: median_rtt.as_micros() as u64,
        },
        requests: RequestMetrics {
            total: total_sent,
            successful: total_successes,
            failed: total_errors,
            errors,
        },
        rates: RateMetrics {
            attempted_requests_per_second,
            successful_requests_per_second: total_successes as f64 / actual_duration,
            target_achievement_percent: (!cli.unthrottled)
                .then_some(attempted_requests_per_second / cli.rate as f64 * 100.0),
            run_wall_time_ms: start.elapsed().as_secs_f64() * 1000.0,
        },
        successful_latency_us: latency_metrics(&merged_hist),
    };

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("bug: benchmark result should serialize")
        );
    } else {
        print_human_result(&result);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::{hex::Hex0x, runtime_manifest::RuntimeManifest};
    use groundhog_xdr::types::{
        ContractAddress as XdrContractAddress, Ed25519PublicKey as XdrEd25519PublicKey, SHA256Hash,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn json_rpc_result_is_successful() {
        assert_eq!(
            classify_json_rpc_value(&json!({"jsonrpc": "2.0", "id": 7, "result": null}), 7,),
            RequestOutcome::Success
        );
    }

    #[test]
    fn json_rpc_error_is_not_successful() {
        assert_eq!(
            classify_json_rpc_value(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "error": {"code": -32000, "message": "rejected"}
                }),
                7,
            ),
            RequestOutcome::Error(RequestErrorKind::JsonRpc)
        );
    }

    #[test]
    fn mismatched_or_incomplete_json_rpc_response_is_invalid() {
        for value in [
            json!({"jsonrpc": "2.0", "id": 8, "result": true}),
            json!({"jsonrpc": "2.0", "id": 7}),
            json!({"id": 7, "result": true}),
            json!({"jsonrpc": "2.0", "id": 7, "result": true, "error": null}),
            json!({"jsonrpc": "2.0", "id": 7, "error": "rejected"}),
            json!([]),
        ] {
            assert_eq!(
                classify_json_rpc_value(&value, 7),
                RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse)
            );
        }
    }

    #[test]
    fn error_counts_have_stable_json_field_names() {
        let value = serde_json::to_value(RequestErrorCounts {
            transport: 1,
            http_status: 2,
            invalid_json_rpc_response: 3,
            json_rpc: 4,
        })
        .expect("bug: error counts should serialize");

        assert_eq!(
            value,
            json!({
                "transport": 1,
                "httpStatus": 2,
                "invalidJsonRpcResponse": 3,
                "jsonRpc": 4,
            })
        );
    }

    #[test]
    fn benchmark_result_has_stable_json_shape() {
        let value = serde_json::to_value(BenchResult {
            schema_version: BENCH_RESULT_SCHEMA_VERSION,
            kind: "groundhog-bench-result",
            generated_at_unix_ms: 1_700_000_000_000,
            producer: ProducerMetadata {
                name: "groundhog-bench",
                version: "0.9.2",
                build_profile: "release",
                git_commit: Some("abcdef0".into()),
                git_dirty: Some(false),
            },
            platform: PlatformMetadata {
                os: "linux",
                arch: "x86_64",
            },
            runtime: Some(RuntimeIdentity {
                manifest_sha256: format!("0x{}", "11".repeat(32)),
                manifest_schema_version: 2,
                kind: "hogd-dev-runtime".into(),
                chain_state: Some("fresh".into()),
                network_hash: "22".repeat(32),
                protocol: "hogp/abcdef012345".into(),
                genesis_hash: format!("0x{}", "33".repeat(32)),
                producer: RuntimeProducerIdentity {
                    binary: "hog-devnode".into(),
                    version: "1.2.3".into(),
                    commit_hash: Some("7654321".into()),
                },
            }),
            configuration: BenchConfiguration {
                scenario: Some("gateway-baseline".into()),
                target_label: Some("standalone gateway baseline".into()),
                url: "http://127.0.0.1:9546".into(),
                method: "chainTip".into(),
                mode: "rate_limited",
                target_requests_per_second: Some(250),
                duration_seconds: 30,
                warmup_seconds: 2,
                workers: 4,
                connections_per_worker: 16,
                max_inflight_per_worker: 32,
                accounts: 10,
                transaction_batch_size: 0,
                max_skew_seconds: 1.0,
                runtime_manifest: Some("/tmp/runtime-manifest.json".into()),
            },
            calibration: CalibrationMetrics { median_rtt_us: 800 },
            requests: RequestMetrics {
                total: 7_500,
                successful: 7_496,
                failed: 4,
                errors: RequestErrorCounts {
                    transport: 1,
                    http_status: 1,
                    invalid_json_rpc_response: 1,
                    json_rpc: 1,
                },
            },
            rates: RateMetrics {
                attempted_requests_per_second: 250.0,
                successful_requests_per_second: 249.866,
                target_achievement_percent: Some(100.0),
                run_wall_time_ms: 32_010.5,
            },
            successful_latency_us: Some(LatencyMetrics {
                samples: 7_496,
                min: 410,
                p50: 700,
                p90: 900,
                p95: 1_000,
                p99: 1_200,
                p999: 1_500,
                max: 2_000,
                mean: 725.5,
            }),
        })
        .expect("bug: benchmark result should serialize");

        assert_eq!(
            value,
            json!({
                "schemaVersion": 1,
                "kind": "groundhog-bench-result",
                "generatedAtUnixMs": 1_700_000_000_000_u64,
                "producer": {
                    "name": "groundhog-bench",
                    "version": "0.9.2",
                    "buildProfile": "release",
                    "gitCommit": "abcdef0",
                    "gitDirty": false,
                },
                "platform": {
                    "os": "linux",
                    "arch": "x86_64",
                },
                "runtime": {
                    "manifestSha256": format!("0x{}", "11".repeat(32)),
                    "manifestSchemaVersion": 2,
                    "kind": "hogd-dev-runtime",
                    "chainState": "fresh",
                    "networkHash": "22".repeat(32),
                    "protocol": "hogp/abcdef012345",
                    "genesisHash": format!("0x{}", "33".repeat(32)),
                    "producer": {
                        "binary": "hog-devnode",
                        "version": "1.2.3",
                        "commitHash": "7654321",
                    },
                },
                "configuration": {
                    "scenario": "gateway-baseline",
                    "targetLabel": "standalone gateway baseline",
                    "url": "http://127.0.0.1:9546",
                    "method": "chainTip",
                    "mode": "rate_limited",
                    "targetRequestsPerSecond": 250,
                    "durationSeconds": 30,
                    "warmupSeconds": 2,
                    "workers": 4,
                    "connectionsPerWorker": 16,
                    "maxInflightPerWorker": 32,
                    "accounts": 10,
                    "transactionBatchSize": 0,
                    "maxSkewSeconds": 1.0,
                    "runtimeManifest": "/tmp/runtime-manifest.json",
                },
                "calibration": {
                    "medianRttUs": 800,
                },
                "requests": {
                    "total": 7_500,
                    "successful": 7_496,
                    "failed": 4,
                    "errors": {
                        "transport": 1,
                        "httpStatus": 1,
                        "invalidJsonRpcResponse": 1,
                        "jsonRpc": 1,
                    },
                },
                "rates": {
                    "attemptedRequestsPerSecond": 250.0,
                    "successfulRequestsPerSecond": 249.866,
                    "targetAchievementPercent": 100.0,
                    "runWallTimeMs": 32_010.5,
                },
                "successfulLatencyUs": {
                    "samples": 7_496,
                    "min": 410,
                    "p50": 700,
                    "p90": 900,
                    "p95": 1_000,
                    "p99": 1_200,
                    "p999": 1_500,
                    "max": 2_000,
                    "mean": 725.5,
                },
            })
        );
    }

    #[test]
    fn producer_metadata_preserves_unknown_git_provenance() {
        let value = serde_json::to_value(ProducerMetadata {
            name: "groundhog-bench",
            version: "0.9.2",
            build_profile: "release",
            git_commit: None,
            git_dirty: None,
        })
        .expect("bug: producer metadata should serialize");

        assert_eq!(
            value,
            json!({
                "name": "groundhog-bench",
                "version": "0.9.2",
                "buildProfile": "release",
                "gitCommit": null,
                "gitDirty": null,
            })
        );
    }

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

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 1) {
            Ok(_) => panic!("bug: unsupported derivation scheme should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            err,
            BenchError::UnsupportedDerivationScheme { .. }
        ));
    }

    #[test]
    fn send_raw_transaction_requires_runtime_manifest() {
        let err = match transaction_inputs_for_method("sendRawTransaction", None, 1) {
            Ok(_) => panic!("bug: submit benchmark without runtime metadata should fail"),
            Err(error) => error,
        };

        assert!(matches!(err, BenchError::MissingRuntimeManifest));
    }

    #[test]
    fn send_raw_transaction_rejects_zero_accounts_before_manifest_validation() {
        let mut manifest = manifest();
        manifest.bootstrap_account_derivation.scheme = "future-scheme".into();

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 0) {
            Ok(_) => panic!("bug: zero-account submit benchmark should fail"),
            Err(error) => error,
        };

        assert!(matches!(err, BenchError::MissingAccounts));
    }

    #[test]
    fn send_raw_transaction_uses_manifest_do_action_entrypoint() {
        let (accounts, entrypoint) =
            transaction_inputs_for_method("sendRawTransaction", Some(&manifest()), 1)
                .expect("bug: valid manifest should provide transaction inputs");

        assert_eq!(accounts.len(), 1);
        assert_eq!(entrypoint, ContractEntrypointID::from(*b"doAction"));
    }

    #[test]
    fn send_raw_transaction_uses_the_requested_subset_of_manifest_accounts() {
        let (accounts, _) = transaction_inputs_for_method(
            "sendRawTransaction",
            Some(&manifest_with_accounts(3)),
            2,
        )
        .expect("bug: manifest has the requested number of accounts");

        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn send_raw_transaction_excludes_faucet_and_preserves_derivation_indices() {
        let mut manifest = manifest_with_accounts(3);
        manifest.bootstrap_accounts[0].role = "faucet".into();

        let (accounts, _) = transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 2)
            .expect("bug: two local-user accounts should be available");

        assert_eq!(accounts.len(), 2);
        assert_eq!(
            accounts[0].signer.verifying_key_bytes(),
            derive_dev_account_signer(&manifest.bootstrap_account_derivation.seed, 1)
                .verifying_key_bytes()
        );
    }

    #[test]
    fn send_raw_transaction_rejects_more_accounts_than_the_manifest_contains() {
        let err = match transaction_inputs_for_method(
            "sendRawTransaction",
            Some(&manifest_with_accounts(2)),
            3,
        ) {
            Ok(_) => panic!("bug: account underfill should fail"),
            Err(error) => error,
        };

        assert!(matches!(
            err,
            BenchError::NotEnoughAccounts {
                requested: 3,
                available: 2
            }
        ));
    }

    #[test]
    fn send_raw_transaction_rejects_public_key_mismatch() {
        let mut manifest = manifest();
        manifest.bootstrap_accounts[0].public_key = Hex0x(XdrEd25519PublicKey::from([0x99; 32]));

        let err = match transaction_inputs_for_method("sendRawTransaction", Some(&manifest), 1) {
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
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
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
                    "createWalletEntrypoint": "0x6372656174655761",
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

    fn manifest_with_accounts(count: usize) -> RuntimeManifest {
        let mut manifest = manifest();
        for index in 1..count {
            let signer =
                derive_dev_account_signer(&manifest.bootstrap_account_derivation.seed, index);
            let mut public_key = [0u8; 32];
            public_key.copy_from_slice(&signer.verifying_key_bytes());
            let mut account = manifest.bootstrap_accounts[0].clone();
            account.index = u32::try_from(index).expect("bug: test account index should fit");
            account.public_key = Hex0x(XdrEd25519PublicKey::from(public_key));
            account.wallet_address = Hex0x(XdrContractAddress::from([index as u8; 32]));
            manifest.bootstrap_accounts.push(account);
        }
        manifest
    }

    fn hash(byte: u8) -> Hex0x<SHA256Hash> {
        Hex0x(SHA256Hash::from([byte; 32]))
    }
}
