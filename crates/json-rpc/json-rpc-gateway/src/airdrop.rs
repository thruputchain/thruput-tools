use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use groundhog_json_rpc_api::AirdropApiServer;
use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::{
    SHA256Hash,
    airdrop::{AirdropAttemptResponse, AirdropRequest, HostedAirdropRequest},
    network_descriptor::{
        NETWORK_DESCRIPTOR_KIND, NETWORK_DESCRIPTOR_SCHEMA_VERSION, NetworkDescriptor,
    },
    network_info::NetworkInfo,
    receipt::ExecStatus,
};
use jsonrpsee::{RpcModule, core::RpcResult, types::ErrorObjectOwned};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::{
    config::GatewayConfig,
    metrics::{AirdropRequestOutcome, GatewayMetrics},
};

const MAX_IN_FLIGHT: usize = 64;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct HostedAirdrop {
    inner: Arc<HostedAirdropInner>,
}

impl std::fmt::Debug for HostedAirdrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedAirdrop")
            .field("base_url", &self.inner.base_url)
            .finish_non_exhaustive()
    }
}

struct HostedAirdropInner {
    client: reqwest::Client,
    base_url: Url,
    network_id: String,
    network: NetworkInfo,
    genesis_hash: SHA256Hash,
    validation: tokio::sync::RwLock<ValidationState>,
    admission: Semaphore,
    request_timeout: Duration,
    metrics: GatewayMetrics,
}

#[derive(Default)]
struct ValidationState {
    validated: bool,
    last_error: Option<ProbeError>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AirdropStartupError {
    #[error("invalid faucet URL: {reason}")]
    InvalidUrl { reason: String },
    #[error("failed to build faucet HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("faucet network descriptor validation failed: {0}")]
    Descriptor(String),
    #[error("a hosted faucet requires a non-empty gateway field `network_id`")]
    MissingNetworkId,
    #[error("a hosted faucet requires gateway field `genesis_hash`")]
    MissingGenesisHash,
}

#[derive(Debug)]
pub(crate) struct AirdropProbe {
    pub(crate) up: bool,
    pub(crate) error: Option<String>,
}

impl HostedAirdrop {
    #[cfg(test)]
    pub(crate) async fn from_config(
        config: &GatewayConfig,
        network: NetworkInfo,
    ) -> Result<Option<Self>, AirdropStartupError> {
        Self::from_config_with_metrics(config, network, GatewayMetrics::for_tests()).await
    }

    pub(crate) async fn from_config_with_metrics(
        config: &GatewayConfig,
        network: NetworkInfo,
        metrics: GatewayMetrics,
    ) -> Result<Option<Self>, AirdropStartupError> {
        let Some(base_url) = config.faucet_url.as_deref() else {
            return Ok(None);
        };
        let network_id = config
            .network_id
            .as_deref()
            .map(str::trim)
            .filter(|network_id| !network_id.is_empty())
            .ok_or(AirdropStartupError::MissingNetworkId)?
            .to_owned();
        let genesis_hash = config
            .genesis_hash
            .ok_or(AirdropStartupError::MissingGenesisHash)?;
        let base_url = Url::parse(base_url).map_err(|source| AirdropStartupError::InvalidUrl {
            reason: source.to_string(),
        })?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(AirdropStartupError::InvalidUrl {
                reason: "must use http or https".into(),
            });
        }
        if !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(AirdropStartupError::InvalidUrl {
                reason: "must not contain credentials, a query, or a fragment".into(),
            });
        }
        let request_timeout = config.backend_request_timeout();
        let connect_timeout =
            Duration::from_millis(config.backend_connect_timeout_ms.max(1)).min(request_timeout);
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(AirdropStartupError::Client)?;
        let facade = Self {
            inner: Arc::new(HostedAirdropInner {
                client,
                base_url,
                network_id,
                network,
                genesis_hash,
                validation: tokio::sync::RwLock::new(ValidationState::default()),
                admission: Semaphore::new(MAX_IN_FLIGHT),
                request_timeout,
                metrics,
            }),
        };

