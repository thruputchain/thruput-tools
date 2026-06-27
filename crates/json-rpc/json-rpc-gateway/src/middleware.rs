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
    time::{Duration, Instant},
};

use axum::http::{Request, Response, StatusCode};
use tower::{Layer, Service, ServiceBuilder};
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;

use crate::config::GatewayConfig;
use crate::metrics::{GatewayMetrics, status_class};

/// Build the Tower HTTP middleware stack from configuration.
///
/// Layer order (outermost → innermost):
/// 1. **CORS** — permissive for dev, restrictive for prod
/// 2. **Timeout** — per-request timeout from config
#[allow(clippy::type_complexity)]
pub(crate) fn http_middleware(
    config: &GatewayConfig,
    metrics: GatewayMetrics,
) -> ServiceBuilder<
    tower::layer::util::Stack<
        TimeoutLayer,
        tower::layer::util::Stack<
            CorsLayer,
            tower::layer::util::Stack<HttpMetricsLayer, tower::layer::util::Identity>,
        >,
    >,
> {
    ServiceBuilder::new()
        .layer(HttpMetricsLayer::new(metrics, "jsonrpc"))
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
    use tower::{ServiceExt, service_fn};

    #[tokio::test]
    async fn http_middleware_records_timeout_status_class() {
        let config = GatewayConfig {
            request_timeout_ms: 1,
            ..GatewayConfig::default()
        };
        let metrics = GatewayMetrics::new("test", None).expect("metrics should register");
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
}
