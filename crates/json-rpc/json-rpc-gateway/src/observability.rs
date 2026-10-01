use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use std::{net::SocketAddr, time::Instant};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

use crate::{
    health::{GatewayHealth, HealthStatus},
    metrics::GatewayMetrics,
};

#[derive(Clone)]
struct ObservabilityState {
    health: GatewayHealth,
    metrics: GatewayMetrics,
}

pub struct ObservabilityServer {
    /// Address actually bound by the observability listener.
    #[cfg_attr(not(test), allow(dead_code))]
    pub local_addr: SocketAddr,
    /// Join handle for the Axum observability server task.
    pub handle: JoinHandle<std::io::Result<()>>,
}

/// Spawn the private health/metrics HTTP server.
pub async fn spawn_observability_server(
    listen_address: &str,
    health: GatewayHealth,
    metrics: GatewayMetrics,
    shutdown: oneshot::Receiver<()>,
    enabled: bool,
) -> Result<ObservabilityServer, std::io::Error> {
    let listener = TcpListener::bind(listen_address).await?;
    let local_addr = listener.local_addr()?;
    let state = ObservabilityState { health, metrics };
    let app = Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/health", get(healthz))
        .route("/metrics", get(metrics_endpoint))
        .merge(crate::heap_profiling::routes(enabled))
        .fallback(unknown_endpoint)
        .method_not_allowed_fallback(method_not_allowed_endpoint)
        .with_state(state);

    tracing::info!(listen = %local_addr, "gateway observability server listening");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
            })
            .await
    });
    Ok(ObservabilityServer { local_addr, handle })
}

async fn livez(State(state): State<ObservabilityState>) -> Response {
    let started_at = Instant::now();
    let response = Json(json!({ "status": "ok" })).into_response();
    state.metrics.observe_http_request(
        "livez",
        crate::metrics::status_class(response.status().as_u16()),
        started_at.elapsed(),
    );
    response
}

async fn readyz(State(state): State<ObservabilityState>) -> Response {
    readiness_endpoint(state, "readyz").await
}

async fn healthz(State(state): State<ObservabilityState>) -> Response {
    readiness_endpoint(state, "health").await
}

async fn readiness_endpoint(state: ObservabilityState, endpoint: &'static str) -> Response {
    let started_at = Instant::now();
    let snapshot = state.health.snapshot().await;
    let status = match snapshot.status() {
        HealthStatus::Ok | HealthStatus::Degraded => StatusCode::OK,
        HealthStatus::Starting | HealthStatus::Error => StatusCode::SERVICE_UNAVAILABLE,
    };
    let response = (status, Json(snapshot)).into_response();
    let duration = started_at.elapsed();
    tracing::debug!(
        endpoint,
        http_status = response.status().as_u16(),
        status_class = crate::metrics::status_class(response.status().as_u16()),
        duration_ms = duration.as_millis(),
        "gateway readiness endpoint completed",
    );
    state.metrics.observe_http_request(
        endpoint,
        crate::metrics::status_class(response.status().as_u16()),
        duration,
    );
    response
}

async fn metrics_endpoint(State(state): State<ObservabilityState>) -> Response {
    let started_at = Instant::now();
    let snapshot = state.health.snapshot().await;
    // The registry is gathered and encoded once per scrape. This request's
    // own sample is recorded afterwards, so it appears on the next scrape.
    let (status, response) = match state.metrics.gather(&snapshot) {
        Ok(body) => (
            StatusCode::OK,
            ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response(),
        ),
        Err(err) => {
            tracing::error!(error = %err, "failed to encode gateway metrics");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            )
        }
    };
    let duration = started_at.elapsed();
    state.metrics.observe_http_request(
        "metrics",
        crate::metrics::status_class(status.as_u16()),
        duration,
    );
    tracing::debug!(
        endpoint = "metrics",
        status = status.as_u16(),
        duration_ms = duration.as_millis(),
        "gateway metrics endpoint completed",
    );
    response
}

async fn unknown_endpoint(State(state): State<ObservabilityState>) -> Response {
    fixed_status_endpoint(&state, "unknown", StatusCode::NOT_FOUND)
}

async fn method_not_allowed_endpoint(State(state): State<ObservabilityState>) -> Response {
    fixed_status_endpoint(&state, "unknown", StatusCode::METHOD_NOT_ALLOWED)
}

