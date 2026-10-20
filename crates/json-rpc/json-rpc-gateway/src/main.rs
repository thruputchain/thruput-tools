mod adaptive_batch;
mod airdrop;
mod backend;
mod config;
mod context;
mod handlers;
mod health;
mod heap_profiling;
mod metrics;
mod middleware;
mod observability;
mod rpc_middleware;
mod storage_batcher;
mod submitter;
mod telemetry;
mod tip_coalescer;

use std::{future::Future, path::Path, sync::Arc, time::Duration};

use clap::{CommandFactory, FromArgMatches, Parser};
use tokio::io::AsyncReadExt;
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

use crate::backend::GatewayBackend;
use crate::config::{GatewayConfig, HealthBackend};
use crate::context::GatewayContext;
use thruput_bundle_identity::{BundleIdentity, BundleSubject};
use thruput_json_rpc_server::{
    RpcServerConfig,
    native::service::{NativeCapabilities, NativeService},
};
use thruput_json_rpc_types::{NetworkHashText, SHA256Hash};

fn native_capabilities(config: &GatewayConfig) -> NativeCapabilities {
    NativeCapabilities::new(
        true,
        config.indexer_address.is_some(),
        config.db_gateway_address.is_some(),
        config.view_gateway_address.is_some(),
    )
}

fn native_module(
    api: NativeService<GatewayBackend, GatewayBackend, GatewayBackend, GatewayBackend>,
    funding: Option<airdrop::HostedAirdrop>,
) -> Result<jsonrpsee::RpcModule<()>, jsonrpsee::core::RegisterMethodError> {
    match funding {
        Some(funding) => Ok(api
            .with_funding(funding)
            .into_module()?
            .into_rpc()
            .remove_context()),
        None => Ok(api.into_module()?.into_rpc().remove_context()),
    }
}

const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;
const BUILD_VERSION: thruput_build_info::BuildVersion = thruput_build_info::build_version!();

/// Groundhog JSON-RPC gateway.
#[derive(Parser)]
#[command(name = "hog-gateway", about = "Groundhog JSON-RPC gateway")]
struct Cli {
    /// Print resolved configuration as JSON and exit without opening listeners.
    #[arg(long)]
    dump_effective_config: bool,

    /// Emit close events for enabled tracing spans (diagnostics; adds overhead).
    #[arg(long)]
    trace_spans: bool,

    /// Path to TOML config file
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Logical network identifier used by the hosted faucet API
    #[arg(long)]
    network_id: Option<String>,

    /// Canonical unprefixed network hash (overrides config file)
    #[arg(long, value_parser = parse_network_hash)]
    network_hash: Option<NetworkHashText>,

    /// Hosted-faucet genesis hash (overrides config file)
    #[arg(long, value_parser = parse_sha256_hash)]
    genesis_hash: Option<SHA256Hash>,

    /// Hosted faucet base URL (overrides config file)
    #[arg(long)]
    faucet_url: Option<String>,

    /// Path to backend runtime manifest JSON
    #[arg(long)]
    runtime_manifest: Option<std::path::PathBuf>,

    /// Listen address (overrides config file)
    #[arg(long)]
    listen: Option<String>,

    /// Concurrent entries per JSON-RPC array (1..=1024, default 128; overrides config)
    #[arg(long)]
    rpc_batch_concurrency: Option<thruput_json_rpc_server::batch::RpcBatchConcurrency>,

    /// Private health/metrics listen address (overrides config file)
    #[arg(long)]
    observability_listen: Option<String>,

    /// Serve heap profiles on a non-loopback observability listener (unauthenticated).
    #[arg(long)]
    pprof_allow_remote: bool,

    /// Comma-separated backends required for readiness
    #[arg(long, value_delimiter = ',')]
    health_required_backends: Option<Vec<HealthBackend>>,

    /// Maximum allowed age of the last successful chain-tip probe
    #[arg(long)]
    health_chain_tip_max_age_ms: Option<u64>,

    /// Background readiness probe interval
    #[arg(long)]
    health_probe_interval_ms: Option<u64>,

