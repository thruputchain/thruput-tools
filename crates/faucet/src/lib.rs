pub mod airdrop;
/// Faucet service configuration types.
pub mod config;
/// Faucet signing-key source configuration and loaders.
pub mod key_source;
mod error;
pub mod policy;
mod utils;
pub mod wallet_create;

use axum::{
    Json, Router,
    extract::{FromRef, State},
    http::{HeaderValue, Method, header},
    routing::{get, post},
};
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use serde::Serialize;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

pub const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1:8081";

#[derive(Clone)]
pub struct FaucetConfig {
    pub listen_address: String,
    pub cors_allowed_origins: Vec<HeaderValue>,
    pub network_descriptor: NetworkDescriptor,
    pub airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
    pub wallet_create_submitter: Arc<dyn wallet_create::WalletCreateSubmitter>,
    pub policy_config: config::PolicyRuntimeConfig,
    pub policy_store: Arc<dyn policy::FaucetPolicyStore>,
}

impl FaucetConfig {
    pub fn new(
        network_descriptor: NetworkDescriptor,
        airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
        wallet_create_submitter: Arc<dyn wallet_create::WalletCreateSubmitter>,
    ) -> Self {
        Self {
            listen_address: DEFAULT_LISTEN_ADDRESS.to_owned(),
            cors_allowed_origins: Vec::new(),
            network_descriptor,
            airdrop_submitter,
            wallet_create_submitter,
            policy_config: config::PolicyRuntimeConfig::default(),
            policy_store: Arc::new(policy::InMemoryFaucetPolicyStore::new()),
        }
    }
}

impl fmt::Debug for FaucetConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FaucetConfig")
            .field("listen_address", &self.listen_address)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("network_descriptor", &self.network_descriptor)
            .field("airdrop_submitter", &"<dyn AirdropSubmitter>")
            .field("wallet_create_submitter", &"<dyn WalletCreateSubmitter>")
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
    airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
    wallet_create_service: Arc<wallet_create::WalletCreateService>,
}

impl FromRef<AppState> for Arc<dyn airdrop::AirdropSubmitter> {
    fn from_ref(state: &AppState) -> Self {
        state.airdrop_submitter.clone()
    }
}

impl FromRef<AppState> for Arc<wallet_create::WalletCreateService> {
    fn from_ref(state: &AppState) -> Self {
        state.wallet_create_service.clone()
    }
}

