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

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use axum::http::{HeaderValue, Method, Request, Response, StatusCode, header};
use tower::{Layer, Service, ServiceBuilder};
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;

use crate::config::GatewayConfig;
use crate::metrics::{GatewayMetrics, status_class};

/// A configured CORS origin the HTTP layer cannot use.
#[derive(Debug, thiserror::Error)]
#[error("invalid cors_allowed_origins entry `{origin}`: {message}")]
pub(crate) struct InvalidCorsOrigin {
    origin: String,
    message: String,
}

/// Build the Tower HTTP middleware stack from configuration.
///
/// Layer order (outermost → innermost):
/// 1. **CORS** — origins from `cors_allowed_origins`; empty emits no
///    CORS headers
/// 2. **Timeout** — per-request timeout from config
#[allow(clippy::type_complexity)]
pub(crate) fn http_middleware(
    config: &GatewayConfig,
    metrics: GatewayMetrics,
) -> Result<
    ServiceBuilder<
        tower::layer::util::Stack<
            TimeoutLayer,
            tower::layer::util::Stack<
                CorsLayer,
                tower::layer::util::Stack<HttpMetricsLayer, tower::layer::util::Identity>,
            >,
        >,
    >,
    InvalidCorsOrigin,
> {
    Ok(ServiceBuilder::new()
        .layer(HttpMetricsLayer::new(metrics, "jsonrpc"))
        .layer(cors_layer(config)?)
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            config.http_request_timeout(),
        )))
}

/// CORS layer built from `cors_allowed_origins`.
///
/// An empty origin list produces a layer that emits no CORS headers, so
/// browsers refuse cross-origin calls. Configured origins are allowed
/// for JSON-RPC POSTs and their preflights.
fn cors_layer(config: &GatewayConfig) -> Result<CorsLayer, InvalidCorsOrigin> {
    if config.cors_allowed_origins.is_empty() {
        return Ok(CorsLayer::new());
    }
    let origins = config
        .cors_allowed_origins
        .iter()
        .map(|origin| {
            HeaderValue::from_str(origin).map_err(|error| InvalidCorsOrigin {
                origin: origin.clone(),
                message: error.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE]))
}

#[derive(Clone)]
pub(crate) struct HttpMetricsLayer {
    metrics: GatewayMetrics,
    endpoint: &'static str,
}

impl HttpMetricsLayer {
    fn new(metrics: GatewayMetrics, endpoint: &'static str) -> Self {
        Self { metrics, endpoint }
    }
}

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpMetricsService {
            inner,
            metrics: self.metrics.clone(),
            endpoint: self.endpoint,
        }
    }
}

#[derive(Clone)]
pub(crate) struct HttpMetricsService<S> {
    inner: S,
    metrics: GatewayMetrics,
    endpoint: &'static str,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for HttpMetricsService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
    S::Error: 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let started_at = Instant::now();
        let metrics = self.metrics.clone();
        let endpoint = self.endpoint;
        let future = self.inner.call(req);
        Box::pin(async move {
            match future.await {
                Ok(response) => {
                    let status_class = status_class(response.status().as_u16());
                    let duration = started_at.elapsed();
                    tracing::debug!(
                        endpoint,
                        status = response.status().as_u16(),
                        status_class,
                        duration_ms = duration.as_millis(),
                        "gateway HTTP request completed",
                    );
                    metrics.observe_http_request(endpoint, status_class, duration);
                    Ok(response)
                }
                Err(err) => {
                    let duration = started_at.elapsed();
                    tracing::debug!(
                        endpoint,
                        status = StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                        status_class = status_class(StatusCode::INTERNAL_SERVER_ERROR.as_u16()),
                        duration_ms = duration.as_millis(),
                        "gateway HTTP request failed",
                    );
                    metrics.observe_http_request(
                        endpoint,
                        status_class(StatusCode::INTERNAL_SERVER_ERROR.as_u16()),
                        duration,
                    );
                    Err(err)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        health::{GatewayHealthSnapshot, HealthStatus},
        metrics::GatewayMetrics,
    };
    use axum::body::Body;
    use std::time::Duration;
    use tower::{ServiceExt, service_fn};

    #[tokio::test]
    async fn http_middleware_records_timeout_status_class() {
        let config = GatewayConfig {
            request_timeout_ms: 1,
            ..GatewayConfig::default()
        };
        let metrics = GatewayMetrics::for_tests();
        let mut service = http_middleware(&config, metrics.clone())
            .expect("bug: default config should build middleware")
            .service(service_fn(|_request: Request<Body>| async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
            }));

        let response = service
            .ready()
            .await
            .expect("service should become ready")
            .call(Request::new(Body::empty()))
            .await
            .expect("timeout layer returns a response");

        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics.gather(&snapshot).expect("metrics should encode");
        assert!(body.contains(
            "groundhog_gateway_http_requests_total{endpoint=\"jsonrpc\",status_class=\"4xx\"} 1"
        ));
    }

    async fn cors_response_for(config: &GatewayConfig, origin: &str) -> Response<Body> {
        let mut service = http_middleware(config, GatewayMetrics::for_tests())
            .expect("bug: config should build middleware")
            .service(service_fn(|_request: Request<Body>| async move {
                Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
            }));
        let request = Request::builder()
            .method("POST")
            .header("origin", origin)
            .body(Body::empty())
            .expect("bug: test request should build");
        service
            .ready()
            .await
            .expect("bug: service should become ready")
            .call(request)
            .await
            .expect("bug: request should complete")
    }

    #[tokio::test]
    async fn default_config_emits_no_cors_headers() {
        let config = GatewayConfig::default();

        let response = cors_response_for(&config, "http://localhost:5173").await;

        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[tokio::test]
    async fn configured_origin_is_allowed_and_others_are_not() {
        let config = GatewayConfig {
            cors_allowed_origins: vec!["http://localhost:5173".into()],
            ..GatewayConfig::default()
        };

        let allowed = cors_response_for(&config, "http://localhost:5173").await;
        assert_eq!(
            allowed
                .headers()
                .get("access-control-allow-origin")
                .map(|value| value.to_str().expect("bug: header should be ASCII")),
            Some("http://localhost:5173")
        );

        let denied = cors_response_for(&config, "http://evil.example").await;
        assert!(
            denied
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
    }

    #[test]
    fn malformed_configured_origin_is_a_startup_error() {
        let config = GatewayConfig {
            cors_allowed_origins: vec!["bad\norigin".into()],
            ..GatewayConfig::default()
        };

        let error = http_middleware(&config, GatewayMetrics::for_tests())
            .err()
            .expect("bug: malformed origin should fail middleware construction");

        assert!(error.to_string().contains("cors_allowed_origins"));
    }
}
