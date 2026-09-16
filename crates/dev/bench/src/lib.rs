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

use std::io::{self, IsTerminal};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{process, sync::Arc};

use clap::Parser;
use groundhog_json_rpc_types::runtime_manifest::RuntimeManifestEntrypointError;
use groundhog_signing::SigningError;
use hdrhistogram::Histogram;

mod cli;
mod request;
mod results;
mod worker;
mod workload;

use cli::{Cli, confirm_submission, confirmation_required_json};
use request::calibrate_rtt;
use results::{
    BENCH_RESULT_SCHEMA_VERSION, BenchConfiguration, BenchResult, CalibrationMetrics,
    PlatformMetadata, ProducerMetadata, RateMetrics, RequestErrorCounts, RequestMetrics,
    RuntimeIdentity, RuntimeProducerIdentity, benchmark_git_commit, benchmark_git_dirty,
    latency_metrics, print_human_result,
};
use worker::{WorkerConfig, run_worker};
use workload::{
    generate_tx_batch, load_runtime_manifest, transaction_inputs_for_method, validate_method,
};

#[derive(Debug, thiserror::Error)]
enum BenchError {
    #[error("--workers must be >= 1")]
    InvalidWorkers,
    #[error("--rate must be >= 1")]
    InvalidRate,
    #[error("--duration must be >= 1")]
    InvalidDuration,
    #[error("--request-timeout-ms must be >= 1")]
    InvalidRequestTimeout,
    #[error("--max-response-bytes must be >= 1")]
    InvalidMaxResponseBytes,
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
    #[error(
        "confirmation required to benchmark sendRawTransaction; pass --force to continue non-interactively"
    )]
    ConfirmationRequired,
    #[error("sendRawTransaction benchmark cancelled")]
    ConfirmationDeclined,
    #[error("failed to read or write confirmation: {source}")]
    ConfirmationIo {
        #[source]
        source: std::io::Error,
    },
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

/// Parse command-line arguments and run the benchmark.
///
/// Reports errors using the selected output format and exits with status 1 on failure.
pub async fn run_cli() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(error) = run(cli).await {
        if json && matches!(error, BenchError::ConfirmationRequired) {
            println!("{}", confirmation_required_json());
            process::exit(1);
        }
        eprintln!("error: {error}");
        process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), BenchError> {
    if cli.workers == 0 {
        return Err(BenchError::InvalidWorkers);
    }
    if !cli.unthrottled && cli.rate == 0 {
        return Err(BenchError::InvalidRate);
    }
    if cli.duration == 0 {
        return Err(BenchError::InvalidDuration);
    }
    if cli.request_timeout_ms == 0 {
        return Err(BenchError::InvalidRequestTimeout);
    }
    if cli.max_response_bytes == 0 {
        return Err(BenchError::InvalidMaxResponseBytes);
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

    let stdin = io::stdin();
    let stderr = io::stderr();
    confirm_submission(
        &cli,
        stdin.is_terminal(),
        stderr.is_terminal(),
        &mut stdin.lock(),
        &mut stderr.lock(),
    )?;

    let total_duration = Duration::from_secs(cli.duration);
    let warmup_duration = Duration::from_secs(cli.warmup);
    let total_run_duration = total_duration + warmup_duration;
    let interval = if cli.unthrottled {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(1.0 / cli.rate as f64)
    };
    let max_skew = Duration::from_secs_f64(cli.max_skew);
    let request_timeout = Duration::from_millis(cli.request_timeout_ms);

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
        // sendRawTransaction requires the manifest, so the hash is always
        // available on this path; transaction_inputs_for_method enforces it.
        let network_hash = manifest
            .ok_or(BenchError::MissingRuntimeManifest)?
            .network_hash
            .to_network_hash();
        let batch = generate_tx_batch(batch_size, &accounts, entrypoint, &network_hash)?;
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
    let median_rtt = calibrate_rtt(
        &probe_client,
        &cli.url,
        request_timeout,
        cli.max_response_bytes,
    )
    .await?;
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
        request_timeout,
        max_response_bytes: cli.max_response_bytes,
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
            request_timeout_ms: cli.request_timeout_ms,
            max_response_bytes: cli.max_response_bytes,
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