pub fn app(config: FaucetConfig) -> Router {
    let cors_allowed_origins = config.cors_allowed_origins.clone();
    let wallet_create_service = Arc::new(wallet_create::WalletCreateService::new(
        config.network_descriptor.clone(),
        config.airdrop_submitter.clone(),
        config.wallet_create_submitter,
        config.policy_store,
        config.policy_config.clone(),
    ));
    let state = AppState {
        network_descriptor: Arc::new(config.network_descriptor),
        airdrop_submitter: config.airdrop_submitter,
        wallet_create_service,
    };
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/network", get(network))
        .route("/v1/airdrop", post(airdrop::request_airdrop))
        .route("/v1/create-wallet", post(wallet_create::create_wallet))
        .with_state(state);
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
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode};
    use groundhog_json_rpc_types::hex::Hex0x;
    use groundhog_json_rpc_types::network_descriptor::{
        NETWORK_DESCRIPTOR_SCHEMA_VERSION, NetworkDescriptor,
    };
    use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
    use groundhog_json_rpc_types::transaction::TxHash;
    use groundhog_json_rpc_types::{BlockHeight, ContractAddress};
    use groundhog_xdr::types::{Ed25519PublicKey, SHA256Hash};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;
    use tower::ServiceExt;

    use super::{DEFAULT_LISTEN_ADDRESS, FaucetConfig, airdrop, app, policy, wallet_create};

    #[test]
    fn default_config_uses_local_listen_address() {
        let config = test_config();

        assert_eq!(config.listen_address, DEFAULT_LISTEN_ADDRESS);
    }

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

    #[tokio::test]
    async fn serve_stops_when_shutdown_future_resolves() {
        let mut config = test_config();
        config.listen_address = "127.0.0.1:0".into();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(super::serve_with_shutdown(config, async move {
            let _ = shutdown_rx.await;
        }));

        shutdown_tx
            .send(())
            .expect("bug: shutdown receiver should be alive");
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("bug: server should stop after shutdown future resolves")
            .expect("bug: server task should join")
            .expect("bug: server should shut down cleanly");
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = app(test_config())
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("bug: health request should build"),
            )
            .await
            .expect("bug: health request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("bug: health body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: health body should be JSON");
        assert_eq!(json, serde_json::json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn network_returns_public_descriptor() {
        let descriptor = test_network_descriptor();
        let response = app(FaucetConfig::new(
            descriptor.clone(),
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::success()),
        ))
        .oneshot(
            Request::builder()
                .uri("/v1/network")
                .body(Body::empty())
                .expect("bug: network request should build"),
        )
        .await
        .expect("bug: network request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: network body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: network body should be JSON");
        assert_eq!(json["schemaVersion"], NETWORK_DESCRIPTOR_SCHEMA_VERSION);
        assert_eq!(json["networkId"], "devnet-preview");
        assert!(json["endpoints"].get("faucetUrl").is_none());
        assert_eq!(
            json["nativeHog"]["transferEntrypoint"],
            "0x63285b3523f9dc3f"
        );
        assert_eq!(json["walletCreate"]["prefundRawAmount"], "100000000");
        assert!(json.get("bootstrapAccountDerivation").is_none());
        assert!(json.get("bootstrapAccounts").is_none());
        assert!(json.get("privateKey").is_none());

        let returned: NetworkDescriptor =
            serde_json::from_slice(&body).expect("bug: network body should deserialize");
        assert_eq!(returned, descriptor);
    }

    #[tokio::test]
    async fn airdrop_validates_request_then_submits() {
        let response = app(test_config())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/airdrop")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "recipient": format!("0x{}", "11".repeat(32)),
                            "clientNonce": "0x01",
                            "networkId": "devnet-preview",
                            "genesisHash": format!("0x{}", "00".repeat(32))
                        })
                        .to_string(),
                    ))
                    .expect("bug: airdrop request should build"),
            )
            .await
            .expect("bug: airdrop request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["rawAmount"], "100000000");
        assert_eq!(json["height"], 42);
        assert_eq!(json["status"]["result"], "ok");
    }

    #[tokio::test]
    async fn airdrop_rejects_malformed_recipient() {
        let response = app(test_config())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/airdrop")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "recipient": "0xdeadbeef",
                            "clientNonce": "0x01",
                            "networkId": "devnet-preview",
                            "genesisHash": format!("0x{}", "00".repeat(32))
                        })
                        .to_string(),
                    ))
                    .expect("bug: airdrop request should build"),
            )
            .await
            .expect("bug: airdrop request should complete");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn airdrop_submit_failure_returns_bad_gateway() {
        let response = app(FaucetConfig::new(
            test_network_descriptor(),
            Arc::new(MockAirdropSubmitter::failure()),
            Arc::new(MockWalletCreateSubmitter::success()),
        ))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/airdrop")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "recipient": format!("0x{}", "11".repeat(32)),
                        "clientNonce": "0x01",
                        "networkId": "devnet-preview",
                        "genesisHash": format!("0x{}", "00".repeat(32))
                    })
                    .to_string(),
                ))
                .expect("bug: airdrop request should build"),
        )
        .await
        .expect("bug: airdrop request should complete");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_submit_failed");
    }

    #[tokio::test]
    async fn wallet_create_prepare_route_is_removed() {
        let response = app(test_config())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/create-wallet/prepare")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "ownerPublicKey": format!("0x{}", "33".repeat(32)),
                            "deployNonce": format!("0x{}", "44".repeat(32))
                        })
                        .to_string(),
                    ))
                    .expect("bug: wallet-create prepare request should build"),
            )
            .await
            .expect("bug: wallet-create prepare request should complete");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wallet_create_validates_request_then_submits() {
        let response = app(test_config())
            .oneshot(request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/create-wallet")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "ownerPublicKey": format!("0x{}", "33".repeat(32)),
                            "deployNonce": format!("0x{}", "44".repeat(32)),
                            "clientNonce": "0x01",
                            "ownerSignature": format!("0x{}", "66".repeat(64)),
                            "networkId": "devnet-preview",
                            "genesisHash": format!("0x{}", "00".repeat(32))
                        })
                        .to_string(),
                    ))
                    .expect("bug: wallet-create request should build"),
            ))
            .await
            .expect("bug: wallet-create request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: wallet-create body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: wallet-create body should be JSON");
        assert_eq!(json["attemptStatus"], "created");
        assert_eq!(json["walletAddress"], format!("0x{}", "55".repeat(32)));
        assert_eq!(json["prefundTxHash"], format!("0x{}", "22".repeat(32)));
        assert_eq!(json["txHash"], format!("0x{}", "22".repeat(32)));
        assert_eq!(json["height"], 42);
        assert_eq!(json["status"]["result"], "ok");
    }

    #[tokio::test]
    async fn wallet_create_retry_returns_recorded_created_attempt() {
        let config = test_config();
        let response = wallet_create_request(&config).await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = wallet_create_request(&config).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: wallet-create body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: wallet-create body should be JSON");
        assert_eq!(json["attemptStatus"], "created");
        assert_eq!(json["txHash"], format!("0x{}", "22".repeat(32)));
    }

    #[tokio::test]
    async fn wallet_create_duplicate_in_flight_returns_pending_attempt() {
        let config = test_config_with_store();
        seed_wallet_create_attempt(
            &config,
            policy::WalletCreateAttemptStatus::PrefundPending,
            None,
            None,
        );

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: wallet-create body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: wallet-create body should be JSON");
        assert_eq!(json["attemptStatus"], "prefundPending");
        assert!(json.get("txHash").is_none());
    }

    #[tokio::test]
    async fn wallet_create_quota_rejects_before_prefund() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let mut config = FaucetConfig::new(
            test_network_descriptor(),
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        config.policy_config.wallet_create.owner_quota_max = 0;

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(airdrop.calls(), 0);
    }

    #[tokio::test]
    async fn wallet_create_rejects_invalid_owner_proof_before_prefund() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::verify_failure()),
        );

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(airdrop.calls(), 0);
        assert!(stored_wallet_create_attempt(&config).is_none());
    }

    #[tokio::test]
    async fn wallet_create_failed_prefund_records_retryable_attempt() {
        let config = test_config_with_submitters(
            Arc::new(MockAirdropSubmitter::failure()),
            Arc::new(MockWalletCreateSubmitter::success()),
        );

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(
            attempt.status,
            policy::WalletCreateAttemptStatus::FailedRetryable
        );
        assert_eq!(
            attempt.error_code.as_deref(),
            Some("wallet_create_prefund_failed")
        );
        assert!(attempt.prefund_tx_hash.is_none());
    }

    #[tokio::test]
    async fn wallet_create_retries_failed_prefund_attempt() {
        let airdrop = Arc::new(MockAirdropSubmitter::fail_once());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        let response = wallet_create_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 2);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::WalletCreateAttemptStatus::Created);
    }

    #[tokio::test]
    async fn wallet_create_failed_create_records_retryable_attempt() {
        let config = test_config_with_submitters(
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::failure()),
        );

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(
            attempt.status,
            policy::WalletCreateAttemptStatus::FailedRetryable
        );
        assert_eq!(
            attempt.error_code.as_deref(),
            Some("wallet_create_submit_failed")
        );
        assert_eq!(
            attempt.prefund_tx_hash.as_deref(),
            Some(format!("0x{}", "22".repeat(32)).as_str())
        );
        assert!(attempt.create_tx_hash.is_none());
    }

    #[tokio::test]
    async fn wallet_create_retries_failed_create_without_reprefunding() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let wallet_create = Arc::new(MockWalletCreateSubmitter::fail_once());
        let config = test_config_with_submitters(airdrop.clone(), wallet_create);
        let response = wallet_create_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 1);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::WalletCreateAttemptStatus::Created);
    }

    #[tokio::test]
    async fn wallet_create_rejects_malformed_owner_key() {
        let response = app(test_config())
            .oneshot(request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/create-wallet")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "ownerPublicKey": "0xdeadbeef",
                            "deployNonce": format!("0x{}", "44".repeat(32)),
                            "clientNonce": "0x01",
                            "ownerSignature": format!("0x{}", "66".repeat(64)),
                            "networkId": "devnet-preview",
                            "genesisHash": format!("0x{}", "00".repeat(32))
                        })
                        .to_string(),
                    ))
                    .expect("bug: wallet-create malformed request should build"),
            ))
            .await
            .expect("bug: wallet-create malformed request should complete");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn wallet_create_submit_failure_returns_bad_gateway() {
        let response = app(FaucetConfig::new(
            test_network_descriptor(),
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::failure()),
        ))
        .oneshot(request_with_connect_info(
            Request::builder()
                .method("POST")
                .uri("/v1/create-wallet")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "ownerPublicKey": format!("0x{}", "33".repeat(32)),
                        "deployNonce": format!("0x{}", "44".repeat(32)),
                        "clientNonce": "0x01",
                        "ownerSignature": format!("0x{}", "66".repeat(64)),
                        "networkId": "devnet-preview",
                        "genesisHash": format!("0x{}", "00".repeat(32))
                    })
                    .to_string(),
                ))
                .expect("bug: wallet-create request should build"),
        ))
        .await
        .expect("bug: wallet-create request should complete");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: wallet-create body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: wallet-create body should be JSON");
        assert_eq!(json["error"]["code"], "wallet_create_submit_failed");
    }

    fn test_config() -> FaucetConfig {
        test_config_with_store()
    }

    fn test_config_with_store() -> FaucetConfig {
        FaucetConfig::new(
            test_network_descriptor(),
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::success()),
        )
    }

    fn test_config_with_submitters(
        airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
        wallet_create_submitter: Arc<dyn wallet_create::WalletCreateSubmitter>,
    ) -> FaucetConfig {
        FaucetConfig::new(
            test_network_descriptor(),
            airdrop_submitter,
            wallet_create_submitter,
        )
    }

    async fn wallet_create_request(config: &FaucetConfig) -> axum::response::Response {
        app(config.clone())
            .oneshot(request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/create-wallet")
                    .header("content-type", "application/json")
                    .body(Body::from(wallet_create_body().to_string()))
                    .expect("bug: wallet-create request should build"),
            ))
            .await
            .expect("bug: wallet-create request should complete")
    }

    fn wallet_create_body() -> serde_json::Value {
        serde_json::json!({
            "ownerPublicKey": format!("0x{}", "33".repeat(32)),
            "deployNonce": format!("0x{}", "44".repeat(32)),
            "clientNonce": "0x01",
            "ownerSignature": format!("0x{}", "66".repeat(64)),
            "networkId": "devnet-preview",
            "genesisHash": format!("0x{}", "00".repeat(32))
        })
    }

    fn request_with_connect_info(mut request: Request<Body>) -> Request<Body> {
        let addr: SocketAddr = "127.0.0.1:12345"
            .parse()
            .expect("bug: test socket address should parse");
        request.extensions_mut().insert(ConnectInfo(addr));
        request
    }

    fn wallet_create_attempt_key() -> policy::WalletCreateAttemptKey {
        policy::WalletCreateAttemptKey {
            network_id: "devnet-preview".into(),
            genesis_hash: format!("0x{}", "00".repeat(32)),
            owner_public_key: format!("0x{}", "33".repeat(32)),
            client_nonce: "01".into(),
        }
    }

    fn seed_wallet_create_attempt(
        config: &FaucetConfig,
        status: policy::WalletCreateAttemptStatus,
        prefund_tx_hash: Option<String>,
        create_tx_hash: Option<String>,
    ) {
        let now = SystemTime::UNIX_EPOCH;
        config
            .policy_store
            .put_wallet_create_attempt(policy::WalletCreateAttempt {
                key: wallet_create_attempt_key(),
                deploy_nonce: format!("0x{}", "44".repeat(32)),
                wallet_address: format!("0x{}", "55".repeat(32)),
                wallet_contract_hash: format!("0x{}", "77".repeat(32)),
                prefund_raw_amount: "100000000".into(),
                status,
                prefund_tx_hash,
                prefund_height: None,
                create_tx_hash,
                create_height: None,
                error_code: None,
                created_at: now,
                updated_at: now,
                expires_at: None,
            })
            .expect("bug: test attempt insert should succeed");
    }

    fn stored_wallet_create_attempt(config: &FaucetConfig) -> Option<policy::WalletCreateAttempt> {
        config
            .policy_store
            .get_wallet_create_attempt(&wallet_create_attempt_key())
            .expect("bug: test attempt fetch should succeed")
    }

    fn test_network_descriptor() -> NetworkDescriptor {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": "groundhog-network",
            "networkId": "devnet-preview",
            "protocol": "x23",
            "genesisHash": format!("0x{}", "00".repeat(32)),
            "endpoints": {
                "rpcUrl": "https://rpc.devnet.example"
            },
            "nativeHog": {
                "tokenAddress": format!("0x{}", "00".repeat(32)),
                "decimals": 7,
                "transferEntrypoint": "0x63285b3523f9dc3f"
            },
            "faucet": {
                "walletAddress": format!("0x{}", "00".repeat(32))
            },
            "walletCreate": {
                "prefundRawAmount": "100000000",
                "walletContractHash": format!("0x{}", "00".repeat(32))
            }
        }))
        .expect("bug: test network descriptor should deserialize")
    }

    struct MockAirdropSubmitter {
        failures_remaining: Mutex<usize>,
        calls: Mutex<usize>,
    }

    impl MockAirdropSubmitter {
        fn success() -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
            }
        }

        fn failure() -> Self {
            Self {
                failures_remaining: Mutex::new(usize::MAX),
                calls: Mutex::new(0),
            }
        }

        fn fail_once() -> Self {
            Self {
                failures_remaining: Mutex::new(1),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("bug: calls lock should succeed")
        }
    }

    #[async_trait::async_trait]
    impl airdrop::AirdropSubmitter for MockAirdropSubmitter {
        async fn submit_airdrop(
            &self,
            _recipient: ContractAddress,
        ) -> Result<airdrop::AirdropSubmission, airdrop::AirdropSubmitError> {
            *self.calls.lock().expect("bug: calls lock should succeed") += 1;
            let mut failures = self
                .failures_remaining
                .lock()
                .expect("bug: failures lock should succeed");
            if *failures > 0 {
                if *failures != usize::MAX {
                    *failures -= 1;
                }
                return Err(airdrop::AirdropSubmitError::BuildTransaction("boom".into()));
            }
            Ok(airdrop::AirdropSubmission {
                raw_amount: "100000000".into(),
                tx_hash: test_tx_hash(),
                receipt: TransactionReceipt {
                    tx_hash: test_tx_hash(),
                    height: BlockHeight::from(42),
                    status: ExecStatus::Ok,
                    events: vec![],
                    return_data: None,
                    abort_data: None,
                },
            })
        }
    }

    fn test_tx_hash() -> TxHash {
        TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x22; 32])))
    }

    struct MockWalletCreateSubmitter {
        failures_remaining: Mutex<usize>,
        verify_fails: bool,
    }

    impl MockWalletCreateSubmitter {
        fn success() -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                verify_fails: false,
            }
        }

        fn failure() -> Self {
            Self {
                failures_remaining: Mutex::new(usize::MAX),
                verify_fails: false,
            }
        }

        fn fail_once() -> Self {
            Self {
                failures_remaining: Mutex::new(1),
                verify_fails: false,
            }
        }

        fn verify_failure() -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                verify_fails: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl wallet_create::WalletCreateSubmitter for MockWalletCreateSubmitter {
        fn prepare_wallet_create(
            &self,
            _owner_public_key: Ed25519PublicKey,
            _deploy_nonce: SHA256Hash,
        ) -> Result<wallet_create::PreparedWalletCreate, wallet_create::WalletCreateError> {
            Ok(wallet_create::PreparedWalletCreate {
                wallet_address: Hex0x(groundhog_xdr::types::ContractAddress::from([0x55; 32])),
                prefund_raw_amount: "100000000".into(),
                wallet_contract_hash: Hex0x(SHA256Hash::from([0x77; 32])),
            })
        }

        fn verify_wallet_create_request(
            &self,
            owner_public_key: Ed25519PublicKey,
            deploy_nonce: SHA256Hash,
            _client_nonce: &[u8],
            _network_id: &str,
            _genesis_hash: SHA256Hash,
            _owner_signature: &[u8],
        ) -> Result<wallet_create::PreparedWalletCreate, wallet_create::WalletCreateError> {
            if self.verify_fails {
                return Err(wallet_create::WalletCreateError::OwnerSignatureMismatch);
            }
            self.prepare_wallet_create(owner_public_key, deploy_nonce)
        }

        async fn create_wallet(
            &self,
            _owner_public_key: Ed25519PublicKey,
            _deploy_nonce: SHA256Hash,
            _client_nonce: Vec<u8>,
            _network_id: String,
            _genesis_hash: SHA256Hash,
            _owner_signature: Vec<u8>,
        ) -> Result<wallet_create::WalletCreateSubmission, wallet_create::WalletCreateError>
        {
            let mut failures = self
                .failures_remaining
                .lock()
                .expect("bug: failures lock should succeed");
            if *failures > 0 {
                if *failures != usize::MAX {
                    *failures -= 1;
                }
                return Err(wallet_create::WalletCreateError::BuildTransaction(
                    "boom".into(),
                ));
            }
            Ok(wallet_create::WalletCreateSubmission {
                wallet_address: Hex0x(groundhog_xdr::types::ContractAddress::from([0x55; 32])),
                tx_hash: test_tx_hash(),
                receipt: TransactionReceipt {
                    tx_hash: test_tx_hash(),
                    height: BlockHeight::from(42),
                    status: ExecStatus::Ok,
                    events: vec![],
                    return_data: None,
                    abort_data: None,
                },
            })
        }
    }
}
