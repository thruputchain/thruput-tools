//! Thruput JSON-RPC benchmark harness.
//!
//! Multi-worker load generator. In the default open-loop mode, each worker owns
//! its own HTTP connection pool and fires requests on a pre-computed schedule.
//! If the harness cannot sustain the target rate it fails rather than silently
//! degrading to closed-loop. `--unthrottled` instead dispatches as fast as the
//! configured worker and in-flight caps allow.
//!
//! Usage:
//!   # Against a local JSON-RPC endpoint (smoke):
//!   thruput-bench --url http://127.0.0.1:9545 --method getChainTip --rate 1000 --duration 5
//!
//!   # Against real gateway (load):
//!   thruput-bench --url http://127.0.0.1:8545 --runtime-manifest /path/to/runtime-manifest.json --rate 100000 --duration 60 --workers 16

use std::io::{self, IsTerminal};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{process, sync::Arc};

use clap::Parser;
use hdrhistogram::Histogram;
use thruput_signing::SigningError;

mod cli;
mod compare;
mod corpus;
mod metadata;
mod observations;
mod request;
mod results;
mod worker;
mod workload;

use cli::{Cli, confirm_submission, confirmation_required_json};
use request::calibrate_rtt;
use results::{
    BENCH_RESULT_SCHEMA_VERSION, BenchConfiguration, BenchResult, CalibrationMetrics,
    ProducerMetadata, RateMetrics, RequestMetrics, RuntimeIdentity, RuntimeProducerIdentity,
    benchmark_git_commit, benchmark_git_dirty, latency_metrics, print_human_result,
};
use worker::{WorkerConfig, run_worker};
use workload::{
    generate_tx_batch, load_runtime_manifest, transaction_inputs_for_method, validate_method,
};

#[derive(Debug, thiserror::Error)]
enum BenchError {
    #[error("invalid benchmark configuration: {0}")]
    InvalidConfiguration(String),
    #[error("benchmark setup failed: {0}")]
    Setup(String),
    #[error("failed to access {path}: {source}")]
    File {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("benchmark recorded failed requests; see the result for counts")]
    MeasuredFailures,
    #[error(
        "benchmark started no requests in the measured window; increase duration or reduce warmup request time"
    )]
    EmptyMeasurement,
    #[error("--duration must be >= 1")]
    InvalidDuration,
    #[error("--request-timeout-ms must be >= 1")]
    InvalidRequestTimeout,
    #[error("--max-response-bytes must be >= 1")]
    InvalidMaxResponseBytes,
    #[error("--accounts must be >= 1 for sendTransaction")]
    MissingAccounts,
    #[error(
        "--accounts requested {requested} accounts, but the runtime manifest only contains {available}"
    )]
    NotEnoughAccounts { requested: usize, available: usize },
    #[error("--runtime-manifest is required for sendTransaction")]
    MissingRuntimeManifest,
    #[error("unknown method `{method}`")]
    UnknownMethod { method: String },
    #[error(
        "confirmation required to submit benchmark transactions; pass --force to continue non-interactively"
    )]
    ConfirmationRequired,
    #[error("benchmark submission cancelled")]
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
    #[error(
        "runtime manifest bootstrap account {index} address is not its nonce-zero native account"
    )]
    BootstrapAddressMismatch { index: u32 },
    #[error("runtime manifest bootstrap account index {index} does not fit this platform")]
    BootstrapAccountIndexTooLarge { index: u32 },
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
    let mut cli = Cli::parse();
    if let Some(cli::Command::Compare(args)) = cli.command.take() {
        if let Err(error) = compare::run(args) {
            eprintln!("error: {error}");
            process::exit(1);
        }
        return;
    }
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