    /// Maximum time to build one backend x23 connection pool
    #[arg(long)]
    backend_connect_timeout_ms: Option<u64>,

    /// Delay scheduler tip probes after building its x23 connection pool
    #[arg(long)]
    scheduler_reconnect_grace_ms: Option<u64>,

    /// Maximum total time to drain after a shutdown signal
    #[arg(long)]
    shutdown_grace_ms: Option<u64>,

    /// Scheduler (mempool) x23 RPC address (overrides config file)
    #[arg(long)]
    scheduler: Option<String>,

    /// Indexer x23 RPC address (overrides config file)
    #[arg(long)]
    indexer: Option<String>,

    /// Scheduler lookup execution permits (1..=68, default 16; overrides config)
    #[arg(long)]
    scheduler_lookup_pool_size: Option<crate::config::SchedulerLookupPoolSize>,

    /// Connections per x23 data pool (1..=64, default 4; overrides config)
    #[arg(long)]
    x23_multiplex_connections: Option<crate::config::X23MultiplexConnections>,

    /// `DbGateway` x23 RPC address (overrides config file)
    #[arg(long)]
    db_gateway: Option<String>,

    /// `ViewGateway` x23 RPC address (overrides config file)
    #[arg(long)]
    view_gateway: Option<String>,

    /// Override hogd protocol string for local cross-repo smoke tests
    #[arg(long, hide = true)]
    hogd_protocol: Option<String>,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bundle_identity =
        BundleIdentity::discover(BundleSubject::component("groundhog-rpc"), BUILD_VERSION);
    let cli = parse_cli(&bundle_identity);

    // --- Observability ---
    let env_filter = if cli.dump_effective_config {
        EnvFilter::new("off")
    } else {
        EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .from_env_lossy()
    };
    let span_events = if cli.trace_spans {
        tracing_subscriber::fmt::format::FmtSpan::CLOSE
    } else {
        tracing_subscriber::fmt::format::FmtSpan::NONE
    };
    let telemetry = telemetry::init("hog-gateway", env_filter, span_events);

    tracing::info!(
        component_version = bundle_identity.build_version().crate_version(),
        source_commit = ?bundle_identity.build_version().source_commit(),
        release_version = ?bundle_identity.release_version(),
        bundle_manifest_digest = ?bundle_identity.bundle_manifest_digest(),
        bundle_manifest_mismatch_reason = ?bundle_identity.mismatch_reason(),
        "Groundhog release identity",
    );

    // --- Configuration ---
    let mut config = match &cli.config {
        Some(path) => {
            let contents = tokio::fs::read_to_string(path).await?;
            toml::from_str::<GatewayConfig>(&contents)?
        }
        None => GatewayConfig::default(),
    };
    apply_identity_cli_overrides(&mut config, &cli);

    if let Some(path) = &cli.runtime_manifest {
        let contents = read_runtime_manifest(path).await?;
        let manifest = thruput_json_rpc_types::runtime_manifest::parse_runtime_manifest(&contents)?;
        config.apply_runtime_manifest(&manifest)?;
    }

    let dump_effective_config = cli.dump_effective_config;
    let allow_remote = cli.pprof_allow_remote || heap_profiling::allow_remote_env();
    apply_cli_overrides(&mut config, cli);
    let network_info = config.network_info()?;
    if dump_effective_config {
        println!("{}", serde_json::to_string_pretty(&config)?);
        return Ok(());
    }
    let heap_dump = heap_profiling::initialize().await?;

    tracing::info!(
        network_hash = ?network_info.network_hash,
        faucet_network_id = ?config.network_id,
        faucet_genesis_hash = ?config.genesis_hash,
        faucet_configured = config.faucet_url.is_some(),
        listen = %config.listen_address,
        observability_listen = %config.observability_listen,
        health_required_backends = ?config.health_required_backends,
        backend_connect_timeout_ms = config.backend_connect_timeout_ms,
        scheduler_reconnect_grace_ms = config.scheduler_reconnect_grace_ms,
        shutdown_grace_ms = config.shutdown_grace_ms,
        scheduler = %config.scheduler_address,
        indexer = ?config.indexer_address,
        scheduler_lookup_pool_size = config.scheduler_lookup_pool_size.get(),
        x23_multiplex_connections = config.x23_multiplex_connections.get(),
        view_gateway = ?config.view_gateway_address,
        hogd_protocol_override = ?config.hogd_protocol_override,
        "loading configuration",
    );

