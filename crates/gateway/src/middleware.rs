//! Tower middleware stack: timeout, CORS, request tracing.
//!
//! Layered via `tower::ServiceBuilder` and applied to the jsonrpsee
//! server's HTTP transport.
//!
//! Phase 4 adds:
//! - Per-IP rate limiting (token bucket)
//! - Request size limits
//! - Per-method Prometheus histograms

use std::time::Duration;

use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::config::GatewayConfig;

/// Build the Tower middleware stack from configuration.
///
/// Layer order (outermost → innermost):
/// 1. **CORS** — permissive for dev, restrictive for prod
/// 2. **Tracing** — request/response spans for observability
/// 3. **Timeout** — per-request timeout from config
pub fn middleware_stack(
    config: &GatewayConfig,
) -> tower::ServiceBuilder<
    tower::layer::util::Stack<
        tower_http::timeout::TimeoutLayer,
        tower::layer::util::Stack<
            TraceLayer,
            tower::layer::util::Stack<CorsLayer, tower::layer::util::Identity>,
        >,
    >,
> {
    ServiceBuilder::new()
        .layer(cors_layer())
        .layer(TraceLayer::new_for_http())
        .layer(tower_http::timeout::TimeoutLayer::new(
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