fn validate_configuration(cli: &Cli) -> Result<(), BenchError> {
    let invalid = |message: &str| BenchError::InvalidConfiguration(message.into());
    if cli.workers == 0 || cli.workers > 1024 {
        return Err(invalid("--workers must be in 1..=1024"));
    }
    if cli.duration == 0 {
        return Err(BenchError::InvalidDuration);
    }
    if cli.duration.checked_mul(1000).is_none()
        || Instant::now()
            .checked_add(Duration::from_secs(cli.setup_timeout_secs))
            .is_none()
        || Instant::now()
            .checked_add(Duration::from_millis(cli.request_timeout_ms))
            .is_none()
    {
        return Err(invalid(
            "duration or timeout exceeds the supported clock range",
        ));
    }
    if cli
        .rate
        .is_some_and(|rate| rate == 0 || rate > 1_000_000_000)
    {
        return Err(invalid("--rate must be in 1..=1000000000"));
    }
    if cli.request_timeout_ms == 0 {
        return Err(BenchError::InvalidRequestTimeout);
    }
    if cli.max_response_bytes == 0 {
        return Err(BenchError::InvalidMaxResponseBytes);
    }
    if cli.connections_per_worker > 65536 {
        return Err(invalid("--connections-per-worker must be <= 65536"));
    }
    if cli.outstanding.is_some_and(|n| n == 0 || n > 65536) {
        return Err(invalid("--outstanding must be in 1..=65536"));
    }
    if !cli.max_skew.is_finite() || cli.max_skew < 0.0 || cli.max_skew > 86400.0 {
        return Err(invalid("--max-skew must be finite and in 0..=86400"));
    }
    if cli.setup_timeout_secs == 0 {
        return Err(invalid("--setup-timeout-secs must be >= 1"));
    }
    if cli
        .seed_count
        .is_some_and(|count| count == 0 || count > corpus::MAX_SEED_COUNT)
    {
        return Err(invalid("--seed-count must be in 1..=100000"));
    }
    validate_method(&cli.method)?;
    let confirmed = cli.hashes_file.is_some() || cli.seed_count.is_some();
    if confirmed
        && !matches!(
            cli.method.as_str(),
            "getTransactionStatus" | "getTransactionReceipt"
        )
    {
        return Err(invalid(
            "a confirmed corpus requires getTransactionStatus or getTransactionReceipt",
        ));
    }
    if cli.method == "getTransactionReceipt" && !confirmed {
        return Err(invalid(
            "getTransactionReceipt requires --hashes-file or --seed-count",
        ));
    }
    if cli.tx_batch_size == Some(0) {
        return Err(invalid("--tx-batch-size must be >= 1"));
    }
    Ok(())
}

