pub mod airdrop;
/// Faucet service configuration types.
pub mod config;
mod error;
/// Faucet signing-key source configuration and loaders.
pub mod key_source;
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
    wallet_prefund_submitter: Arc<dyn airdrop::AirdropSubmitter>,
    pub wallet_create_submitter: Arc<dyn wallet_create::WalletCreateSubmitter>,
    pub policy_config: config::PolicyRuntimeConfig,
    pub policy_store: Arc<dyn policy::FaucetPolicyStore>,
}

impl FaucetConfig {
    pub fn new(
        network_descriptor: NetworkDescriptor,
        airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
        wallet_prefund_submitter: Arc<dyn airdrop::AirdropSubmitter>,
        wallet_create_submitter: Arc<dyn wallet_create::WalletCreateSubmitter>,
    ) -> Self {
        let descriptor_airdrop_raw_amount = network_descriptor
            .airdrop
            .as_ref()
            .expect("faucet network descriptor must advertise airdrop policy")
            .raw_amount
            .parse::<u128>()
            .expect("airdrop amount in network descriptor must be a valid u128");
        assert_eq!(
            airdrop_submitter.raw_amount(),
            descriptor_airdrop_raw_amount,
            "airdrop submitter amount must match network descriptor"
        );
        let descriptor_prefund_raw_amount = network_descriptor
            .wallet_create
            .prefund_raw_amount
            .parse::<u128>()
            .expect("wallet prefund amount in network descriptor must be a valid u128");
        assert_eq!(
            wallet_prefund_submitter.raw_amount(),
            descriptor_prefund_raw_amount,
            "wallet prefund submitter amount must match network descriptor"
        );

        Self {
            listen_address: DEFAULT_LISTEN_ADDRESS.to_owned(),
            cors_allowed_origins: Vec::new(),
            network_descriptor,
            airdrop_submitter,
            wallet_prefund_submitter,
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
            .field("wallet_prefund_submitter", &"<dyn AirdropSubmitter>")
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
    airdrop_service: Arc<airdrop::AirdropService>,
    wallet_create_service: Arc<wallet_create::WalletCreateService>,
}

impl FromRef<AppState> for Arc<airdrop::AirdropService> {
    fn from_ref(state: &AppState) -> Self {
        state.airdrop_service.clone()
    }
}

impl FromRef<AppState> for Arc<wallet_create::WalletCreateService> {
    fn from_ref(state: &AppState) -> Self {
        state.wallet_create_service.clone()
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
    let wallet_create_service = Arc::new(wallet_create::WalletCreateService::new(
        config.network_descriptor.clone(),
        config.wallet_prefund_submitter,
        config.wallet_create_submitter,
        config.policy_store,
        config.policy_config.clone(),
    ));
    let state = AppState {
        network_descriptor: Arc::new(config.network_descriptor),
        airdrop_service,
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
    use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};
    use groundhog_json_rpc_types::{BlockHeight, ContractAddress};
    use groundhog_xdr::types::{Ed25519PublicKey, SHA256Hash};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};
    use tower::ServiceExt;

    use super::{
        DEFAULT_LISTEN_ADDRESS, FaucetConfig, airdrop, app, config, policy, wallet_create,
    };

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
        assert_eq!(json["airdrop"]["rawAmount"], "100000000");
        assert!(json["airdrop"].get("amountHog").is_none());
        assert_eq!(json["airdrop"]["recipientCooldownSeconds"], 43_200);
        assert_eq!(json["airdrop"]["clientQuotaMax"], 10);
        assert_eq!(json["airdrop"]["clientQuotaWindowSeconds"], 3_600);
        assert_eq!(json["airdrop"]["maxPendingPerRecipient"], 1);
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
            .oneshot(request_with_connect_info(
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
            ))
            .await
            .expect("bug: airdrop request should complete");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(
            json,
            serde_json::json!({
                "attemptStatus": "succeeded",
                "recipient": format!("0x{}", "11".repeat(32)),
                "txHash": format!("0x{}", "22".repeat(32)),
                "rawAmount": "100000000",
                "height": 42,
                "status": { "result": "ok" }
            })
        );
    }

    #[tokio::test]
    async fn airdrop_rejects_mismatched_chain_identity() {
        let mut body = airdrop_body();
        body["genesisHash"] = serde_json::json!(format!("0x{}", "99".repeat(32)));

        let response = airdrop_request_with_body(&test_config(), body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_network_mismatch");
    }

    #[tokio::test]
    async fn airdrop_rejects_empty_client_nonce() {
        let mut body = airdrop_body();
        body["clientNonce"] = serde_json::json!("0x");

        let response = airdrop_request_with_body(&test_config(), body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_invalid_client_nonce");
    }

    #[tokio::test]
    async fn airdrop_rejects_client_nonce_over_32_bytes() {
        let mut body = airdrop_body();
        body["clientNonce"] = serde_json::json!(format!("0x{}", "01".repeat(33)));

        let response = airdrop_request_with_body(&test_config(), body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_invalid_client_nonce");
    }

    #[tokio::test]
    async fn airdrop_rejects_odd_length_client_nonce_with_structured_error() {
        let mut body = airdrop_body();
        body["clientNonce"] = serde_json::json!("0x1");

        let response = airdrop_request_with_body(&test_config(), body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_invalid_client_nonce");
    }

    #[tokio::test]
    async fn airdrop_rejects_non_string_client_nonce_with_structured_error() {
        let mut body = airdrop_body();
        body["clientNonce"] = serde_json::json!(1);

        let response = airdrop_request_with_body(&test_config(), body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_invalid_client_nonce");
    }

    #[tokio::test]
    async fn airdrop_retry_returns_recorded_success() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        let response = airdrop_request(&config).await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 1);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["attemptStatus"], "succeeded");
        assert_eq!(json["txHash"], format!("0x{}", "22".repeat(32)));
    }

    #[test]
    #[should_panic(expected = "airdrop submitter amount must match network descriptor")]
    fn airdrop_submitter_amount_must_match_network_descriptor() {
        FaucetConfig::new(
            test_network_descriptor(),
            Arc::new(MockAirdropSubmitter::success_with_raw_amount("200000000")),
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
    }

    #[tokio::test]
    async fn airdrop_rejects_mismatched_accepted_amount_without_resubmitting() {
        let airdrop = Arc::new(MockAirdropSubmitter::success_with_mismatched_accepted_amount());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );

        let response = airdrop_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(airdrop.calls(), 1);
        assert_eq!(airdrop.poll_calls(), 0);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(
            attempt.error_code.as_deref(),
            Some("airdrop_submit_amount_mismatch")
        );
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
    }

    #[tokio::test]
    async fn airdrop_and_wallet_create_use_distinct_transfer_amounts() {
        let mut descriptor = test_network_descriptor();
        descriptor.wallet_create.prefund_raw_amount = "200000000".into();
        let airdrop = Arc::new(MockAirdropSubmitter::success_with_raw_amount("100000000"));
        let prefund = Arc::new(MockAirdropSubmitter::success_with_raw_amount("200000000"));
        let config = FaucetConfig::new(
            descriptor,
            airdrop.clone(),
            prefund.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );

        let wallet_response = wallet_create_request(&config).await;

        assert_eq!(wallet_response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 0);
        assert_eq!(prefund.calls(), 1);

        let airdrop_response = airdrop_request(&config).await;

        assert_eq!(airdrop_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(airdrop_response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["rawAmount"], "100000000");
        assert_eq!(airdrop.calls(), 1);
        assert_eq!(prefund.calls(), 1);
        assert_eq!(
            stored_airdrop_attempt(&config)
                .expect("bug: airdrop attempt should be stored")
                .raw_amount,
            "100000000"
        );
    }

    #[test]
    #[should_panic(expected = "wallet prefund submitter amount must match network descriptor")]
    fn faucet_config_rejects_wallet_prefund_amount_mismatch() {
        let _ = FaucetConfig::new(
            test_network_descriptor(),
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockAirdropSubmitter::success_with_raw_amount("200000000")),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
    }

    #[tokio::test]
    async fn airdrop_final_store_failure_retains_accepted_transaction_for_retry() {
        let mut config = test_config_with_submitters(
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        config.policy_store = Arc::new(FailFinalPutPolicyStore::fail_succeeded_airdrop());

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
        assert_eq!(
            attempt.tx_hash.as_deref(),
            Some(format!("0x{}", "22".repeat(32)).as_str())
        );
        assert!(attempt.expires_at.is_none());
    }

    #[tokio::test]
    async fn airdrop_duplicate_in_flight_returns_pending_attempt() {
        let config = test_config();
        seed_airdrop_attempt(&config, policy::AirdropAttemptStatus::Pending, None);

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["attemptStatus"], "pending");
        assert_eq!(json["recipient"], format!("0x{}", "11".repeat(32)));
        assert_eq!(json["retryAfterSeconds"], 5);
        assert!(json.get("txHash").is_none());
    }

    #[tokio::test]
    async fn airdrop_pending_response_matches_shared_wire_type() {
        let config = test_config_with_submitters(
            Arc::new(MockAirdropSubmitter::pending()),
            Arc::new(MockWalletCreateSubmitter::success()),
        );

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: pending airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: pending airdrop body should be JSON");
        assert_eq!(
            json,
            serde_json::json!({
                "attemptStatus": "pending",
                "recipient": format!("0x{}", "11".repeat(32)),
                "txHash": format!("0x{}", "22".repeat(32)),
                "rawAmount": "100000000",
                "retryAfterSeconds": 5
            })
        );
    }

    #[tokio::test]
    async fn concurrent_same_key_airdrops_submit_once_with_zero_cooldown() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let mut config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        config.policy_config.airdrop.recipient_cooldown_secs = 0;

        let (first, second) = tokio::join!(airdrop_request(&config), airdrop_request(&config));

        assert!(
            matches!(first.status(), StatusCode::OK | StatusCode::ACCEPTED)
                && matches!(second.status(), StatusCode::OK | StatusCode::ACCEPTED)
        );
        assert_eq!(airdrop.calls(), 1);
    }

    #[tokio::test]
    async fn airdrop_execution_capacity_bounds_detached_workers() {
        let mut submitter = MockAirdropSubmitter::success();
        let submit_gate = Arc::new(tokio::sync::Semaphore::new(0));
        submitter.submit_gate = Some(submit_gate.clone());
        let airdrop = Arc::new(submitter);
        let mut config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        config.policy_config.airdrop.ip_quota_max = 100;
        let router = app(config);
        let mut tasks = Vec::new();
        for value in 1_u8..=64 {
            let mut body = airdrop_body();
            body["recipient"] = serde_json::json!(format!("0x{}", hex::encode([value; 32])));
            body["clientNonce"] = serde_json::json!(format!("0x{value:02x}"));
            let request = request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/airdrop")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("bug: airdrop request should build"),
            );
            let router = router.clone();
            tasks.push(tokio::spawn(async move { router.oneshot(request).await }));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while airdrop.calls() < 64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached workers should reach the submitter");
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            assert!(
                task.await
                    .expect_err("canceled request task should not complete")
                    .is_cancelled()
            );
        }
        let mut overflow_body = airdrop_body();
        overflow_body["recipient"] = serde_json::json!(format!("0x{}", hex::encode([65; 32])));
        overflow_body["clientNonce"] = serde_json::json!("0x41");

        let response = router
            .oneshot(request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/airdrop")
                    .header("content-type", "application/json")
                    .body(Body::from(overflow_body.to_string()))
                    .expect("bug: airdrop request should build"),
            ))
            .await
            .expect("bug: capacity request should complete");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: capacity response should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: capacity response should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_capacity_exceeded");
        submit_gate.add_permits(64);
        tokio::time::timeout(Duration::from_secs(1), async {
            while airdrop.poll_calls() < 64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached workers should finish after capacity is released");
    }

    #[tokio::test]
    async fn canceled_request_does_not_strand_pending_airdrop() {
        let mut submitter = MockAirdropSubmitter::success();
        submitter.submit_delay = Some(Duration::from_millis(25));
        let airdrop = Arc::new(submitter);
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        let request = {
            let config = config.clone();
            tokio::spawn(async move { airdrop_request(&config).await })
        };
        while airdrop.calls() == 0 {
            tokio::task::yield_now().await;
        }

        request.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Succeeded);
        assert_eq!(airdrop.calls(), 1);
    }

    #[tokio::test]
    async fn canceled_request_still_recovers_panicking_submission_task() {
        let config = test_config_with_submitters(
            Arc::new(PanickingAirdropSubmitter),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        let request = {
            let config = config.clone();
            tokio::spawn(async move { airdrop_request(&config).await })
        };
        while stored_airdrop_attempt(&config).is_none() {
            tokio::task::yield_now().await;
        }

        request.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::FailedTerminal);
        assert_eq!(attempt.error_code.as_deref(), Some("airdrop_submit_failed"));
    }

    #[tokio::test]
    async fn panicking_poll_does_not_overwrite_accepted_transaction() {
        let config = test_config_with_submitters(
            Arc::new(PanickingPollAirdropSubmitter),
            Arc::new(MockWalletCreateSubmitter::success()),
        );

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
        assert_eq!(attempt.tx_hash, Some(format!("0x{}", "22".repeat(32))));
    }

    #[tokio::test]
    async fn terminal_same_key_attempt_is_retained_with_zero_cooldown() {
        let mut config = test_config();
        config.policy_config.airdrop.recipient_cooldown_secs = 0;
        seed_airdrop_attempt(
            &config,
            policy::AirdropAttemptStatus::FailedTerminal,
            Some(format!("0x{}", "22".repeat(32))),
        );

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            stored_airdrop_attempt(&config).map(|attempt| attempt.status),
            Some(policy::AirdropAttemptStatus::FailedTerminal)
        );
    }

    #[tokio::test]
    async fn airdrop_accepted_transaction_is_polled_without_resubmission() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        seed_airdrop_attempt(
            &config,
            policy::AirdropAttemptStatus::Pending,
            Some(format!("0x{}", "22".repeat(32))),
        );

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 0);
        assert_eq!(airdrop.poll_calls(), 1);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Succeeded);
    }

    #[tokio::test]
    async fn airdrop_poll_retry_reuses_accepted_transaction() {
        let airdrop = Arc::new(MockAirdropSubmitter::poll_fail_once());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );

        let response = airdrop_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
        assert!(attempt.tx_hash.is_some());
        assert!(attempt.expires_at.is_none());

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 1);
        assert_eq!(airdrop.poll_calls(), 2);
    }

    #[tokio::test]
    async fn airdrop_invalid_transaction_becomes_terminal() {
        let airdrop = Arc::new(MockAirdropSubmitter::invalid());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );

        let response = airdrop_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::FailedTerminal);
        assert!(attempt.expires_at.is_some());

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(airdrop.calls(), 1);
        assert_eq!(airdrop.poll_calls(), 1);
    }

    #[tokio::test]
    async fn airdrop_never_expires_pending_attempt_before_retry() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        seed_airdrop_attempt_with_expiry(
            &config,
            policy::AirdropAttemptStatus::Pending,
            None,
            Some(SystemTime::UNIX_EPOCH),
        );

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(airdrop.calls(), 0);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Pending);
    }

    #[tokio::test]
    async fn airdrop_quota_rejects_before_submit() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let mut config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        config.policy_config.airdrop.ip_quota_max = 0;

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(airdrop.calls(), 0);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: rejection body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: rejection body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_quota_exceeded");
    }

    #[tokio::test]
    async fn airdrop_socket_ip_override_uses_finite_override() {
        let mut config = test_config();
        config.policy_config.trust_proxy_client_ip_headers = true;
        config.policy_config.airdrop.ip_quota_max = 0;
        config
            .policy_config
            .airdrop
            .client_overrides
            .push(config::AirdropClientOverride {
                ip: "127.0.0.1".parse().expect("bug: override IP should parse"),
                quota_max: 1,
                quota_window_secs: 60,
            });
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/airdrop")
            .header("content-type", "application/json")
            .header("x-forwarded-for", "203.0.113.10")
            .body(Body::from(airdrop_body().to_string()))
            .expect("bug: airdrop request should build");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));

        let response = app(config.clone())
            .oneshot(request)
            .await
            .expect("request should complete");
        let mut second_body = airdrop_body();
        second_body["recipient"] = serde_json::json!(format!("0x{}", "12".repeat(32)));
        second_body["clientNonce"] = serde_json::json!("0x02");
        let second_response = airdrop_request_with_body(&config, second_body).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(second_response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn airdrop_unconfigured_socket_ip_uses_default_quota() {
        let mut config = test_config();
        config.policy_config.airdrop.ip_quota_max = 0;
        config
            .policy_config
            .airdrop
            .client_overrides
            .push(config::AirdropClientOverride {
                ip: "127.0.0.2".parse().expect("bug: override IP should parse"),
                quota_max: 1,
                quota_window_secs: 60,
            });

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn airdrop_retries_failed_submit_attempt() {
        let airdrop = Arc::new(MockAirdropSubmitter::fail_once());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        let response = airdrop_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(
            stored_airdrop_attempt(&config)
                .expect("bug: failed attempt should be stored")
                .expires_at
                .is_some()
        );

        let response = airdrop_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 2);
        let attempt = stored_airdrop_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::AirdropAttemptStatus::Succeeded);
    }

    #[tokio::test]
    async fn airdrop_rejects_malformed_recipient() {
        let response = app(test_config())
            .oneshot(request_with_connect_info(
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
            ))
            .await
            .expect("bug: airdrop request should complete");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn airdrop_submit_failure_returns_bad_gateway() {
        let response = app(FaucetConfig::new(
            test_network_descriptor(),
            Arc::new(MockAirdropSubmitter::failure()),
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::success()),
        ))
        .oneshot(request_with_connect_info(
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
        ))
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
    async fn wallet_create_final_store_failure_retains_accepted_transaction_for_retry() {
        let mut config = test_config_with_submitters(
            Arc::new(MockAirdropSubmitter::success()),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        config.policy_store = Arc::new(FailFinalPutPolicyStore::fail_created_wallet());

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(
            attempt.status,
            policy::WalletCreateAttemptStatus::CreatePending
        );
        assert_eq!(
            attempt.create_tx_hash.as_deref(),
            Some(format!("0x{}", "22".repeat(32)).as_str())
        );
        assert!(attempt.expires_at.is_none());
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
    async fn wallet_create_accepted_prefund_is_polled_without_resubmission() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let wallet_create = Arc::new(MockWalletCreateSubmitter::success());
        let config = test_config_with_submitters(airdrop.clone(), wallet_create.clone());
        seed_wallet_create_attempt(
            &config,
            policy::WalletCreateAttemptStatus::PrefundPending,
            Some(format!("0x{}", "22".repeat(32))),
            None,
        );

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 0);
        assert_eq!(airdrop.poll_calls(), 1);
        assert_eq!(wallet_create.calls(), 1);
        assert_eq!(wallet_create.poll_calls(), 1);
    }

    #[tokio::test]
    async fn wallet_create_accepted_create_is_polled_without_resubmission() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let wallet_create = Arc::new(MockWalletCreateSubmitter::success());
        let config = test_config_with_submitters(airdrop.clone(), wallet_create.clone());
        seed_wallet_create_attempt(
            &config,
            policy::WalletCreateAttemptStatus::CreatePending,
            Some(format!("0x{}", "22".repeat(32))),
            Some(format!("0x{}", "22".repeat(32))),
        );

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 0);
        assert_eq!(airdrop.poll_calls(), 0);
        assert_eq!(wallet_create.calls(), 0);
        assert_eq!(wallet_create.poll_calls(), 1);
    }

    #[tokio::test]
    async fn wallet_create_prefund_poll_retry_reuses_accepted_transaction() {
        let airdrop = Arc::new(MockAirdropSubmitter::poll_fail_once());
        let wallet_create = Arc::new(MockWalletCreateSubmitter::success());
        let config = test_config_with_submitters(airdrop.clone(), wallet_create.clone());

        let response = wallet_create_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(
            attempt.status,
            policy::WalletCreateAttemptStatus::PrefundPending
        );
        assert!(attempt.prefund_tx_hash.is_some());
        assert!(attempt.expires_at.is_none());

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 1);
        assert_eq!(airdrop.poll_calls(), 2);
        assert_eq!(wallet_create.calls(), 1);
    }

    #[tokio::test]
    async fn wallet_create_poll_retry_reuses_accepted_transaction() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let wallet_create = Arc::new(MockWalletCreateSubmitter::poll_fail_once());
        let config = test_config_with_submitters(airdrop.clone(), wallet_create.clone());

        let response = wallet_create_request(&config).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(
            attempt.status,
            policy::WalletCreateAttemptStatus::CreatePending
        );
        assert!(attempt.create_tx_hash.is_some());
        assert!(attempt.expires_at.is_none());

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(airdrop.calls(), 1);
        assert_eq!(wallet_create.calls(), 1);
        assert_eq!(wallet_create.poll_calls(), 2);
    }

    #[tokio::test]
    async fn wallet_create_expires_stale_pending_attempt_before_retry() {
        let config = test_config_with_store();
        seed_wallet_create_attempt_with_expiry(
            &config,
            policy::WalletCreateAttemptStatus::PrefundPending,
            None,
            None,
            Some(SystemTime::UNIX_EPOCH),
        );

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::OK);
        let attempt = stored_wallet_create_attempt(&config).expect("bug: attempt should be stored");
        assert_eq!(attempt.status, policy::WalletCreateAttemptStatus::Created);
    }

    #[tokio::test]
    async fn wallet_create_quota_rejects_before_prefund() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let mut config = FaucetConfig::new(
            test_network_descriptor(),
            airdrop.clone(),
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        config.policy_config.wallet_create.owner_quota_max = 0;

        let response = wallet_create_request(&config).await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(airdrop.calls(), 0);
    }

    #[tokio::test]
    async fn wallet_create_rejects_empty_client_nonce() {
        let mut body = wallet_create_body();
        body["clientNonce"] = serde_json::json!("0x");

        let response = app(test_config())
            .oneshot(request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/create-wallet")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("bug: wallet-create request should build"),
            ))
            .await
            .expect("bug: wallet-create request should complete");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: wallet-create body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: wallet-create body should be JSON");
        assert_eq!(json["error"]["code"], "wallet_create_invalid_client_nonce");
    }

    #[tokio::test]
    async fn wallet_create_pending_owner_limit_rejects_second_nonce_before_prefund() {
        let airdrop = Arc::new(MockAirdropSubmitter::success());
        let config = test_config_with_submitters(
            airdrop.clone(),
            Arc::new(MockWalletCreateSubmitter::success()),
        );
        seed_wallet_create_attempt(
            &config,
            policy::WalletCreateAttemptStatus::Prepared,
            None,
            None,
        );
        let mut body = wallet_create_body();
        body["clientNonce"] = serde_json::json!("0x02");

        let response = app(config.clone())
            .oneshot(request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/create-wallet")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("bug: wallet-create request should build"),
            ))
            .await
            .expect("bug: wallet-create request should complete");

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
        let airdrop_submitter = Arc::new(MockAirdropSubmitter::success());
        FaucetConfig::new(
            test_network_descriptor(),
            airdrop_submitter.clone(),
            airdrop_submitter,
            Arc::new(MockWalletCreateSubmitter::success()),
        )
    }

    fn test_config_with_submitters(
        airdrop_submitter: Arc<dyn airdrop::AirdropSubmitter>,
        wallet_create_submitter: Arc<dyn wallet_create::WalletCreateSubmitter>,
    ) -> FaucetConfig {
        FaucetConfig::new(
            test_network_descriptor(),
            airdrop_submitter.clone(),
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

    async fn airdrop_request(config: &FaucetConfig) -> axum::response::Response {
        airdrop_request_with_body(config, airdrop_body()).await
    }

    async fn airdrop_request_with_body(
        config: &FaucetConfig,
        body: serde_json::Value,
    ) -> axum::response::Response {
        app(config.clone())
            .oneshot(request_with_connect_info(
                Request::builder()
                    .method("POST")
                    .uri("/v1/airdrop")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("bug: airdrop request should build"),
            ))
            .await
            .expect("bug: airdrop request should complete")
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

    fn airdrop_body() -> serde_json::Value {
        serde_json::json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "0x01",
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
        seed_wallet_create_attempt_with_expiry(
            config,
            status,
            prefund_tx_hash,
            create_tx_hash,
            None,
        );
    }

    fn seed_wallet_create_attempt_with_expiry(
        config: &FaucetConfig,
        status: policy::WalletCreateAttemptStatus,
        prefund_tx_hash: Option<String>,
        create_tx_hash: Option<String>,
        expires_at: Option<SystemTime>,
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
                expires_at,
            })
            .expect("bug: test attempt insert should succeed");
    }

    fn stored_wallet_create_attempt(config: &FaucetConfig) -> Option<policy::WalletCreateAttempt> {
        config
            .policy_store
            .get_wallet_create_attempt(&wallet_create_attempt_key())
            .expect("bug: test attempt fetch should succeed")
    }

    fn airdrop_attempt_key() -> policy::AirdropAttemptKey {
        policy::AirdropAttemptKey {
            network_id: "devnet-preview".into(),
            genesis_hash: format!("0x{}", "00".repeat(32)),
            recipient: format!("0x{}", "11".repeat(32)),
            client_nonce: "01".into(),
        }
    }

    fn seed_airdrop_attempt(
        config: &FaucetConfig,
        status: policy::AirdropAttemptStatus,
        tx_hash: Option<String>,
    ) {
        seed_airdrop_attempt_with_expiry(config, status, tx_hash, None);
    }

    fn seed_airdrop_attempt_with_expiry(
        config: &FaucetConfig,
        status: policy::AirdropAttemptStatus,
        tx_hash: Option<String>,
        expires_at: Option<SystemTime>,
    ) {
        let now = SystemTime::UNIX_EPOCH;
        config
            .policy_store
            .put_airdrop_attempt(policy::AirdropAttempt {
                key: airdrop_attempt_key(),
                client_identity: policy::ClientIdentity::Unknown,
                raw_amount: "100000000".into(),
                status,
                tx_hash,
                height: None,
                error_code: None,
                created_at: now,
                updated_at: now,
                expires_at,
            })
            .expect("bug: test airdrop attempt insert should succeed");
    }

    fn stored_airdrop_attempt(config: &FaucetConfig) -> Option<policy::AirdropAttempt> {
        config
            .policy_store
            .get_airdrop_attempt(&airdrop_attempt_key())
            .expect("bug: test airdrop attempt fetch should succeed")
    }

    struct FailFinalPutPolicyStore {
        inner: policy::InMemoryFaucetPolicyStore,
        fail_created_wallet: bool,
        fail_succeeded_airdrop: bool,
    }

    impl FailFinalPutPolicyStore {
        fn fail_created_wallet() -> Self {
            Self {
                inner: policy::InMemoryFaucetPolicyStore::new(),
                fail_created_wallet: true,
                fail_succeeded_airdrop: false,
            }
        }

        fn fail_succeeded_airdrop() -> Self {
            Self {
                inner: policy::InMemoryFaucetPolicyStore::new(),
                fail_created_wallet: false,
                fail_succeeded_airdrop: true,
            }
        }
    }

    impl policy::FaucetPolicyStore for FailFinalPutPolicyStore {
        fn get_wallet_create_attempt(
            &self,
            key: &policy::WalletCreateAttemptKey,
        ) -> Result<Option<policy::WalletCreateAttempt>, policy::PolicyStoreError> {
            self.inner.get_wallet_create_attempt(key)
        }

        fn put_wallet_create_attempt(
            &self,
            attempt: policy::WalletCreateAttempt,
        ) -> Result<Option<policy::WalletCreateAttempt>, policy::PolicyStoreError> {
            if self.fail_created_wallet
                && attempt.status == policy::WalletCreateAttemptStatus::Created
            {
                return Err(policy::PolicyStoreError::LockPoisoned);
            }
            self.inner.put_wallet_create_attempt(attempt)
        }

        fn insert_wallet_create_attempt_if_absent(
            &self,
            attempt: policy::WalletCreateAttempt,
            max_pending_per_owner: u32,
        ) -> Result<policy::InsertWalletCreateAttemptResult, policy::PolicyStoreError> {
            self.inner
                .insert_wallet_create_attempt_if_absent(attempt, max_pending_per_owner)
        }

        fn claim_wallet_create_transition(
            &self,
            key: &policy::WalletCreateAttemptKey,
            expected: &[policy::WalletCreateAttemptStatus],
            next: policy::WalletCreateAttemptStatus,
            now: SystemTime,
        ) -> Result<policy::WalletCreateAttempt, policy::PolicyStoreError> {
            self.inner
                .claim_wallet_create_transition(key, expected, next, now)
        }

        fn get_airdrop_attempt(
            &self,
            key: &policy::AirdropAttemptKey,
        ) -> Result<Option<policy::AirdropAttempt>, policy::PolicyStoreError> {
            self.inner.get_airdrop_attempt(key)
        }

        fn put_airdrop_attempt(
            &self,
            attempt: policy::AirdropAttempt,
        ) -> Result<Option<policy::AirdropAttempt>, policy::PolicyStoreError> {
            if self.fail_succeeded_airdrop
                && attempt.status == policy::AirdropAttemptStatus::Succeeded
            {
                return Err(policy::PolicyStoreError::LockPoisoned);
            }
            self.inner.put_airdrop_attempt(attempt)
        }

        fn admit_airdrop_attempt(
            &self,
            attempt: policy::AirdropAttempt,
            attempt_capacity: usize,
            max_pending_per_recipient: u32,
            quota_checks: &[policy::QuotaCheck],
            now: SystemTime,
        ) -> Result<policy::AdmitAirdropAttemptResult, policy::PolicyStoreError> {
            self.inner.admit_airdrop_attempt(
                attempt,
                attempt_capacity,
                max_pending_per_recipient,
                quota_checks,
                now,
            )
        }

        fn claim_airdrop_transition(
            &self,
            key: &policy::AirdropAttemptKey,
            expected: &[policy::AirdropAttemptStatus],
            next: policy::AirdropAttemptStatus,
            now: SystemTime,
        ) -> Result<policy::AirdropAttempt, policy::PolicyStoreError> {
            self.inner
                .claim_airdrop_transition(key, expected, next, now)
        }

        fn check_and_record_quota(
            &self,
            key: policy::QuotaKey,
            max: u32,
            window: Duration,
            now: SystemTime,
        ) -> Result<policy::QuotaDecision, policy::PolicyStoreError> {
            self.inner.check_and_record_quota(key, max, window, now)
        }

        fn check_and_record_quotas(
            &self,
            checks: &[policy::QuotaCheck],
            now: SystemTime,
        ) -> Result<policy::QuotaDecision, policy::PolicyStoreError> {
            self.inner.check_and_record_quotas(checks, now)
        }

        fn expire(&self, now: SystemTime) -> Result<(), policy::PolicyStoreError> {
            self.inner.expire(now)
        }
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
            "airdrop": {
                "rawAmount": "100000000",
                "recipientCooldownSeconds": 43200,
                "clientQuotaMax": 10,
                "clientQuotaWindowSeconds": 3600,
                "maxPendingPerRecipient": 1
            },
            "walletCreate": {
                "prefundRawAmount": "100000000",
                "walletContractHash": format!("0x{}", "00".repeat(32))
            },
            "devWallet": {
                "deployEntrypoint": "0xf969542572bd6860",
                "doActionEntrypoint": "0xa0fff94c64fbaf3c"
            }
        }))
        .expect("bug: test network descriptor should deserialize")
    }

    struct MockAirdropSubmitter {
        failures_remaining: Mutex<usize>,
        poll_failures_remaining: Mutex<usize>,
        calls: Mutex<usize>,
        poll_calls: Mutex<usize>,
        raw_amount: String,
        accepted_raw_amount: String,
        poll_times_out: bool,
        poll_invalid: bool,
        submit_delay: Option<Duration>,
        submit_gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    struct PanickingAirdropSubmitter;
    struct PanickingPollAirdropSubmitter;

    #[async_trait::async_trait]
    impl airdrop::AirdropSubmitter for PanickingAirdropSubmitter {
        fn raw_amount(&self) -> u128 {
            100_000_000
        }

        async fn submit_airdrop(
            &self,
            _recipient: ContractAddress,
        ) -> Result<airdrop::AcceptedAirdrop, airdrop::AirdropSubmitError> {
            tokio::time::sleep(Duration::from_millis(25)).await;
            panic!("intentional submission panic")
        }

        async fn poll_airdrop(
            &self,
            _tx_hash: TxHash,
        ) -> Result<TransactionReceipt, airdrop::AirdropSubmitError> {
            unreachable!("panicking submitter never accepts a transaction")
        }
    }

    #[async_trait::async_trait]
    impl airdrop::AirdropSubmitter for PanickingPollAirdropSubmitter {
        fn raw_amount(&self) -> u128 {
            100_000_000
        }

        async fn submit_airdrop(
            &self,
            _recipient: ContractAddress,
        ) -> Result<airdrop::AcceptedAirdrop, airdrop::AirdropSubmitError> {
            Ok(airdrop::AcceptedAirdrop {
                raw_amount: "100000000".into(),
                tx_hash: test_tx_hash(),
            })
        }

        async fn poll_airdrop(
            &self,
            _tx_hash: TxHash,
        ) -> Result<TransactionReceipt, airdrop::AirdropSubmitError> {
            panic!("intentional polling panic")
        }
    }

    impl MockAirdropSubmitter {
        fn success() -> Self {
            Self::success_with_raw_amount("100000000")
        }

        fn success_with_raw_amount(raw_amount: &str) -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                poll_failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                raw_amount: raw_amount.into(),
                accepted_raw_amount: raw_amount.into(),
                poll_times_out: false,
                poll_invalid: false,
                submit_delay: None,
                submit_gate: None,
            }
        }

        fn success_with_mismatched_accepted_amount() -> Self {
            let mut submitter = Self::success();
            submitter.accepted_raw_amount = "200000000".into();
            submitter
        }

        fn pending() -> Self {
            let mut submitter = Self::success();
            submitter.poll_times_out = true;
            submitter
        }

        fn invalid() -> Self {
            let mut submitter = Self::success();
            submitter.poll_invalid = true;
            submitter
        }

        fn failure() -> Self {
            Self {
                failures_remaining: Mutex::new(usize::MAX),
                poll_failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                raw_amount: "100000000".into(),
                accepted_raw_amount: "100000000".into(),
                poll_times_out: false,
                poll_invalid: false,
                submit_delay: None,
                submit_gate: None,
            }
        }

        fn fail_once() -> Self {
            Self {
                failures_remaining: Mutex::new(1),
                poll_failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                raw_amount: "100000000".into(),
                accepted_raw_amount: "100000000".into(),
                poll_times_out: false,
                poll_invalid: false,
                submit_delay: None,
                submit_gate: None,
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("bug: calls lock should succeed")
        }

        fn poll_fail_once() -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                poll_failures_remaining: Mutex::new(1),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                raw_amount: "100000000".into(),
                accepted_raw_amount: "100000000".into(),
                poll_times_out: false,
                poll_invalid: false,
                submit_delay: None,
                submit_gate: None,
            }
        }

        fn poll_calls(&self) -> usize {
            *self
                .poll_calls
                .lock()
                .expect("bug: poll calls lock should succeed")
        }
    }

    #[async_trait::async_trait]
    impl airdrop::AirdropSubmitter for MockAirdropSubmitter {
        fn raw_amount(&self) -> u128 {
            self.raw_amount
                .parse()
                .expect("bug: mock raw amount should be valid")
        }

        async fn submit_airdrop(
            &self,
            _recipient: ContractAddress,
        ) -> Result<airdrop::AcceptedAirdrop, airdrop::AirdropSubmitError> {
            *self.calls.lock().expect("bug: calls lock should succeed") += 1;
            if let Some(delay) = self.submit_delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(gate) = &self.submit_gate {
                gate.acquire()
                    .await
                    .expect("bug: submit gate should stay open")
                    .forget();
            }
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
            Ok(airdrop::AcceptedAirdrop {
                raw_amount: self.accepted_raw_amount.clone(),
                tx_hash: test_tx_hash(),
            })
        }

        async fn poll_airdrop(
            &self,
            tx_hash: TxHash,
        ) -> Result<TransactionReceipt, airdrop::AirdropSubmitError> {
            *self
                .poll_calls
                .lock()
                .expect("bug: poll calls lock should succeed") += 1;
            let mut failures = self
                .poll_failures_remaining
                .lock()
                .expect("bug: poll failures lock should succeed");
            if *failures > 0 {
                *failures -= 1;
                return Err(airdrop::AirdropSubmitError::RpcConnect("boom".into()));
            }
            if self.poll_times_out {
                return Err(airdrop::AirdropSubmitError::Poll(
                    groundhog_json_rpc_client::PollTransactionReceiptError::ReceiptTimeout {
                        tx_hash,
                        attempts: 1,
                    },
                ));
            }
            if self.poll_invalid {
                return Err(airdrop::AirdropSubmitError::Poll(
                    groundhog_json_rpc_client::PollTransactionReceiptError::TransactionStatus {
                        tx_hash,
                        status: TransactionStatus::Invalid {
                            reason: "expired".into(),
                        },
                    },
                ));
            }
            Ok(successful_receipt(tx_hash))
        }
    }

    fn test_tx_hash() -> TxHash {
        TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x22; 32])))
    }

    struct MockWalletCreateSubmitter {
        failures_remaining: Mutex<usize>,
        poll_failures_remaining: Mutex<usize>,
        calls: Mutex<usize>,
        poll_calls: Mutex<usize>,
        verify_fails: bool,
    }

    impl MockWalletCreateSubmitter {
        fn success() -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                poll_failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                verify_fails: false,
            }
        }

        fn failure() -> Self {
            Self {
                failures_remaining: Mutex::new(usize::MAX),
                poll_failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                verify_fails: false,
            }
        }

        fn fail_once() -> Self {
            Self {
                failures_remaining: Mutex::new(1),
                poll_failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                verify_fails: false,
            }
        }

        fn verify_failure() -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                poll_failures_remaining: Mutex::new(0),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                verify_fails: true,
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("bug: calls lock should succeed")
        }

        fn poll_fail_once() -> Self {
            Self {
                failures_remaining: Mutex::new(0),
                poll_failures_remaining: Mutex::new(1),
                calls: Mutex::new(0),
                poll_calls: Mutex::new(0),
                verify_fails: false,
            }
        }

        fn poll_calls(&self) -> usize {
            *self
                .poll_calls
                .lock()
                .expect("bug: poll calls lock should succeed")
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
        ) -> Result<wallet_create::AcceptedWalletCreate, wallet_create::WalletCreateError> {
            *self.calls.lock().expect("bug: calls lock should succeed") += 1;
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
            Ok(wallet_create::AcceptedWalletCreate {
                wallet_address: Hex0x(groundhog_xdr::types::ContractAddress::from([0x55; 32])),
                tx_hash: test_tx_hash(),
            })
        }

        async fn poll_wallet_create(
            &self,
            tx_hash: TxHash,
        ) -> Result<TransactionReceipt, wallet_create::WalletCreateError> {
            *self
                .poll_calls
                .lock()
                .expect("bug: poll calls lock should succeed") += 1;
            let mut failures = self
                .poll_failures_remaining
                .lock()
                .expect("bug: poll failures lock should succeed");
            if *failures > 0 {
                *failures -= 1;
                return Err(wallet_create::WalletCreateError::RpcConnect("boom".into()));
            }
            Ok(successful_receipt(tx_hash))
        }
    }

    fn successful_receipt(tx_hash: TxHash) -> TransactionReceipt {
        TransactionReceipt {
            tx_hash,
            height: BlockHeight::from(42),
            status: ExecStatus::Ok,
            events: vec![],
            return_data: None,
            abort_data: None,
        }
    }
}