        match facade
            .validate_and_record(Some(connect_timeout.min(Duration::from_secs(5))))
            .await
        {
            Ok(()) => {}
            Err(ProbeError::Unavailable(_) | ProbeError::Timeout) => {
                tracing::warn!(faucet_url = %facade.inner.base_url, "faucet unavailable at startup; starting degraded");
            }
            Err(error) => return Err(AirdropStartupError::Descriptor(error.to_string())),
        }
        Ok(Some(facade))
    }

    pub(crate) async fn probe(&self, timeout: Duration) -> AirdropProbe {
        match self.validate_and_record(Some(timeout)).await {
            Ok(()) => AirdropProbe {
                up: true,
                error: None,
            },
            Err(error) => AirdropProbe {
                up: false,
                error: Some(error.to_string()),
            },
        }
    }

    async fn validate_and_record(&self, timeout: Option<Duration>) -> Result<(), ProbeError> {
        let (mut state, result) = if let Some(timeout) = timeout {
            let deadline = tokio::time::Instant::now() + timeout;
            let Ok(state) = tokio::time::timeout_at(deadline, self.inner.validation.write()).await
            else {
                return Err(ProbeError::Timeout);
            };
            let result = tokio::time::timeout_at(deadline, self.validate_network_inner())
                .await
                .unwrap_or(Err(ProbeError::Timeout));
            (state, result)
        } else {
            let state = self.inner.validation.write().await;
            let result = self.validate_network_inner().await;
            (state, result)
        };
        state.validated = result.is_ok();
        state.last_error = result.as_ref().err().cloned();
        result
    }

    async fn ensure_validated(&self) -> Result<(), ProbeError> {
        {
            let state = self.inner.validation.read().await;
            if state.validated {
                return Ok(());
            }
        }
        let Ok(mut state) = self.inner.validation.try_write() else {
            let state = self.inner.validation.write().await;
            return if state.validated {
                Ok(())
            } else {
                Err(state.last_error.clone().unwrap_or_else(|| {
                    ProbeError::Unavailable("descriptor validation did not complete".into())
                }))
            };
        };
        if state.validated {
            return Ok(());
        }
        let result = self.validate_network_inner().await;
        state.validated = result.is_ok();
        state.last_error = result.as_ref().err().cloned();
        result
    }

    async fn validate_network_inner(&self) -> Result<(), ProbeError> {
        let url = endpoint(&self.inner.base_url, "v1/network")?;
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| ProbeError::http(&error))?;
        if !response.status().is_success() {
            if response.status().is_server_error() {
                return Err(ProbeError::Unavailable(format!(
                    "GET /v1/network returned HTTP {}",
                    response.status()
                )));
            }
            return Err(ProbeError::Malformed(format!(
                "GET /v1/network returned HTTP {}",
                response.status()
            )));
        }
        let body = read_bounded(response).await?;
        let descriptor: NetworkDescriptor = serde_json::from_slice(&body)
            .map_err(|error| ProbeError::Malformed(error.to_string()))?;
        if descriptor.schema_version != NETWORK_DESCRIPTOR_SCHEMA_VERSION
            || descriptor.kind != NETWORK_DESCRIPTOR_KIND
        {
            return Err(ProbeError::Malformed(format!(
                "unexpected descriptor schemaVersion {} or kind `{}`",
                descriptor.schema_version, descriptor.kind
            )));
        }
        if descriptor.network_id != self.inner.network_id
            || descriptor.network_hash != self.inner.network.network_hash
            || descriptor.genesis_hash != self.inner.genesis_hash
        {
            return Err(ProbeError::Mismatch);
        }
        Ok(())
    }

    #[cfg(test)]
    async fn is_validated(&self) -> bool {
        self.inner.validation.read().await.validated
    }

    async fn forward(
        &self,
        request: AirdropRequest,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        let deadline = tokio::time::Instant::now() + self.inner.request_timeout;
        let _permit = self
            .inner
            .admission
            .try_acquire()
            .map_err(|_| RpcServerError::Overloaded("faucet request limit reached".into()))?;
        tokio::time::timeout_at(deadline, self.ensure_validated())
            .await
            .map_err(|_| RpcServerError::BackendTimeout)?
            .map_err(map_probe_error)?;
        let hosted = HostedAirdropRequest {
            recipient: request.recipient,
            client_nonce: request.client_nonce,
            network_id: self.inner.network_id.clone(),
            network_hash: self.inner.network.network_hash,
            genesis_hash: self.inner.genesis_hash,
        };
        let url = endpoint(&self.inner.base_url, "v1/airdrop")
            .map_err(|error| RpcServerError::BackendUnavailable(error.to_string()))?;
        let started = Instant::now();
        let result = tokio::time::timeout_at(deadline, self.post_airdrop(url, &hosted))
            .await
            .unwrap_or(Err(RpcServerError::BackendTimeout));
        let outcome = match &result {
            Ok(AirdropAttemptResponse::Pending { .. }) => AirdropRequestOutcome::Pending,
            Ok(AirdropAttemptResponse::Succeeded { .. }) => AirdropRequestOutcome::Succeeded,
            Err(RpcServerError::BackendTimeout) => AirdropRequestOutcome::Timeout,
            Ok(_) | Err(_) => AirdropRequestOutcome::Error,
        };
        self.inner
            .metrics
            .observe_airdrop_request("hosted", outcome, started.elapsed());
        result
    }

    async fn post_airdrop(
        &self,
        url: Url,
        hosted: &HostedAirdropRequest,
    ) -> Result<AirdropAttemptResponse, RpcServerError> {
        let response = self
            .inner
            .client
            .post(url)
            .json(&hosted)
            .send()
            .await
            .map_err(|error| map_http_error(&error))?;
        let status = response.status();
        let body = read_bounded(response).await.map_err(map_probe_error)?;
        let decoded_response = serde_json::from_slice::<AirdropAttemptResponse>(&body);
        if status.is_success()
            && let Ok(response) = decoded_response
        {
            if response.recipient() != &hosted.recipient {
                return Err(RpcServerError::BackendUnavailable(
                    "faucet response recipient does not match the request".into(),
                ));
            }
            if matches!(
                &response,
                AirdropAttemptResponse::Succeeded { status, .. }
                    if !matches!(status, ExecStatus::Ok)
            ) {
                return Err(RpcServerError::BackendUnavailable(
                    "faucet returned a succeeded response with a failed execution status".into(),
                ));
            }
            return Ok(response);
        }
        if let Ok(error) = serde_json::from_slice::<FaucetErrorEnvelope>(&body) {
            return Err(map_faucet_error(status, error.error));
        }
        let detail = match decoded_response {
            Ok(_) => "success response body returned with a non-success HTTP status".to_owned(),
            Err(error) => error.to_string(),
        };
        Err(RpcServerError::BackendUnavailable(if status.is_success() {
            format!("faucet returned a malformed success response: {detail}")
        } else {
            format!("faucet returned malformed HTTP {status} error: {detail}")
        }))
    }
}