    // --- Metrics registry ---
    let metrics = metrics::GatewayMetrics::new(
        BUILD_VERSION,
        bundle_identity.build_version().source_commit(),
    )?;
    metrics.set_rpc_connections_limit(config.max_connections);

    // --- Context (backend connections) ---
    let airdrop = airdrop::HostedAirdrop::from_config_with_metrics(
        &config,
        network_info.clone(),
        metrics.clone(),
    )
    .await?;
    let ctx = Arc::new(GatewayContext::new(config, metrics.clone()).await?);

    // --- Observability HTTP server ---
    let health = health::GatewayHealth::new_with_airdrop(
        &ctx.config,
        BUILD_VERSION,
        bundle_identity.build_version().source_commit(),
        airdrop.clone(),
    );
    let (health_shutdown_tx, health_shutdown_rx) = tokio::sync::oneshot::channel();
    let health_handle =
        health::spawn_health_probe_with_shutdown(ctx.clone(), health.clone(), health_shutdown_rx);
    let (observability_shutdown_tx, observability_shutdown_rx) = tokio::sync::oneshot::channel();
    let observability_server = observability::spawn_observability_server(
        &ctx.config.observability_listen,
        health,
        metrics.clone(),
        observability_shutdown_rx,
        heap_dump,
        allow_remote,
    )
    .await?;

    // --- Server ---
    let max_response_body_size = jsonrpsee::core::TEN_MB_SIZE_BYTES;
    let server_cfg = jsonrpsee::server::ServerConfig::builder()
        .max_connections(ctx.config.max_connections)
        .max_response_body_size(max_response_body_size)
        .set_batch_request_config(jsonrpsee::server::BatchRequestConfig::Limit(
            thruput_json_rpc_server::batch::MAX_RPC_BATCH_REQUESTS,
        ))
        .build();
    let http_mw = middleware::http_middleware(&ctx.config, metrics.clone());
    let rpc_metrics = metrics.clone();
    let batch_ctx = ctx.clone();
    let rpc_mw = jsonrpsee::core::middleware::RpcServiceBuilder::new()
        // Metrics must remain outermost: it observes each array once in response order.
        .layer_fn(move |inner| rpc_middleware::RpcMetricsService::new(inner, rpc_metrics.clone()))
        .layer_fn(move |inner| {
            batch_ctx
                .config
                .batch_service(inner, max_response_body_size)
        })
        .layer_fn(rpc_middleware::RpcTracingService::new);
    let server = jsonrpsee::server::Server::builder()
        .set_config(server_cfg)
        .set_http_middleware(http_mw)
        .set_rpc_middleware(rpc_mw)
        .build(&ctx.config.listen_address)
        .await?;

    let rpc_config = RpcServerConfig::default()
        .with_request_timeout_ms(ctx.config.request_timeout_ms.max(3))
        .with_max_events_per_page(ctx.config.max_events_per_page)
        .with_max_transactions_per_page(ctx.config.max_transactions_per_page)
        .with_max_cursor_bytes(ctx.config.max_cursor_bytes)
        .with_max_view_calldata_bytes(ctx.config.max_view_calldata_bytes)
        .with_view_gas_limit(ctx.config.view_gas_limit)
        .with_view_timeout_ms(ctx.config.view_timeout_ms)
        .with_view_batch(ctx.config.view_batch);
    let backend = GatewayBackend::new(ctx.clone());
    let api = NativeService::new(
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend,
        network_info.network_hash,
        rpc_config,
        BUILD_VERSION,
        native_capabilities(&ctx.config),
    )
    .with_bundle_identity(&bundle_identity);
    let rpc = native_module(api, airdrop)?;
    let handle = server.start(rpc);

