mod airdrop;
/// Faucet service configuration types.
pub mod config;

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use groundhog_json_rpc_types::network_descriptor::NetworkDescriptor;
use serde::Serialize;
use std::sync::Arc;
use tokio::net::TcpListener;

pub const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1:8081";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaucetConfig {
    pub listen_address: String,
    pub network_descriptor: NetworkDescriptor,
}

impl FaucetConfig {
    pub fn new(network_descriptor: NetworkDescriptor) -> Self {
        Self {
            listen_address: DEFAULT_LISTEN_ADDRESS.to_owned(),
            network_descriptor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Clone)]
struct AppState {
    network_descriptor: Arc<NetworkDescriptor>,
}

pub fn app(config: FaucetConfig) -> Router {
    let state = AppState {
        network_descriptor: Arc::new(config.network_descriptor),
    };
    Router::new()
        .route("/health", get(health))
        .route("/v1/network", get(network))
        .route("/v1/airdrop", post(airdrop::request_airdrop))
        .with_state(state)
}

pub async fn serve(config: FaucetConfig) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(&config.listen_address).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(listen = %local_addr, "faucet service listening");
    axum::serve(listener, app(config)).await?;
    Ok(())
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
    use axum::http::{Request, StatusCode};
    use groundhog_json_rpc_types::network_descriptor::{
        NETWORK_DESCRIPTOR_SCHEMA_VERSION, NetworkDescriptor,
    };
    use tower::ServiceExt;

    use super::{DEFAULT_LISTEN_ADDRESS, FaucetConfig, app};

    #[test]
    fn default_config_uses_local_listen_address() {
        let config = FaucetConfig::new(test_network_descriptor());

        assert_eq!(config.listen_address, DEFAULT_LISTEN_ADDRESS);
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = app(FaucetConfig::new(test_network_descriptor()))
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
        let response = app(FaucetConfig::new(descriptor.clone()))
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
        assert_eq!(
            json["endpoints"]["faucetUrl"],
            "https://faucet.devnet.example"
        );
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
    async fn airdrop_validates_request_then_reports_not_implemented() {
        let response = app(FaucetConfig::new(test_network_descriptor()))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/airdrop")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "recipient": format!("0x{}", "11".repeat(32))
                        })
                        .to_string(),
                    ))
                    .expect("bug: airdrop request should build"),
            )
            .await
            .expect("bug: airdrop request should complete");

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("bug: airdrop body should read");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("bug: airdrop body should be JSON");
        assert_eq!(json["error"]["code"], "airdrop_not_implemented");
    }

    #[tokio::test]
    async fn airdrop_rejects_malformed_recipient() {
        let response = app(FaucetConfig::new(test_network_descriptor()))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/airdrop")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "recipient": "0xdeadbeef"
                        })
                        .to_string(),
                    ))
                    .expect("bug: airdrop request should build"),
            )
            .await
            .expect("bug: airdrop request should complete");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    fn test_network_descriptor() -> NetworkDescriptor {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": "groundhog-network",
            "networkId": "devnet-preview",
            "protocol": "x23",
            "genesisHash": format!("0x{}", "00".repeat(32)),
            "endpoints": {
                "rpcUrl": "https://rpc.devnet.example",
                "faucetUrl": "https://faucet.devnet.example"
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
}
