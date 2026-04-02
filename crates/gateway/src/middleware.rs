//! Tower middleware stack: timeout, CORS, request tracing.
//!
//! TODO: wire into jsonrpsee server in `main.rs`. Now that we're on
//! jsonrpsee 0.25 + tower 0.5, the version conflict is resolved.
//! Need to build the middleware stack via `tower::ServiceBuilder` and
//! pass it to the server builder.
//!
//! Phase 4 adds:
//! - Per-IP rate limiting (token bucket)
//! - Request size limits
//! - Per-method Prometheus histograms

use std::time::Duration;

use tower_http::cors::CorsLayer;

use crate::config::GatewayConfig;

/// CORS layer. Permissive during development.
///
/// TODO: make origins configurable for production deployments.
#[allow(dead_code)]
pub fn cors_layer(_config: &GatewayConfig) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}

/// Request timeout duration from config.
#[allow(dead_code)]
pub fn timeout(config: &GatewayConfig) -> Duration {
    Duration::from_millis(config.request_timeout_ms)
}