    tracing::info!("hog-gateway listening");

    shutdown_signal().await?;
    let _ = health_shutdown_tx.send(());
    let _ = observability_shutdown_tx.send(());
    handle.stop()?;
    let shutdown_result = drain_shutdown(
        handle.stopped(),
        health_handle,
        observability_server.handle,
        Duration::from_millis(ctx.config.shutdown_grace_ms.max(1)),
    )
    .await;
    ctx.abort_request_workers();
    telemetry::shutdown(telemetry).await;
    shutdown_result?;

    Ok(())
}

fn cli_command(bundle_identity: &BundleIdentity) -> clap::Command {
    Cli::command().version(bundle_identity.to_string())
}

fn parse_cli(bundle_identity: &BundleIdentity) -> Cli {
    let matches = match cli_command(bundle_identity).try_get_matches() {
        Ok(matches) => matches,
        Err(error) => {
            if error.kind() == clap::error::ErrorKind::DisplayVersion
                && let Some(reason) = bundle_identity.terminal_safe_mismatch_reason()
            {
                eprintln!("warning: bundle manifest mismatch: {reason}");
            }
            error.exit()
        }
    };
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

fn apply_identity_cli_overrides(config: &mut GatewayConfig, cli: &Cli) {
    if let Some(network_id) = &cli.network_id {
        config.network_id = Some(network_id.clone());
    }
    if let Some(network_hash) = &cli.network_hash {
        config.network_hash = Some(network_hash.clone());
    }
    if let Some(genesis_hash) = cli.genesis_hash {
        config.genesis_hash = Some(genesis_hash);
    }
}

fn parse_network_hash(value: &str) -> Result<NetworkHashText, String> {
    NetworkHashText::try_from(value).map_err(|error| error.to_string())
}

fn parse_sha256_hash(value: &str) -> Result<SHA256Hash, String> {
    <thruput_xdr::types::SHA256Hash as thruput_json_rpc_types::base58::FixedIdentifier>::decode_base58(value)
        .map(SHA256Hash::from)
        .map_err(|error| error.to_string())
}

fn apply_cli_overrides(config: &mut GatewayConfig, cli: Cli) {
    if let Some(concurrency) = cli.rpc_batch_concurrency {
        config.rpc_batch_concurrency = concurrency;
    }
    if let Some(listen) = cli.listen {
        config.listen_address = listen;
    }
    if let Some(observability_listen) = cli.observability_listen {
        config.observability_listen = observability_listen;
    }
    if let Some(faucet_url) = cli.faucet_url {
        config.faucet_url = Some(faucet_url);
    }
    if let Some(required_backends) = cli.health_required_backends {
        config.health_required_backends = required_backends;
    }
    if let Some(max_age_ms) = cli.health_chain_tip_max_age_ms {
        config.health_chain_tip_max_age_ms = max_age_ms;
    }
    if let Some(interval_ms) = cli.health_probe_interval_ms {
        config.health_probe_interval_ms = interval_ms;
    }
    if let Some(timeout_ms) = cli.backend_connect_timeout_ms {
        config.backend_connect_timeout_ms = timeout_ms;
    }
    if let Some(grace_ms) = cli.scheduler_reconnect_grace_ms {
        config.scheduler_reconnect_grace_ms = grace_ms;
    }
    if let Some(grace_ms) = cli.shutdown_grace_ms {
        config.shutdown_grace_ms = grace_ms;
    }
    if let Some(scheduler) = cli.scheduler {
        config.scheduler_address = scheduler;
    }
    if let Some(indexer) = cli.indexer {
        config.indexer_address = Some(indexer);
    }
    if let Some(pool_size) = cli.scheduler_lookup_pool_size {
        config.scheduler_lookup_pool_size = pool_size;
    }
    if let Some(connections) = cli.x23_multiplex_connections {
        config.x23_multiplex_connections = connections;
    }
    if let Some(db_gateway) = cli.db_gateway {
        config.db_gateway_address = Some(db_gateway);
    }
    if let Some(view_gateway) = cli.view_gateway {
        config.view_gateway_address = Some(view_gateway);
    }
    if let Some(hogd_protocol) = cli.hogd_protocol {
        config.hogd_protocol_override = Some(hogd_protocol);
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

async fn drain_shutdown<R>(
    rpc_stopped: R,
    mut health_handle: tokio::task::JoinHandle<()>,
    mut observability_handle: tokio::task::JoinHandle<std::io::Result<()>>,
    grace: Duration,
) -> std::io::Result<()>
where
    R: Future<Output = ()>,
{
    let deadline = tokio::time::Instant::now()
        .checked_add(grace)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "shutdown grace exceeds the platform timer range",
            )
        })?;
    let mut rpc_stopped = std::pin::pin!(rpc_stopped);
    let mut rpc_done = false;
    let mut health_done = false;
    let mut observability_done = false;
    let mut first_error = None;

    while !(rpc_done && health_done && observability_done) {
        tokio::select! {
            biased;
            () = &mut rpc_stopped, if !rpc_done => rpc_done = true,
            result = &mut health_handle, if !health_done => {
                health_done = true;
                if let Err(error) = result {
                    first_error.get_or_insert_with(|| std::io::Error::other(error));
                }
            }
            result = &mut observability_handle, if !observability_done => {
                observability_done = true;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        first_error.get_or_insert(error);
                    }
                    Err(error) => {
                        first_error.get_or_insert_with(|| std::io::Error::other(error));
                    }
                }
            }
            () = tokio::time::sleep_until(deadline) => break,
        }
    }

    if !(rpc_done && health_done && observability_done) {
        tracing::warn!(
            grace = ?grace,
            rpc_pending = !rpc_done,
            health_pending = !health_done,
            observability_pending = !observability_done,
            "gateway shutdown grace expired; canceling owned tasks"
        );
        if !health_done {
            health_handle.abort();
        }
        if !observability_done {
            observability_handle.abort();
        }
    }

    first_error.map_or(Ok(()), Err)
}