pub(crate) fn merge_rpc(
    rpc: &mut RpcModule<()>,
    airdrop: Option<HostedAirdrop>,
) -> Result<(), jsonrpsee::core::RegisterMethodError> {
    if let Some(airdrop) = airdrop {
        rpc.merge(airdrop.into_rpc().remove_context())?;
    }
    Ok(())
}

#[async_trait]
impl AirdropApiServer for HostedAirdrop {
    async fn request_airdrop(&self, request: AirdropRequest) -> RpcResult<AirdropAttemptResponse> {
        self.forward(request).await.map_err(ErrorObjectOwned::from)
    }
}

#[derive(Debug, Deserialize)]
struct FaucetErrorEnvelope {
    error: FaucetError,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FaucetError {
    code: String,
    message: String,
}

fn map_faucet_error(status: StatusCode, error: FaucetError) -> RpcServerError {
    let FaucetError { code, message } = error;
    let (mapped, bounded_code) = match code.as_str() {
        code @ "airdrop_invalid_client_nonce" => (RpcServerError::InvalidParam(message), code),
        code @ ("airdrop_quota_exceeded"
        | "airdrop_recipient_pending_limit_exceeded"
        | "airdrop_capacity_exceeded"
        | "airdrop_already_pending") => (RpcServerError::Overloaded(message), code),
        code @ ("airdrop_attempt_terminal" | "airdrop_transaction_failed") => {
            (RpcServerError::TxRejected(message), code)
        }
        code @ ("airdrop_network_mismatch"
        | "airdrop_attempt_conflict"
        | "airdrop_attempt_state_invalid"
        | "faucet_policy_store_unavailable"
        | "airdrop_submit_amount_mismatch"
        | "airdrop_submit_failed") => (RpcServerError::BackendUnavailable(message), code),
        _ => (
            RpcServerError::BackendUnavailable(format!(
                "faucet returned HTTP {status}: {message} ({code})"
            )),
            "unknown",
        ),
    };
    tracing::debug!(faucet_code = bounded_code, http_status = %status, "faucet rejected airdrop request");
    mapped
}

fn map_http_error(error: &reqwest::Error) -> RpcServerError {
    if error.is_timeout() {
        RpcServerError::BackendTimeout
    } else {
        RpcServerError::BackendUnavailable(error.to_string())
    }
}

fn map_probe_error(error: ProbeError) -> RpcServerError {
    match error {
        ProbeError::Timeout => RpcServerError::BackendTimeout,
        ProbeError::Unavailable(message) | ProbeError::Malformed(message) => {
            RpcServerError::BackendUnavailable(message)
        }
        ProbeError::Mismatch => RpcServerError::BackendUnavailable(
            "faucet network descriptor does not match the gateway network".into(),
        ),
    }
}

fn endpoint(base: &Url, path: &str) -> Result<Url, ProbeError> {
    let mut base = base.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(path)
        .map_err(|error| ProbeError::Malformed(error.to_string()))
}

async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, ProbeError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(ProbeError::Malformed("response body exceeds 64 KiB".into()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ProbeError::http(&error))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(ProbeError::Malformed("response body exceeds 64 KiB".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, Clone, thiserror::Error)]
enum ProbeError {
    #[error("faucet request timed out")]
    Timeout,
    #[error("faucet is unavailable: {0}")]
    Unavailable(String),
    #[error("malformed faucet network descriptor: {0}")]
    Malformed(String),
    #[error("faucet network descriptor does not match the gateway network")]
    Mismatch,
}

impl ProbeError {
    fn http(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else {
            Self::Unavailable(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Json, Router,
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    use groundhog_json_rpc_types::error::codes;
    use serde_json::json;
    use tokio::{net::TcpListener, sync::Notify};

    use super::*;

    #[test]
    fn unconfigured_facade_does_not_register_airdrop_method() {
        let mut rpc = RpcModule::new(());

        merge_rpc(&mut rpc, None).expect("bug: merging no optional module should succeed");

        assert!(
            !rpc.method_names()
                .any(|name| name == "groundhog_requestAirdrop")
        );
    }

    #[tokio::test]
    async fn unconfigured_faucet_does_not_require_network_id() {
        let facade = HostedAirdrop::from_config(&GatewayConfig::default(), network_info())
            .await
            .expect("bug: an unconfigured faucet should be optional");

        assert!(facade.is_none());
    }

    #[tokio::test]
    async fn configured_faucet_requires_non_empty_network_id() {
        for network_id in [None, Some("  ".to_owned())] {
            let config = GatewayConfig {
                network_id,
                faucet_url: Some("https://faucet.example".into()),
                ..GatewayConfig::default()
            };

            let error = HostedAirdrop::from_config(&config, network_info())
                .await
                .expect_err("configured faucet should require a logical network ID");
            assert!(matches!(error, AirdropStartupError::MissingNetworkId));
        }
    }

    #[tokio::test]
    async fn configured_faucet_requires_genesis_hash() {
        let config = GatewayConfig {
            network_id: Some("devnet".into()),
            faucet_url: Some("https://faucet.example".into()),
            ..GatewayConfig::default()
        };

        let error = HostedAirdrop::from_config(&config, network_info())
            .await
            .expect_err("hosted faucet adapter requires its genesis hash");

        assert!(matches!(error, AirdropStartupError::MissingGenesisHash));
    }

    #[tokio::test]
    async fn descriptor_mismatch_is_startup_fatal() {
        let (base_url, _server) = spawn_server(
            network_descriptor("different-network"),
            StatusCode::OK,
            json!({}),
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let config = config(&base_url);

        let error = HostedAirdrop::from_config(&config, network_info())
            .await
            .expect_err("mismatched network identity must fail startup");

        assert!(error.to_string().contains("does not match"));
    }

    #[tokio::test]
    async fn descriptor_network_hash_mismatch_is_startup_fatal() {
        let mut descriptor = network_descriptor("devnet");
        descriptor["networkHash"] = json!(format!("0x{}", "34".repeat(32)));
        let (base_url, _server) = spawn_server(
            descriptor,
            StatusCode::OK,
            json!({}),
            Arc::new(AtomicUsize::new(0)),
        )
        .await;

        let error = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect_err("mismatched signing network must fail startup");

        assert!(matches!(error, AirdropStartupError::Descriptor(_)));
    }

    #[tokio::test]
    async fn credential_bearing_faucet_url_is_rejected_without_echoing_it() {
        let secret_url = "https://user:secret@example.com/faucet";

        let error = HostedAirdrop::from_config(&config(secret_url), network_info())
            .await
            .expect_err("credential-bearing faucet URL must fail startup");
        let message = error.to_string();

        assert!(message.contains("must not contain credentials"));
        assert!(!message.contains("secret"));
        assert!(!message.contains(secret_url));
    }

    #[tokio::test]
    async fn unreachable_faucet_starts_degraded_and_recovers_on_probe() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test listener should bind");
        let base_url = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("bug: test listener should have an address")
        );
        drop(listener);

        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("connection failure should not fail startup")
            .expect("configured facade should exist");
        let gateway_config = config(&base_url);
        let health = crate::health::GatewayHealth::new_with_airdrop(
            &gateway_config,
            crate::BUILD_VERSION,
            None,
            Some(facade.clone()),
        );
        let snapshot = serde_json::to_value(health.snapshot().await)
            .expect("bug: health snapshot should serialize");
        let probe = facade.probe(Duration::from_secs(1)).await;

        assert_eq!(snapshot["backends"]["faucet"]["configured"], true);
        assert_eq!(snapshot["backends"]["faucet"]["required"], true);
        assert!(!probe.up);
        assert!(!facade.is_validated().await);

        let listener = TcpListener::bind(base_url.trim_start_matches("http://"))
            .await
            .expect("bug: recovery server should bind the released address");
        let descriptor = network_descriptor("devnet");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/network",
                    get(move || {
                        let descriptor = descriptor.clone();
                        async move { Json(descriptor) }
                    }),
                ),
            )
            .await
            .expect("bug: recovery server should run");
        });

        let recovered = facade.probe(Duration::from_secs(1)).await;

        assert!(recovered.up);
        assert!(facade.is_validated().await);
        server.abort();
    }

