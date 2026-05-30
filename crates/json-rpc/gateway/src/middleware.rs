//! Tower middleware stack: CORS and request timeout.
//!
//! Request-level tracing is handled by jsonrpsee's built-in logger
//! rather than tower-http's `TraceLayer`, which wraps the response body
//! in a type incompatible with jsonrpsee's HTTP body constraints.
//!
//! Phase 4 adds:
//! - Per-IP rate limiting (token bucket)
//! - Request size limits
//! - Per-method Prometheus histograms

use std::time::Duration;

use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;

use crate::config::GatewayConfig;

/// Build the Tower HTTP middleware stack from configuration.
///
/// Layer order (outermost → innermost):
/// 1. **CORS** — permissive for dev, restrictive for prod
/// 2. **Timeout** — per-request timeout from config
pub fn http_middleware(
    config: &GatewayConfig,
) -> ServiceBuilder<
    tower::layer::util::Stack<
        TimeoutLayer,
        tower::layer::util::Stack<CorsLayer, tower::layer::util::Identity>,
    >,
> {
    ServiceBuilder::new()
        .layer(cors_layer())
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            Duration::from_millis(config.request_timeout_ms),
        ))
}

/// CORS layer. Permissive during development.
///
/// TODO: make origins configurable for production deployments.
fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}