async fn run(cli: Cli) -> Result<(), BenchError> {
    validate_configuration(&cli)?;
    let loaded_manifest = match &cli.runtime_manifest {
        Some(path) => Some(load_runtime_manifest(path).await?),
        None => None,
    };
    let manifest = loaded_manifest.as_ref().map(|loaded| &loaded.manifest);
    let needs_transactions = cli.method == "sendTransaction";
    let accounts = transaction_inputs_for_method(
        if cli.seed_count.is_some() {
            "sendTransaction"
        } else {
            &cli.method
        },
        manifest,
        cli.accounts,
        cli.account_seed.as_deref(),
    )?;
    let stdin = io::stdin();
    let stderr = io::stderr();
    confirm_submission(
        &cli,
        stdin.is_terminal(),
        stderr.is_terminal(),
        &mut stdin.lock(),
        &mut stderr.lock(),
    )?;
    if let Some(path) = &cli.seed_output {
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(BenchError::InvalidConfiguration(format!(
                    "seed output already exists: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(BenchError::File {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }
    let measured_duration = Duration::from_secs(cli.duration);
    let warmup_duration = Duration::from_secs(cli.warmup);
    let total_duration = measured_duration
        .checked_add(warmup_duration)
        .filter(|duration| Instant::now().checked_add(*duration).is_some())
        .ok_or_else(|| {
            BenchError::InvalidConfiguration("benchmark duration is too large".into())
        })?;
    let rate = cli.rate.unwrap_or(100);
    let closed_loop = cli.unthrottled || cli.outstanding.is_some();
    let interval = (!closed_loop).then(|| Duration::from_secs_f64(1.0 / rate as f64));
    let request_timeout = Duration::from_millis(cli.request_timeout_ms);
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|source| BenchError::HttpClient { source })?;
    let target = metadata::collect(&cli, &client).await?;
    if let (Some(manifest), Some(info)) = (manifest, &target.runner_supplied)
        && let (Some(actual), Some(expected)) =
            (&manifest.producer.commit_hash, &info.backend.commit_hash)
        && actual != expected
    {
        return Err(BenchError::InvalidConfiguration(
            "runner backend commit differs from runtime manifest".into(),
        ));
    }
    let corpus = if cli.hashes_file.is_some() || cli.seed_count.is_some() {
        eprintln!("Preparing and validating confirmed hashes outside the measurement window...");
        Some(corpus::prepare(&cli, manifest, &client).await?)
    } else {
        None
    };
    let tx_batch = if needs_transactions {
        let batch_size = match cli.tx_batch_size {
            Some(count) => count,
            None if closed_loop => 100_000,
            None => usize::try_from(
                rate.checked_mul(
                    cli.duration
                        .checked_add(cli.warmup)
                        .and_then(|n| n.checked_add(1))
                        .ok_or_else(|| {
                            BenchError::InvalidConfiguration(
                                "transaction batch size overflow".into(),
                            )
                        })?,
                )
                .ok_or_else(|| {
                    BenchError::InvalidConfiguration("transaction batch size overflow".into())
                })?,
            )
            .map_err(|_| {
                BenchError::InvalidConfiguration("transaction batch size overflow".into())
            })?,
        };
        let network_hash = manifest
            .ok_or(BenchError::MissingRuntimeManifest)?
            .network_hash
            .to_network_hash();
        // Start load nonces at the current time so a rerun never replays an earlier run.
        let nonce_start = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| BenchError::Setup(error.to_string()))?
                .as_nanos(),
        )
        .map_err(|_| BenchError::Setup("current time does not fit a load nonce".into()))?;
        generate_tx_batch(batch_size, &accounts, &network_hash, nonce_start, cli.block_expiry_height)?
            .into_iter()
            .map(|tx| tx.hex)
            .collect()
    } else {
        Vec::new()
    };
    let transaction_batch_size = tx_batch.len();
    let median_rtt =
        calibrate_rtt(&client, &cli.url, request_timeout, cli.max_response_bytes).await?;
    let connections_per_worker = if cli.connections_per_worker > 0 {
        cli.connections_per_worker
    } else if let Some(n) = cli.outstanding {
        n.div_ceil(cli.workers)
    } else if closed_loop {
        256
    } else {
        ((rate as f64 / cli.workers as f64 * median_rtt.as_secs_f64() * 4.0).ceil() as usize)
            .clamp(16, 2048)
    };
    let per_worker_cap = (cli.outstanding.is_none()).then_some(connections_per_worker * 2);
    let effective_max_inflight = cli
        .outstanding
        .unwrap_or(connections_per_worker * 2 * cli.workers);
    let global_sem = cli
        .outstanding
        .map(|n| Arc::new(tokio::sync::Semaphore::new(n)));
    let mut clients = Vec::with_capacity(cli.workers);
    for _ in 0..cli.workers {
        clients.push(
            reqwest::Client::builder()
                .pool_max_idle_per_host(connections_per_worker)
                .build()
                .map_err(|source| BenchError::HttpClient { source })?,
        );
    }
    let benchmark_binary_sha256 =
        match std::env::current_exe().and_then(|path| metadata::binary_digest(&path)) {
            Ok(value) => metadata::Observation::Observed { value },
            Err(error) => metadata::Observation::Unavailable {
                reason: error.to_string(),
            },
        };
    let platform = metadata::HostMetadata::local();
    let (corpus_identity, records) = match corpus {
        Some(corpus) => (Some(corpus.identity), corpus.records),
        None => (None, Vec::new()),
    };
    eprintln!(
        "Benchmarking {}: {} workers, {} maximum in flight, {}s measured after {}s warmup",
        cli.method, cli.workers, effective_max_inflight, cli.duration, cli.warmup
    );
    if let Some(path) = &cli.load_start_file {
        std::fs::File::create_new(path).map_err(|source| BenchError::File {
            path: path.display().to_string(),
            source,
        })?;
    }
    let start = Instant::now();
    let cfg = Arc::new(WorkerConfig {
        url: cli.url.clone(),
        method: cli.method.clone(),
        tx_batch,
        records,
        start,
        warmup_duration,
        measured_duration,
        interval,
        max_skew: Duration::from_secs_f64(cli.max_skew),
        request_timeout,
        max_response_bytes: cli.max_response_bytes,
        num_workers: cli.workers,
    });
    let mut observation_tasks = tokio::task::JoinSet::new();
    let observation_config = observations::Config {
        metrics_url: cli.metrics_url.clone(),
        gateway_pid: cli.gateway_pid,
        interval_ms: cli.sample_interval_ms,
    };
    observation_tasks.spawn(observations::collect(
        observation_config,
        start + warmup_duration,
        start + total_duration,
    ));
    let mut workers = tokio::task::JoinSet::new();
    for (id, client) in clients.into_iter().enumerate() {
        let sem = global_sem
            .clone()
            .unwrap_or_else(|| Arc::new(tokio::sync::Semaphore::new(connections_per_worker * 2)));
        workers.spawn(run_worker(id, cfg.clone(), client, sem));
    }
    let mut histogram = Histogram::<u64>::new(3).expect("bug: histogram precision is valid");
    let mut requests = RequestMetrics::default();
    let mut drained = RequestMetrics::default();
    while let Some(result) = workers.join_next().await {
        let result = result.map_err(|source| BenchError::WorkerJoin { source })??;
        histogram
            .add(&result.histogram)
            .expect("bug: compatible histograms");
        requests.merge(result.requests);
        drained.merge(result.drained);
    }
    // Open-loop workers can finish their scheduled calls before the last fraction
    // of the window. Keep its duration and drain boundary explicit either way.
    tokio::time::sleep_until((start + total_duration).into()).await;
    let finished = Instant::now();
    let server_observations = observation_tasks
        .join_next()
        .await
        .expect("bug: observation task exists")
        .map_err(|source| BenchError::WorkerJoin { source })?;
    let actual_duration = measured_duration.as_secs_f64();
    let attempted_requests_per_second = (requests.total + drained.total) as f64 / actual_duration;
    let rates = RateMetrics {
        attempted_requests_per_second,
        successful_requests_per_second: requests.successful as f64 / actual_duration,
        target_achievement_percent: (!closed_loop)
            .then_some(attempted_requests_per_second / rate as f64 * 100.0),
        run_wall_time_ms: finished.duration_since(start).as_secs_f64() * 1000.0,
    };
    let result = BenchResult {
        schema_version: BENCH_RESULT_SCHEMA_VERSION,
        kind: "groundhog-bench-result",
        outcome: if requests.failed + drained.failed > 0 {
            "failed"
        } else if requests.total + drained.total == 0 {
            "empty"
        } else {
            "passed"
        },
        generated_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        producer: ProducerMetadata {
            name: "thruput-bench",
            version: env!("CARGO_PKG_VERSION"),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            git_commit: benchmark_git_commit(),
            git_dirty: benchmark_git_dirty(),
        },
        platform,
        server_observations,
        benchmark_binary_sha256,
        runtime_worker_threads: tokio::runtime::Handle::current().metrics().num_workers(),
        target,
        corpus: corpus_identity,
        run: results::RunIdentity {
            scenario: cli.scenario,
            target_label: cli.target_label,
            url: cli.url,
            runtime_manifest: cli.runtime_manifest.map(|path| path.display().to_string()),
            hashes_file: cli
                .hashes_file
                .or(cli.seed_output)
                .map(|path| path.display().to_string()),
        },
        measurement: results::Measurement {
            inclusion: "started-and-completed-in-window",
            window_ms: cli.duration.checked_mul(1000).ok_or_else(|| {
                BenchError::InvalidConfiguration("duration milliseconds overflow".into())
            })?,
            drain_ms: finished
                .saturating_duration_since(start + total_duration)
                .as_secs_f64()
                * 1000.0,
        },
        runtime: loaded_manifest.as_ref().map(|loaded| RuntimeIdentity {
            manifest_sha256: loaded.sha256.clone(),
            manifest_schema_version: loaded.manifest.schema_version,
            kind: loaded.manifest.kind.clone(),
            chain_state: loaded.manifest.chain_state.map(|state| state.to_string()),
            network_hash: loaded.manifest.network_hash.to_string(),
            protocol: loaded.manifest.protocol.clone(),
            producer: RuntimeProducerIdentity {
                binary: loaded.manifest.producer.binary.clone(),
                version: loaded.manifest.producer.version.clone(),
                commit_hash: loaded.manifest.producer.commit_hash.clone(),
            },
        }),
        configuration: BenchConfiguration {
            method: cli.method,
            mode: if cli.outstanding.is_some() {
                "closed_loop"
            } else if cli.unthrottled {
                "unthrottled"
            } else {
                "rate_limited"
            },
            target_requests_per_second: (!closed_loop).then_some(rate),
            duration_seconds: cli.duration,
            warmup_seconds: cli.warmup,
            workers: cli.workers,
            connections_per_worker,
            max_inflight_per_worker: per_worker_cap,
            outstanding: cli.outstanding,
            effective_max_inflight,
            accounts: if needs_transactions { cli.accounts } else { 0 },
            account_fixture_sha256: cli
                .account_seed
                .as_ref()
                .map(|seed| thruput_json_rpc_types::hex::sha256_prefixed_hex(seed.as_bytes())),
            block_expiry_height: (needs_transactions || cli.seed_count.is_some())
                .then_some(cli.block_expiry_height),
            transaction_batch_size,
            max_skew_seconds: cli.max_skew,
            request_timeout_ms: cli.request_timeout_ms,
            max_response_bytes: cli.max_response_bytes,
        },
        calibration: CalibrationMetrics {
            median_rtt_us: median_rtt.as_micros() as u64,
        },
        requests,
        drained,
        rates,
        successful_latency_us: latency_metrics(&histogram),
    };
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("bug: benchmark result should serialize")
        );
    } else {
        print_human_result(&result);
    }
    if result.requests.failed + result.drained.failed > 0 {
        return Err(BenchError::MeasuredFailures);
    }
    if result.requests.total + result.drained.total == 0 {
        return Err(BenchError::EmptyMeasurement);
    }
    Ok(())
}
