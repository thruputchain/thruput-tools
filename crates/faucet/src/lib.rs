pub mod airdrop;
/// Faucet service configuration types.
pub mod config;
mod error;
/// Faucet signing-key source configuration and loaders.
pub mod key_source;
pub mod policy;
/// Optional OpenTelemetry setup and shutdown.
pub mod telemetry;
mod utils;

use axum::{
    Json, Router,
    extract::{FromRef, MatchedPath, Request, State},
    http::{HeaderValue, Method, header},
    middleware::Next,
    response::Response,
    routing::{get, post},
};
use serde::Serialize;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use thruput_json_rpc_types::network_descriptor::NetworkDescriptor;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1:8081";

async fn trace_request(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unknown", MatchedPath::as_str);
    let span = tracing::info_span!(
        target: "groundhog_otel",
        "http",
        otel.name = route,
        otel.kind = "server",
        http.route = route,
        http.request.method = %request.method(),
        http.response.status_code = tracing::field::Empty
    );
    if span.is_disabled() {
        return next.run(request).await;
    }
    let _ = span.set_parent(telemetry::extract(request.headers()));
    let response = next.run(request).instrument(span.clone()).await;
    span.record("http.response.status_code", response.status().as_u16());
    response
}

#[derive(Clone)]
pub struct FaucetConfig {
    pub listen_address: String,
    pub cors_allowed_origins: Vec<HeaderValue>,
    pub network_descriptor: NetworkDescriptor,
    pub airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
    pub policy_config: config::PolicyRuntimeConfig,
    pub policy_store: Arc<dyn policy::FaucetPolicyStore>,
}

impl FaucetConfig {
    pub fn new(
        network_descriptor: NetworkDescriptor,
        airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
    ) -> Result<Self, config::FaucetConfigError> {
        let descriptor_airdrop = network_descriptor
            .airdrop
            .as_ref()
            .ok_or(config::FaucetConfigError::MissingAirdropDescriptor)?;
        let descriptor_airdrop_raw_amount =
            descriptor_airdrop.raw_amount.parse::<u128>().map_err(|_| {
                config::FaucetConfigError::InvalidDescriptorAmount {
                    field: "airdrop.rawAmount",
                    value: descriptor_airdrop.raw_amount.clone(),
                }
            })?;
        if airdrop_submitter.raw_amount() != descriptor_airdrop_raw_amount {
            return Err(config::FaucetConfigError::SubmitterAmountMismatch {
                submitter: "airdrop submitter",
                expected: descriptor_airdrop_raw_amount,
                actual: airdrop_submitter.raw_amount(),
            });
        }
        Ok(Self {
            listen_address: DEFAULT_LISTEN_ADDRESS.to_owned(),
            cors_allowed_origins: Vec::new(),
            network_descriptor,
            airdrop_submitter,
            policy_config: config::PolicyRuntimeConfig::default(),
            policy_store: Arc::new(policy::InMemoryFaucetPolicyStore::new()),
        })
    }
}

impl fmt::Debug for FaucetConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FaucetConfig")
            .field("listen_address", &self.listen_address)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("network_descriptor", &self.network_descriptor)
            .field("airdrop_submitter", &"<dyn AirdropSubmitter>")
            .field("policy_config", &self.policy_config)
            .field("policy_store", &"<dyn FaucetPolicyStore>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Clone)]
struct AppState {
    network_descriptor: Arc<NetworkDescriptor>,
    airdrop_service: Arc<airdrop::AirdropService>,
}

impl FromRef<AppState> for Arc<airdrop::AirdropService> {
    fn from_ref(state: &AppState) -> Self {
        state.airdrop_service.clone()
    }
}

pub fn app(config: FaucetConfig) -> Router {
    let cors_allowed_origins = config.cors_allowed_origins.clone();
    let airdrop_service = Arc::new(airdrop::AirdropService::new(
        config.network_descriptor.clone(),
        config.airdrop_submitter,
        config.policy_store.clone(),
        config.policy_config.clone(),
    ));
    let state = AppState {
        network_descriptor: Arc::new(config.network_descriptor),
        airdrop_service,
    };
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/network", get(network))
        .route("/v1/airdrop", post(airdrop::request_airdrop))
        .with_state(state)
        .layer(axum::middleware::from_fn(trace_request));
    if cors_allowed_origins.is_empty() {
        router
    } else {
        router.layer(
            CorsLayer::new()
                .allow_origin(cors_allowed_origins)
                .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
                .allow_headers([header::CONTENT_TYPE]),
        )
    }
}

/// Serve the faucet API until SIGTERM or Ctrl-C, draining in-flight requests.
///
/// Fails at startup when shutdown signal handlers cannot be registered, so a
/// deployment never runs without a graceful shutdown path.
pub async fn serve(config: FaucetConfig) -> Result<(), std::io::Error> {
    let shutdown = shutdown_signal()?;
    serve_with_shutdown(config, shutdown).await
}

/// Serve the faucet API until `shutdown` resolves, draining in-flight requests.
pub async fn serve_with_shutdown(
    config: FaucetConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(&config.listen_address).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(listen = %local_addr, "faucet service listening");
    axum::serve(
        listener,
        app(config).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

/// Future that resolves once SIGTERM or SIGINT (Ctrl-C) is received.
///
/// Handlers are registered before the future is returned, so registration
/// failure is a startup error rather than a service that can never drain.
#[cfg(unix)]
fn shutdown_signal() -> Result<impl Future<Output = ()> + Send + 'static, std::io::Error> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint.recv() => {},
        }
        tracing::info!("shutdown signal received; draining in-flight requests");
    })
}

/// Future that resolves once Ctrl-C is received.
#[cfg(not(unix))]
fn shutdown_signal() -> Result<impl Future<Output = ()> + Send + 'static, std::io::Error> {
    Ok(async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
        tracing::info!("shutdown signal received; draining in-flight requests");
    })
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn network(State(state): State<AppState>) -> Json<NetworkDescriptor> {
    Json((*state.network_descriptor).clone())
}

#[cfg(test)]
mod tests {

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_signal_resolves_on_sigterm_and_not_before() {
        let mut shutdown = Box::pin(
            super::shutdown_signal().expect("bug: signal registration should succeed in tests"),
        );

        let before_signal =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut shutdown).await;
        assert!(
            before_signal.is_err(),
            "shutdown future resolved without a signal"
        );

        let status = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .expect("bug: kill should run");
        assert!(status.success(), "kill -TERM should succeed");

        tokio::time::timeout(std::time::Duration::from_secs(5), shutdown)
            .await
            .expect("bug: shutdown future should resolve after SIGTERM");
    }
}