async fn read_runtime_manifest(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let file = tokio::fs::File::open(path).await?;
    let mut contents = Vec::new();
    let mut reader = file.take(MAX_RUNTIME_MANIFEST_BYTES + 1);
    reader.read_to_end(&mut contents).await?;
    if contents.len() as u64 > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(format!(
            "runtime manifest {} exceeds {} bytes",
            path.display(),
            MAX_RUNTIME_MANIFEST_BYTES,
        )
        .into());
    }
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pprof_allow_remote_flag_parses() {
        let cli = Cli::try_parse_from(["hog-gateway", "--pprof-allow-remote"]).unwrap();
        assert!(cli.pprof_allow_remote);
    }

    #[test]
    fn x23_multiplex_connections_defaults_and_cli_override() {
        let mut config = GatewayConfig::default();
        assert_eq!(config.x23_multiplex_connections.get(), 4);
        config = toml::from_str("x23_multiplex_connections = 8").unwrap();
        apply_cli_overrides(&mut config, Cli::parse_from(["hog-gateway"]));
        assert_eq!(config.x23_multiplex_connections.get(), 8);
        apply_cli_overrides(
            &mut config,
            Cli::parse_from(["hog-gateway", "--x23-multiplex-connections", "2"]),
        );
        assert_eq!(config.x23_multiplex_connections.get(), 2);
    }

    #[test]
    fn x23_multiplex_connections_cli_and_toml_enforce_the_same_bounds() {
        for value in ["1", "4", "64"] {
            let config: GatewayConfig =
                toml::from_str(&format!("x23_multiplex_connections = {value}")).unwrap();
            let cli =
                Cli::try_parse_from(["hog-gateway", "--x23-multiplex-connections", value]).unwrap();
            assert_eq!(
                config.x23_multiplex_connections,
                cli.x23_multiplex_connections.unwrap()
            );
        }
        for value in ["0", "65", "-1"] {
            assert!(
                toml::from_str::<GatewayConfig>(&format!("x23_multiplex_connections = {value}"))
                    .is_err()
            );
            assert!(
                Cli::try_parse_from(["hog-gateway", "--x23-multiplex-connections", value]).is_err()
            );
        }
    }

    #[test]
    fn scheduler_lookup_pool_size_defaults_and_cli_override() {
        let mut config = GatewayConfig::default();
        assert_eq!(config.scheduler_lookup_pool_size.get(), 16);
        config = toml::from_str("scheduler_lookup_pool_size = 8").unwrap();
        apply_cli_overrides(&mut config, Cli::parse_from(["hog-gateway"]));
        assert_eq!(config.scheduler_lookup_pool_size.get(), 8);
        apply_cli_overrides(
            &mut config,
            Cli::parse_from(["hog-gateway", "--scheduler-lookup-pool-size", "4"]),
        );
        assert_eq!(config.scheduler_lookup_pool_size.get(), 4);
    }

    #[test]
    fn scheduler_lookup_pool_size_cli_and_toml_enforce_the_same_bounds() {
        for value in ["1", "4", "64", "68"] {
            let config: GatewayConfig =
                toml::from_str(&format!("scheduler_lookup_pool_size = {value}")).unwrap();
            let cli = Cli::try_parse_from(["hog-gateway", "--scheduler-lookup-pool-size", value])
                .unwrap();
            assert_eq!(
                config.scheduler_lookup_pool_size,
                cli.scheduler_lookup_pool_size.unwrap()
            );
        }
        for value in ["0", "69", "100000", "-1", "18446744073709551616"] {
            assert!(
                toml::from_str::<GatewayConfig>(&format!("scheduler_lookup_pool_size = {value}"))
                    .is_err()
            );
            assert!(
                Cli::try_parse_from(["hog-gateway", "--scheduler-lookup-pool-size", value])
                    .is_err()
            );
        }
        assert_eq!(
            toml::from_str::<GatewayConfig>("")
                .unwrap()
                .scheduler_lookup_pool_size
                .get(),
            16
        );
    }

    #[test]
    fn rpc_batch_concurrency_cli_overrides_toml_and_rejects_invalid_values() {
        for value in ["0", "1025"] {
            assert!(
                <super::Cli as clap::Parser>::try_parse_from([
                    "hog-gateway",
                    "--rpc-batch-concurrency",
                    value
                ])
                .is_err()
            );
        }
        let mut config =
            toml::from_str::<super::GatewayConfig>("rpc_batch_concurrency = 17").unwrap();
        let cli = <super::Cli as clap::Parser>::parse_from(["hog-gateway"]);
        super::apply_cli_overrides(&mut config, cli);
        assert_eq!(config.rpc_batch_concurrency.get(), 17);
        let cli = <super::Cli as clap::Parser>::parse_from([
            "hog-gateway",
            "--rpc-batch-concurrency",
            "1",
        ]);
        super::apply_cli_overrides(&mut config, cli);
        assert_eq!(config.rpc_batch_concurrency.get(), 1);
    }
    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    #[test]
    fn cli_accepts_complete_network_identity() {
        let cli = Cli::try_parse_from([
            "hog-gateway",
            "--network-id",
            "devnet-preview",
            "--network-hash",
            NetworkHashText::from([0x12; 32]).as_str(),
            "--genesis-hash",
            NetworkHashText::from([0x42; 32]).as_str(),
        ])
        .expect("bug: valid network identity flags should parse");
        let mut config = GatewayConfig::default();

        apply_identity_cli_overrides(&mut config, &cli);
        let network_info = config
            .network_info()
            .expect("bug: complete CLI identity should resolve");

        assert_eq!(network_info.network_hash.as_ref(), &[0x12; 32]);
        assert!(config.genesis_hash.is_some());
    }

    #[test]
    fn cli_rejects_malformed_genesis_hash() {
        assert!(
            Cli::try_parse_from([
                "hog-gateway",
                "--network-id",
                "devnet-preview",
                "--genesis-hash",
                "0x1234",
            ])
            .is_err()
        );
    }

    #[test]
    fn cli_rejects_malformed_network_hash() {
        assert!(Cli::try_parse_from(["hog-gateway", "--network-hash", "0x1234"]).is_err());
    }

    #[test]
    fn cli_overrides_faucet_url() {
        let cli = Cli::try_parse_from([
            "hog-gateway",
            "--faucet-url",
            "https://faucet.devnet.example",
        ])
        .expect("bug: valid faucet URL flag should parse");
        let mut config = GatewayConfig::default();

        apply_cli_overrides(&mut config, cli);

        assert_eq!(
            config.faucet_url.as_deref(),
            Some("https://faucet.devnet.example")
        );
    }

    #[test]
    fn native_registration_follows_configured_sources() {
        for mask in 0..8 {
            let config = GatewayConfig {
                indexer_address: (mask & 1 != 0).then(|| "indexer:1".into()),
                db_gateway_address: (mask & 2 != 0).then(|| "db:1".into()),
                view_gateway_address: (mask & 4 != 0).then(|| "view:1".into()),
                ..GatewayConfig::default()
            };
            let methods = native_capabilities(&config).method_names();
            for method in [
                "getVersionInfo",
                "getNetworkInfo",
                "getServerLimits",
                "sendTransaction",
            ] {
                assert!(methods.contains(&method));
            }
            for method in [
                "getTransactionReceipt",
                "getTransactionStatus",
                "getBlockByHeight",
                "getEvents",
                "getTransactionsByRootAddress",
                "getTransactionsByBlock",
            ] {
                assert_eq!(methods.contains(&method), mask & 1 != 0, "{method}: {mask}");
            }
            for method in [
                "getChainTip",
                "getChainConfig",
                "getStorage",
                "getBalance",
                "getRegisteredAuth",
            ] {
                assert_eq!(methods.contains(&method), mask & 2 != 0, "{method}: {mask}");
            }
            for method in ["simulateInvocation", "simulateInvocations"] {
                assert_eq!(methods.contains(&method), mask & 4 != 0, "{method}: {mask}");
            }
            assert!(!methods.iter().any(|name| name.starts_with("groundhog_")));
        }
    }

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn shutdown_joins_subsystems_that_finish_within_grace() {
        let health = tokio::spawn(async {});
        let observability = tokio::spawn(async { Ok(()) });

        drain_shutdown(async {}, health, observability, Duration::from_secs(1))
            .await
            .expect("bug: graceful shutdown should succeed");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_aborts_owned_tasks_at_one_total_deadline() {
        let (health_dropped_tx, health_dropped_rx) = tokio::sync::oneshot::channel();
        let health = tokio::spawn(async move {
            let _notice = NotifyOnDrop(Some(health_dropped_tx));
            std::future::pending::<()>().await;
        });
        let (observability_dropped_tx, observability_dropped_rx) = tokio::sync::oneshot::channel();
        let observability = tokio::spawn(async move {
            let _notice = NotifyOnDrop(Some(observability_dropped_tx));
            std::future::pending::<std::io::Result<()>>().await
        });
        tokio::task::yield_now().await;

        let started_at = tokio::time::Instant::now();
        drain_shutdown(
            async {
                tokio::time::sleep(Duration::from_secs(7)).await;
            },
            health,
            observability,
            Duration::from_secs(10),
        )
        .await
        .expect("bug: hard shutdown should not manufacture an error");
        assert_eq!(started_at.elapsed(), Duration::from_secs(10));
        health_dropped_rx
            .await
            .expect("bug: health task should be canceled and dropped");
        observability_dropped_rx
            .await
            .expect("bug: observability task should be canceled and dropped");
    }

    #[tokio::test]
    async fn shutdown_rejects_an_unrepresentable_deadline() {
        let health = tokio::spawn(async {});
        let observability = tokio::spawn(async { Ok(()) });

        let error = drain_shutdown(async {}, health, observability, Duration::MAX)
            .await
            .expect_err("bug: an unrepresentable shutdown grace should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
