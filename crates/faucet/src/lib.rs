use axum::{Json, Router, routing::get};
use serde::Serialize;
use tokio::net::TcpListener;

pub const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1:8081";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaucetConfig {
    pub listen_address: String,
}

impl Default for FaucetConfig {
    fn default() -> Self {
        Self {
            listen_address: DEFAULT_LISTEN_ADDRESS.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HealthResponse {
    status: &'static str,
}

pub fn app() -> Router {
    Router::new().route("/health", get(health))
}

pub async fn serve(config: FaucetConfig) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(&config.listen_address).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(listen = %local_addr, "faucet service listening");
    axum::serve(listener, app()).await?;
    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::{DEFAULT_LISTEN_ADDRESS, FaucetConfig, app};

    #[test]
    fn default_config_uses_local_listen_address() {
        assert_eq!(
            FaucetConfig::default().listen_address,
            DEFAULT_LISTEN_ADDRESS
        );
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = app()
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
}