fn fixed_status_endpoint(
    state: &ObservabilityState,
    endpoint: &'static str,
    status: StatusCode,
) -> Response {
    let started_at = Instant::now();
    let response = status.into_response();
    state.metrics.observe_http_request(
        endpoint,
        crate::metrics::status_class(response.status().as_u16()),
        started_at.elapsed(),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{GatewayConfig, HealthBackend},
        health::{GatewayHealth, GatewayHealthSnapshot},
        metrics::GatewayMetrics,
    };
    use std::time::Duration;
    use thruput_build_info::BuildVersion;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn observability_routes_return_expected_statuses() {
        let config = GatewayConfig::default();
        let build_version = BuildVersion::new("test", Some("test-platform"));
        let health = GatewayHealth::new(&config, build_version, None);
        let metrics =
            GatewayMetrics::new(build_version, None).expect("bug: metrics should register");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = spawn_observability_server("127.0.0.1:0", health, metrics, shutdown_rx, false)
            .await
            .expect("observability server should bind");

        let livez = request(server.local_addr, "/livez").await;
        assert!(livez.starts_with("HTTP/1.1 200 OK"), "{livez}");

        let readyz = request(server.local_addr, "/readyz").await;
        assert!(
            readyz.starts_with("HTTP/1.1 503 Service Unavailable"),
            "{readyz}"
        );
        assert!(readyz.contains("\"status\":\"starting\""), "{readyz}");

        let health = request(server.local_addr, "/health").await;
        assert!(
            health.starts_with("HTTP/1.1 503 Service Unavailable"),
            "{health}"
        );
        assert!(health.contains("\"version\":\"test\""), "{health}");
        assert!(
            health.contains("\"platform_version\":\"test-platform\""),
            "{health}"
        );

        // A scrape's own sample is recorded after encoding, so it shows up
        // on the following scrape.
        request(server.local_addr, "/metrics").await;
        let metrics = request(server.local_addr, "/metrics").await;
        assert!(metrics.starts_with("HTTP/1.1 200 OK"), "{metrics}");
        assert!(
            metrics.contains("content-type: text/plain; version=0.0.4"),
            "{metrics}"
        );
        assert!(metrics.contains("groundhog_gateway_health_status"));
        assert!(metrics.contains("groundhog_gateway_backend_up"));
        assert!(metrics.contains("groundhog_gateway_chain_tip_height"));
        assert!(metrics.contains("groundhog_gateway_chain_tip_age_seconds"));
        assert!(metrics.contains("groundhog_gateway_http_requests_total"));
        assert!(metrics.contains("endpoint=\"livez\",status_class=\"2xx\""));
        assert!(metrics.contains("endpoint=\"metrics\",status_class=\"2xx\""));
        assert!(metrics.contains("groundhog_gateway_build_info"));

        let unknown = request(server.local_addr, "/missing").await;
        assert!(unknown.starts_with("HTTP/1.1 404 Not Found"), "{unknown}");

        let heap = request(server.local_addr, "/debug/pprof/heap").await;
        assert!(heap.starts_with("HTTP/1.1 404 Not Found"), "{heap}");

        let method_not_allowed = request_with_method(server.local_addr, "POST", "/livez").await;
        assert!(
            method_not_allowed.starts_with("HTTP/1.1 405 Method Not Allowed"),
            "{method_not_allowed}"
        );

        let metrics = request(server.local_addr, "/metrics").await;
        assert!(metrics.contains("endpoint=\"unknown\",status_class=\"4xx\""));

        let _ = shutdown_tx.send(());
        server
            .handle
            .await
            .expect("observability task should join")
            .expect("observability server should stop cleanly");
    }

    #[tokio::test]
    async fn stale_readiness_snapshot_returns_unavailable() {
        let config = GatewayConfig {
            health_probe_interval_ms: 10,
            ..GatewayConfig::default()
        };
        let health = GatewayHealth::new(&config, BuildVersion::new("test", None), None);
        let mut snapshot = GatewayHealthSnapshot::for_test(
            HealthStatus::Ok,
            Some((1, 0)),
            &[
                (HealthBackend::Scheduler, true),
                (HealthBackend::Indexer, true),
            ],
        );
        snapshot.snapshot_at = tokio::time::Instant::now()
            .checked_sub(Duration::from_millis(31))
            .expect("test duration should be in range");
        health.update_for_test(snapshot).await;

        let metrics = GatewayMetrics::for_tests();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = spawn_observability_server("127.0.0.1:0", health, metrics, shutdown_rx, false)
            .await
            .expect("observability server should bind");

        let readyz = request(server.local_addr, "/readyz").await;
        assert!(
            readyz.starts_with("HTTP/1.1 503 Service Unavailable"),
            "{readyz}"
        );
        assert!(readyz.contains("\"status\":\"error\""), "{readyz}");
        assert!(readyz.contains("\"snapshot_age_ms\":"), "{readyz}");

        let healthz = request(server.local_addr, "/health").await;
        assert!(
            healthz.starts_with("HTTP/1.1 503 Service Unavailable"),
            "{healthz}"
        );

        let _ = shutdown_tx.send(());
        server
            .handle
            .await
            .expect("observability task should join")
            .expect("observability server should stop cleanly");
    }

    async fn request(addr: SocketAddr, path: &str) -> String {
        request_with_method(addr, "GET", path).await
    }

    async fn request_with_method(addr: SocketAddr, method: &str, path: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("test client should connect");
        stream
            .write_all(
                format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("test request should write");
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_end(&mut response),
        )
        .await
        .expect("test response should not hang")
        .expect("test response should read");
        String::from_utf8(response).expect("response should be utf8")
    }
}
