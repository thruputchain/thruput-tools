//! Tower middleware stack: request metrics and timeout (CORS wraps the
//! connection service in `serve`, outside the rate limiter).
//!
//! Request-level tracing is handled by jsonrpsee's built-in logger
//! rather than tower-http's `TraceLayer`, which wraps the response body
//! in a type incompatible with jsonrpsee's HTTP body constraints.
//!
//! Per-IP rate limiting lives in `rate_limit`, applied per connection
//! in `serve`; request size limits are set on the jsonrpsee server
//! config.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use axum::http::{HeaderValue, Request, Response, StatusCode};
use tower::{Layer, Service, ServiceBuilder};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::timeout::TimeoutLayer;

use crate::config::GatewayConfig;
use crate::metrics::{GatewayMetrics, status_class};

/// A configured CORS origin is not a valid HTTP header value.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid CORS origin `{origin}` in cors_allowed_origins")]
pub(crate) struct InvalidCorsOrigin {
    origin: String,
}

/// Build the Tower HTTP middleware stack from configuration.
///
/// Layer order (outermost → innermost):
/// 1. **Metrics** — per-request HTTP metrics
/// 2. **Timeout** — per-request timeout from config
///
/// CORS is not part of this stack: it wraps the whole per-connection
/// service (outside the rate limiter) in `serve`, so admission
/// rejections such as 429 still carry CORS headers that a browser
/// client can observe.
#[allow(clippy::type_complexity)]
pub(crate) fn http_middleware(
    config: &GatewayConfig,
    metrics: GatewayMetrics,
) -> ServiceBuilder<
    tower::layer::util::Stack<
        TimeoutLayer,
        tower::layer::util::Stack<HttpMetricsLayer, tower::layer::util::Identity>,
    >,
> {
    ServiceBuilder::new()
        .layer(HttpMetricsLayer::new(metrics, "jsonrpc"))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            config.http_request_timeout(),
        ))
}

/// CORS layer honoring `cors_allowed_origins`.
///
/// An empty list or a `"*"` entry allows any origin, preserving the
/// dev-friendly default; otherwise only the listed origins are allowed.
/// `Retry-After` is exposed so browser scripts can read the backoff on
/// rate-limited responses.
pub(crate) fn cors_layer(config: &GatewayConfig) -> Result<CorsLayer, InvalidCorsOrigin> {
    let origins = &config.cors_allowed_origins;
    let allow_origin = if origins.is_empty() || origins.iter().any(|origin| origin == "*") {
        AllowOrigin::any()
    } else {
        let values = origins
            .iter()
            .map(|origin| {
                HeaderValue::from_str(origin).map_err(|_| InvalidCorsOrigin {
                    origin: origin.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        AllowOrigin::list(values)
    };
    Ok(CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
        .expose_headers([axum::http::header::RETRY_AFTER]))
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
        let mut service = http_middleware(&config, metrics.clone()).service(service_fn(
            |_request: Request<Body>| async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
            },
        ));

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

    async fn cors_response_for_origin(
        config: &GatewayConfig,
        origin: &'static str,
    ) -> Response<Body> {
        let mut service = cors_layer(config)
            .expect("CORS config should build")
            .layer(service_fn(|_request: Request<Body>| async move {
                Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
            }));
        let request = Request::builder()
            .header("origin", origin)
            .body(Body::empty())
            .expect("request should build");
        service
            .ready()
            .await
            .expect("service should become ready")
            .call(request)
            .await
            .expect("CORS layer should answer")
    }

    #[tokio::test]
    async fn cors_defaults_to_any_origin() {
        let config = GatewayConfig::default();

        let response = cors_response_for_origin(&config, "http://anywhere.example").await;

        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("*")
        );
    }

    #[tokio::test]
    async fn cors_honors_configured_origin_list() {
        let config = GatewayConfig {
            cors_allowed_origins: vec!["http://localhost:3000".into()],
            ..GatewayConfig::default()
        };

        let allowed = cors_response_for_origin(&config, "http://localhost:3000").await;
        assert_eq!(
            allowed
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("http://localhost:3000")
        );

        let denied = cors_response_for_origin(&config, "http://evil.example").await;
        assert!(
            denied
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "unlisted origin must not be allowed"
        );
    }

    #[tokio::test]
    async fn cors_star_entry_allows_any_origin() {
        let config = GatewayConfig {
            cors_allowed_origins: vec!["*".into()],
            ..GatewayConfig::default()
        };

        let response = cors_response_for_origin(&config, "http://anywhere.example").await;

        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("*")
        );
    }

    #[test]
    fn cors_rejects_an_unencodable_origin() {
        let config = GatewayConfig {
            cors_allowed_origins: vec!["bad\norigin".into()],
            ..GatewayConfig::default()
        };

        let err = cors_layer(&config).expect_err("invalid origin should fail");

        assert_eq!(
            err,
            InvalidCorsOrigin {
                origin: "bad\norigin".into()
            }
        );
    }
}