    #[tokio::test]
    async fn post_is_not_retried_and_quota_maps_to_overloaded() {
        let requests = Arc::new(AtomicUsize::new(0));
        let (base_url, _server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::TOO_MANY_REQUESTS,
            faucet_error("airdrop_quota_exceeded", "quota exceeded"),
            requests.clone(),
        )
        .await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");
        let request = serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "0x01"
        }))
        .expect("bug: request fixture should decode");

        let error = facade
            .request_airdrop(request)
            .await
            .expect_err("quota response should fail the RPC");

        assert_eq!(error.code(), codes::OVERLOADED);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_degraded_requests_share_validation_and_never_post() {
        let network_requests = Arc::new(AtomicUsize::new(0));
        let post_requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/network",
                get({
                    let requests = network_requests.clone();
                    move || {
                        let requests = requests.clone();
                        async move {
                            requests.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(25)).await;
                            StatusCode::SERVICE_UNAVAILABLE
                        }
                    }
                }),
            )
            .route(
                "/v1/airdrop",
                post({
                    let requests = post_requests.clone();
                    move || {
                        let requests = requests.clone();
                        async move {
                            requests.fetch_add(1, Ordering::SeqCst);
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    }
                }),
            );
        let (base_url, _server) = spawn_app(app).await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("unavailable faucet should permit degraded startup")
            .expect("configured facade should exist");
        network_requests.store(0, Ordering::SeqCst);
        let request = airdrop_request();

        let (a, b, c, d) = tokio::join!(
            facade.request_airdrop(request.clone()),
            facade.request_airdrop(request.clone()),
            facade.request_airdrop(request.clone()),
            facade.request_airdrop(request),
        );

        for result in [a, b, c, d] {
            assert_eq!(
                result
                    .expect_err("validation failure should reject airdrop")
                    .code(),
                codes::BACKEND_UNAVAILABLE
            );
        }
        assert_eq!(network_requests.load(Ordering::SeqCst), 1);
        assert_eq!(post_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn probe_timeout_includes_waiting_for_active_validation() {
        let network_requests = Arc::new(AtomicUsize::new(0));
        let validation_started = Arc::new(Notify::new());
        let app = Router::new().route(
            "/v1/network",
            get({
                let requests = network_requests.clone();
                let validation_started = validation_started.clone();
                move || {
                    let requests = requests.clone();
                    let validation_started = validation_started.clone();
                    async move {
                        if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        validation_started.notify_one();
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    }
                }
            }),
        );
        let (base_url, _server) = spawn_app(app).await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("unavailable faucet should permit degraded startup")
            .expect("configured facade should exist");
        let active_validation = {
            let facade = facade.clone();
            tokio::spawn(async move { facade.request_airdrop(airdrop_request()).await })
        };
        tokio::time::timeout(Duration::from_secs(1), validation_started.notified())
            .await
            .expect("active validation should start");

        let probe = tokio::time::timeout(
            Duration::from_millis(40),
            facade.probe(Duration::from_millis(10)),
        )
        .await
        .expect("probe deadline must include validation lock contention");

        assert!(!probe.up);
        assert_eq!(probe.error.as_deref(), Some("faucet request timed out"));
        active_validation.abort();
    }

    #[tokio::test]
    async fn slow_forward_does_not_block_a_readiness_probe() {
        let post_started = Arc::new(Notify::new());
        let release_post = Arc::new(Notify::new());
        let app = Router::new()
            .route(
                "/v1/network",
                get(|| async { Json(network_descriptor("devnet")) }),
            )
            .route(
                "/v1/airdrop",
                post({
                    let post_started = post_started.clone();
                    let release_post = release_post.clone();
                    move || {
                        let post_started = post_started.clone();
                        let release_post = release_post.clone();
                        async move {
                            post_started.notify_one();
                            release_post.notified().await;
                            Json(json!({
                                "attemptStatus": "pending",
                                "recipient": format!("0x{}", "11".repeat(32)),
                                "rawAmount": "100000000",
                                "retryAfterSeconds": 5
                            }))
                        }
                    }
                }),
            );
        let (base_url, _server) = spawn_app(app).await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");
        let active_request = {
            let facade = facade.clone();
            tokio::spawn(async move { facade.request_airdrop(airdrop_request()).await })
        };
        tokio::time::timeout(Duration::from_secs(1), post_started.notified())
            .await
            .expect("faucet POST should start");

        let probe = facade.probe(Duration::from_millis(10)).await;

        assert!(probe.up);
        assert!(facade.is_validated().await);
        release_post.notify_one();
        active_request
            .await
            .expect("request task should complete")
            .expect("active request should remain valid");
    }

    #[tokio::test]
    async fn descriptor_invalidation_does_not_cancel_active_forward_and_blocks_later_posts() {
        let network_requests = Arc::new(AtomicUsize::new(0));
        let post_requests = Arc::new(AtomicUsize::new(0));
        let post_started = Arc::new(Notify::new());
        let release_post = Arc::new(Notify::new());
        let app = Router::new()
            .route(
                "/v1/network",
                get({
                    let requests = network_requests.clone();
                    move || {
                        let request = requests.fetch_add(1, Ordering::SeqCst);
                        async move {
                            Json(if request == 0 {
                                network_descriptor("devnet")
                            } else {
                                network_descriptor("different-network")
                            })
                        }
                    }
                }),
            )
            .route(
                "/v1/airdrop",
                post({
                    let requests = post_requests.clone();
                    let post_started = post_started.clone();
                    let release_post = release_post.clone();
                    move || {
                        let requests = requests.clone();
                        let post_started = post_started.clone();
                        let release_post = release_post.clone();
                        async move {
                            requests.fetch_add(1, Ordering::SeqCst);
                            post_started.notify_one();
                            release_post.notified().await;
                            Json(json!({
                                "attemptStatus": "succeeded",
                                "recipient": format!("0x{}", "11".repeat(32)),
                                "rawAmount": "100000000",
                                "txHash": format!("0x{}", "22".repeat(32)),
                                "height": 42,
                                "status": { "result": "ok" }
                            }))
                        }
                    }
                }),
            );
        let (base_url, _server) = spawn_app(app).await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");
        let active_request = {
            let facade = facade.clone();
            tokio::spawn(async move { facade.request_airdrop(airdrop_request()).await })
        };
        tokio::time::timeout(Duration::from_secs(1), post_started.notified())
            .await
            .expect("faucet POST should start");
        let invalidation = {
            let facade = facade.clone();
            tokio::spawn(async move { facade.probe(Duration::from_secs(1)).await })
        };
        let probe = invalidation.await.expect("probe task should complete");
        assert!(!probe.up);
        assert_eq!(network_requests.load(Ordering::SeqCst), 2);

        release_post.notify_one();
        active_request
            .await
            .expect("request task should complete")
            .expect("request validated before descriptor changed");

        let error = facade
            .request_airdrop(airdrop_request())
            .await
            .expect_err("mismatched descriptor must disable later execution");
        assert_eq!(error.code(), codes::BACKEND_UNAVAILABLE);
        assert_eq!(post_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pending_and_succeeded_responses_round_trip() {
        let pending_body = json!({
            "attemptStatus": "pending",
            "recipient": format!("0x{}", "11".repeat(32)),
            "rawAmount": "100000000",
            "txHash": format!("0x{}", "22".repeat(32)),
            "retryAfterSeconds": 5
        });
        let (pending_url, _pending_server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::ACCEPTED,
            pending_body,
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let pending = HostedAirdrop::from_config(&config(&pending_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist")
            .request_airdrop(airdrop_request())
            .await
            .expect("pending response should round-trip");

        assert!(matches!(pending, AirdropAttemptResponse::Pending { .. }));

        let succeeded_body = json!({
            "attemptStatus": "succeeded",
            "recipient": format!("0x{}", "11".repeat(32)),
            "rawAmount": "100000000",
            "txHash": format!("0x{}", "22".repeat(32)),
            "height": 42,
            "status": { "result": "ok" }
        });
        let (succeeded_url, _succeeded_server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::OK,
            succeeded_body,
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let succeeded = HostedAirdrop::from_config(&config(&succeeded_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist")
            .request_airdrop(airdrop_request())
            .await
            .expect("succeeded response should round-trip");

        assert!(matches!(
            succeeded,
            AirdropAttemptResponse::Succeeded { .. }
        ));
    }

    #[tokio::test]
    async fn faucet_post_timeout_maps_to_backend_timeout() {
        let descriptor = network_descriptor("devnet");
        let app = Router::new()
            .route(
                "/v1/network",
                get(move || {
                    let descriptor = descriptor.clone();
                    async move { Json(descriptor) }
                }),
            )
            .route(
                "/v1/airdrop",
                post(|| async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    StatusCode::ACCEPTED
                }),
            );
        let (base_url, _server) = spawn_app(app).await;
        let mut gateway_config = config(&base_url);
        gateway_config.request_timeout_ms = 20;
        let facade = HostedAirdrop::from_config(&gateway_config, network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");

        let error = facade
            .request_airdrop(airdrop_request())
            .await
            .expect_err("slow faucet POST should time out");

        assert_eq!(error.code(), codes::BACKEND_TIMEOUT);
    }

    #[tokio::test]
    async fn structured_error_on_http_202_is_mapped_before_status() {
        let (base_url, _server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::ACCEPTED,
            faucet_error("airdrop_already_pending", "attempt in progress"),
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");
        let request = serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "0x01"
        }))
        .expect("bug: request fixture should decode");

        let error = facade
            .request_airdrop(request)
            .await
            .expect_err("structured 202 error should fail the RPC");

        assert_eq!(error.code(), codes::OVERLOADED);
    }

    #[tokio::test]
    async fn malformed_success_response_maps_to_backend_unavailable() {
        let (base_url, _server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::OK,
            json!({ "attemptStatus": "unknown" }),
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");
        let request = serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "0x01"
        }))
        .expect("bug: request fixture should decode");

        let error = facade
            .request_airdrop(request)
            .await
            .expect_err("malformed success should fail the RPC");

        assert_eq!(error.code(), codes::BACKEND_UNAVAILABLE);
    }

    #[tokio::test]
    async fn succeeded_response_with_failed_status_maps_to_backend_unavailable() {
        let (base_url, _server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::OK,
            json!({
                "attemptStatus": "succeeded",
                "recipient": format!("0x{}", "11".repeat(32)),
                "rawAmount": "100000000",
                "txHash": format!("0x{}", "22".repeat(32)),
                "height": 42,
                "status": { "result": "reverted", "message": "denied" }
            }),
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");

        let error = facade
            .request_airdrop(airdrop_request())
            .await
            .expect_err("failed execution status must not be reported as succeeded");

        assert_eq!(error.code(), codes::BACKEND_UNAVAILABLE);
    }

    #[tokio::test]
    async fn forwarded_request_includes_configured_identity_and_public_fields() {
        let captured = Arc::new(Mutex::new(None::<HostedAirdropRequest>));
        let app = Router::new()
            .route(
                "/v1/network",
                get(|| async { Json(network_descriptor("devnet")) }),
            )
            .route(
                "/v1/airdrop",
                post({
                    let captured = captured.clone();
                    move |Json(request): Json<HostedAirdropRequest>| {
                        let captured = captured.clone();
                        async move {
                            *captured.lock().expect("bug: capture lock should succeed") =
                                Some(request);
                            (
                                StatusCode::ACCEPTED,
                                Json(json!({
                                    "attemptStatus": "pending",
                                    "recipient": format!("0x{}", "11".repeat(32)),
                                    "rawAmount": "100000000",
                                    "retryAfterSeconds": 5
                                })),
                            )
                        }
                    }
                }),
            );
        let (base_url, _server) = spawn_app(app).await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");

        facade
            .request_airdrop(airdrop_request())
            .await
            .expect("airdrop should be forwarded");

        let forwarded = captured
            .lock()
            .expect("bug: capture lock should succeed")
            .clone()
            .expect("bug: forwarded request should be captured");
        assert_eq!(
            serde_json::to_value(forwarded).expect("forwarded request should serialize"),
            json!({
                "recipient": format!("0x{}", "11".repeat(32)),
                "clientNonce": "0x01",
                "networkId": "devnet",
                "networkHash": format!("0x{}", "12".repeat(32)),
                "genesisHash": format!("0x{}", "42".repeat(32))
            })
        );
    }

    #[tokio::test]
    async fn success_body_with_error_status_maps_to_backend_unavailable() {
        let (base_url, _server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::BAD_GATEWAY,
            json!({
                "attemptStatus": "pending",
                "recipient": format!("0x{}", "11".repeat(32)),
                "rawAmount": "100000000",
                "retryAfterSeconds": 5
            }),
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");
        let request = serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "0x01"
        }))
        .expect("bug: request fixture should decode");

        let error = facade
            .request_airdrop(request)
            .await
            .expect_err("HTTP error with a success body should fail the RPC");

        assert_eq!(error.code(), codes::BACKEND_UNAVAILABLE);
    }

    #[tokio::test]
    async fn mismatched_response_recipient_maps_to_backend_unavailable() {
        let (base_url, _server) = spawn_server(
            network_descriptor("devnet"),
            StatusCode::ACCEPTED,
            json!({
                "attemptStatus": "pending",
                "recipient": format!("0x{}", "99".repeat(32)),
                "rawAmount": "100000000",
                "retryAfterSeconds": 5
            }),
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let facade = HostedAirdrop::from_config(&config(&base_url), network_info())
            .await
            .expect("matching descriptor should pass startup")
            .expect("configured facade should exist");

        let error = facade
            .request_airdrop(airdrop_request())
            .await
            .expect_err("faucet must not redirect an airdrop response to another recipient");

        assert_eq!(error.code(), codes::BACKEND_UNAVAILABLE);
    }

    #[test]
    fn structured_faucet_errors_map_to_public_categories() {
        let cases = [
            (
                StatusCode::BAD_REQUEST,
                "airdrop_invalid_client_nonce",
                codes::INVALID_PARAMS,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                "airdrop_quota_exceeded",
                codes::OVERLOADED,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                "airdrop_recipient_pending_limit_exceeded",
                codes::OVERLOADED,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                "airdrop_capacity_exceeded",
                codes::OVERLOADED,
            ),
            (
                StatusCode::ACCEPTED,
                "airdrop_already_pending",
                codes::OVERLOADED,
            ),
            (
                StatusCode::CONFLICT,
                "airdrop_attempt_terminal",
                codes::TX_REJECTED,
            ),
            (
                StatusCode::BAD_GATEWAY,
                "airdrop_transaction_failed",
                codes::TX_REJECTED,
            ),
            (
                StatusCode::BAD_GATEWAY,
                "airdrop_submit_failed",
                codes::BACKEND_UNAVAILABLE,
            ),
            (
                StatusCode::BAD_REQUEST,
                "airdrop_network_mismatch",
                codes::BACKEND_UNAVAILABLE,
            ),
            (
                StatusCode::CONFLICT,
                "airdrop_attempt_conflict",
                codes::BACKEND_UNAVAILABLE,
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "airdrop_attempt_state_invalid",
                codes::BACKEND_UNAVAILABLE,
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "faucet_policy_store_unavailable",
                codes::BACKEND_UNAVAILABLE,
            ),
            (
                StatusCode::BAD_GATEWAY,
                "airdrop_submit_amount_mismatch",
                codes::BACKEND_UNAVAILABLE,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                "unknown_future_code",
                codes::BACKEND_UNAVAILABLE,
            ),
        ];

        for (status, code, expected) in cases {
            let mapped = map_faucet_error(
                status,
                FaucetError {
                    code: code.into(),
                    message: "test error".into(),
                },
            );
            let rpc_error = ErrorObjectOwned::from(mapped);
            assert_eq!(rpc_error.code(), expected, "wrong mapping for {code}");
        }
    }

    async fn spawn_server(
        descriptor: serde_json::Value,
        post_status: StatusCode,
        post_body: serde_json::Value,
        post_requests: Arc<AtomicUsize>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route(
                "/v1/network",
                get({
                    let descriptor = descriptor.clone();
                    move || async move { Json(descriptor.clone()) }
                }),
            )
            .route(
                "/v1/airdrop",
                post(move || {
                    let body = post_body.clone();
                    let requests = post_requests.clone();
                    async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        (post_status, Json(body))
                    }
                }),
            );
        spawn_app(app).await
    }

    async fn spawn_app(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test HTTP server should bind");
        let address = listener
            .local_addr()
            .expect("bug: test HTTP server should have an address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("bug: test HTTP server should run");
        });
        (format!("http://{address}"), server)
    }

    fn airdrop_request() -> AirdropRequest {
        serde_json::from_value(json!({
            "recipient": format!("0x{}", "11".repeat(32)),
            "clientNonce": "0x01"
        }))
        .expect("bug: request fixture should decode")
    }

    fn config(base_url: &str) -> GatewayConfig {
        GatewayConfig {
            network_id: Some("devnet".into()),
            network_hash: Some(groundhog_json_rpc_types::NetworkHashHex::from([0x12; 32])),
            genesis_hash: Some(genesis_hash()),
            faucet_url: Some(base_url.into()),
            request_timeout_ms: 200,
            backend_connect_timeout_ms: 100,
            ..GatewayConfig::default()
        }
    }

    fn network_info() -> NetworkInfo {
        serde_json::from_value(json!({
            "networkHash": format!("0x{}", "12".repeat(32))
        }))
        .expect("bug: network fixture should decode")
    }

    fn genesis_hash() -> SHA256Hash {
        serde_json::from_value(json!(format!("0x{}", "42".repeat(32))))
            .expect("bug: genesis hash fixture should decode")
    }

    fn network_descriptor(network_id: &str) -> serde_json::Value {
        json!({
            "schemaVersion": NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": NETWORK_DESCRIPTOR_KIND,
            "networkId": network_id,
            "networkHash": format!("0x{}", "12".repeat(32)),
            "protocol": "hogp/test",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "endpoints": { "rpcUrl": "https://rpc.example" },
            "nativeHog": {
                "tokenAddress": format!("0x{}", "11".repeat(32)),
                "decimals": 18,
                "transferEntrypoint": "0x0102030405060708"
            },
            "faucet": { "walletAddress": format!("0x{}", "22".repeat(32)) },
            "walletCreate": {
                "prefundRawAmount": "1000",
                "walletContractHash": format!("0x{}", "33".repeat(32))
            },
            "devWallet": {
                "deployEntrypoint": "0x0102030405060708",
                "doActionEntrypoint": "0x1112131415161718"
            }
        })
    }

    fn faucet_error(code: &str, message: &str) -> serde_json::Value {
        json!({ "error": { "code": code, "message": message } })
    }
}
