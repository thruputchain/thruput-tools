//! Per-client-IP token-bucket admission for the JSON-RPC listener.
//!
//! [`IpRateLimiter`] holds one refilling token bucket per client IP
//! behind a mutex; [`RateLimitService`] is a per-connection Tower
//! service wrapped around the jsonrpsee connection service that spends
//! one token per HTTP request and answers over-limit requests with
//! HTTP 429 and a `Retry-After` header before they reach the RPC stack.
//!
//! The client identity is the accepted socket address, unless
//! `rate_limit.trusted_proxy` is set, in which case the rightmost
//! `X-Forwarded-For` entry (the one appended by the trusted fronting
//! proxy) is used and the socket address is the fallback.

use std::{
    collections::HashMap,
    future::Future,
    net::IpAddr,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use axum::http::{HeaderMap, Request, Response, StatusCode, header};
use tower::Service;

use crate::config::RateLimitConfig;
use crate::metrics::GatewayMetrics;

/// Hard cap on distinct client IPs tracked at once. Beyond this the
/// limiter fails open for new IPs rather than growing without bound.
const MAX_TRACKED_IPS: usize = 100_000;

/// How often the bucket map sheds idle (fully refilled) entries.
const PRUNE_INTERVAL: Duration = Duration::from_secs(30);

/// Outcome of admitting one request from a client IP.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RateLimitDecision {
    /// The request may proceed.
    Allowed,
    /// The client is over its limit and should retry after this delay.
    Limited {
        /// Time until the client's bucket holds a whole token again.
        retry_after: Duration,
    },
}

/// Shared token-bucket state for every client IP seen by the listener.
pub(crate) struct IpRateLimiter {
    tokens_per_second: f64,
    burst: f64,
    state: Mutex<LimiterState>,
}

struct LimiterState {
    buckets: HashMap<IpAddr, Bucket>,
    last_prune: Instant,
    capacity_warned: bool,
}

struct Bucket {
    tokens: f64,
    updated_at: Instant,
}

impl IpRateLimiter {
    /// Build the shared limiter from config, or `None` when disabled.
    pub(crate) fn from_config(config: &RateLimitConfig) -> Option<Arc<Self>> {
        config
            .enabled
            .then(|| Arc::new(Self::new(config.requests_per_second, config.burst)))
    }

    pub(crate) fn new(requests_per_second: u32, burst: u32) -> Self {
        Self {
            tokens_per_second: f64::from(requests_per_second.max(1)),
            burst: f64::from(burst.max(1)),
            state: Mutex::new(LimiterState {
                buckets: HashMap::new(),
                last_prune: Instant::now(),
                capacity_warned: false,
            }),
        }
    }

    /// Spend one token for `ip`, refilling its bucket first.
    pub(crate) fn check(&self, ip: IpAddr) -> RateLimitDecision {
        self.check_at(ip, Instant::now())
    }

    fn check_at(&self, ip: IpAddr, now: Instant) -> RateLimitDecision {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if now.duration_since(state.last_prune) >= PRUNE_INTERVAL {
            self.prune(&mut state, now);
        }

        if !state.buckets.contains_key(&ip) {
            if state.buckets.len() >= MAX_TRACKED_IPS {
                // One more chance to shed idle buckets before failing open.
                self.prune(&mut state, now);
            }
            if state.buckets.len() >= MAX_TRACKED_IPS {
                if !state.capacity_warned {
                    state.capacity_warned = true;
                    tracing::warn!(
                        tracked_ips = state.buckets.len(),
                        "rate limiter is tracking its maximum number of client IPs; \
                         admitting new IPs without limiting"
                    );
                }
                return RateLimitDecision::Allowed;
            }
            state.buckets.insert(
                ip,
                Bucket {
                    tokens: self.burst,
                    updated_at: now,
                },
            );
        }

        let bucket = state
            .buckets
            .get_mut(&ip)
            .expect("bug: bucket inserted above must be present");
        let elapsed = now.duration_since(bucket.updated_at).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.tokens_per_second).min(self.burst);
        bucket.updated_at = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            RateLimitDecision::Allowed
        } else {
            let missing = 1.0 - bucket.tokens;
            RateLimitDecision::Limited {
                retry_after: Duration::from_secs_f64(missing / self.tokens_per_second),
            }
        }
    }

    /// Drop buckets idle long enough to have fully refilled: a fresh
    /// bucket is indistinguishable from them, so nothing is lost.
    fn prune(&self, state: &mut LimiterState, now: Instant) {
        let idle_cutoff = Duration::from_secs_f64(self.burst / self.tokens_per_second)
            .max(Duration::from_secs(1));
        state
            .buckets
            .retain(|_, bucket| now.duration_since(bucket.updated_at) < idle_cutoff);
        state.last_prune = now;
        state.capacity_warned = false;
    }

    #[cfg(test)]
    fn tracked_ips(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .buckets
            .len()
    }
}

/// Resolve the client IP for one request.
///
/// Uses the rightmost `X-Forwarded-For` entry — the one appended by the
/// nearest (trusted) proxy — when `trusted_proxy` is set and the entry
/// parses; the socket address otherwise.
fn client_ip(headers: &HeaderMap, socket_ip: IpAddr, trusted_proxy: bool) -> IpAddr {
    if !trusted_proxy {
        return socket_ip;
    }
    headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .and_then(|entry| {
            let entry = entry.trim();
            entry
                .parse::<IpAddr>()
                .ok()
                .or_else(|| entry.parse::<std::net::SocketAddr>().ok().map(|a| a.ip()))
        })
        .unwrap_or(socket_ip)
}

/// Per-connection admission wrapper around the jsonrpsee connection
/// service. `limiter: None` disables limiting entirely.
#[derive(Clone)]
pub(crate) struct RateLimitService<S> {
    inner: S,
    limiter: Option<Arc<IpRateLimiter>>,
    socket_ip: IpAddr,
    trusted_proxy: bool,
    metrics: GatewayMetrics,
}

impl<S> RateLimitService<S> {
    pub(crate) fn new(
        inner: S,
        limiter: Option<Arc<IpRateLimiter>>,
        socket_ip: IpAddr,
        trusted_proxy: bool,
        metrics: GatewayMetrics,
    ) -> Self {
        Self {
            inner,
            limiter,
            socket_ip,
            trusted_proxy,
            metrics,
        }
    }
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for RateLimitService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        if let Some(limiter) = &self.limiter {
            let ip = client_ip(req.headers(), self.socket_ip, self.trusted_proxy);
            if let RateLimitDecision::Limited { retry_after } = limiter.check(ip) {
                self.metrics.observe_rate_limited_request();
                tracing::debug!(client_ip = %ip, retry_after_ms = retry_after.as_millis(),
                    "rate-limited JSON-RPC request");
                let response = too_many_requests(retry_after);
                return Box::pin(async move { Ok(response) });
            }
        }
        Box::pin(self.inner.call(req))
    }
}

fn too_many_requests<ResBody: Default>(retry_after: Duration) -> Response<ResBody> {
    let retry_after_secs = retry_after.as_secs().max(1);
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header(header::RETRY_AFTER, retry_after_secs)
        .body(ResBody::default())
        .expect("bug: static 429 response should build")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::HeaderValue;
    use tower::{ServiceExt, service_fn};

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    #[test]
    fn burst_is_admitted_then_limited_then_recovers() {
        let limiter = IpRateLimiter::new(10, 3);
        let start = Instant::now();

        for _ in 0..3 {
            assert_eq!(limiter.check_at(ip(1), start), RateLimitDecision::Allowed);
        }
        let RateLimitDecision::Limited { retry_after } = limiter.check_at(ip(1), start) else {
            panic!("fourth request within the burst window should be limited");
        };
        assert!(retry_after > Duration::ZERO && retry_after <= Duration::from_millis(100));

        // 100ms at 10 tokens/s refills one whole token.
        let later = start + Duration::from_millis(100);
        assert_eq!(limiter.check_at(ip(1), later), RateLimitDecision::Allowed);
        assert!(matches!(
            limiter.check_at(ip(1), later),
            RateLimitDecision::Limited { .. }
        ));
    }

    #[test]
    fn limits_are_tracked_per_ip() {
        let limiter = IpRateLimiter::new(1, 1);
        let now = Instant::now();

        assert_eq!(limiter.check_at(ip(1), now), RateLimitDecision::Allowed);
        assert!(matches!(
            limiter.check_at(ip(1), now),
            RateLimitDecision::Limited { .. }
        ));
        assert_eq!(limiter.check_at(ip(2), now), RateLimitDecision::Allowed);
    }

    #[test]
    fn idle_buckets_are_pruned() {
        let limiter = IpRateLimiter::new(10, 3);
        let start = Instant::now();
        for last in 1..=5 {
            limiter.check_at(ip(last), start);
        }
        assert_eq!(limiter.tracked_ips(), 5);

        // Past the prune interval every bucket has fully refilled, so the
        // next request prunes them all and re-tracks only its own IP.
        let later = start + PRUNE_INTERVAL + Duration::from_secs(1);
        limiter.check_at(ip(6), later);
        assert_eq!(limiter.tracked_ips(), 1);
    }

    #[test]
    fn disabled_config_builds_no_limiter() {
        let config = RateLimitConfig {
            enabled: false,
            ..RateLimitConfig::default()
        };
        assert!(IpRateLimiter::from_config(&config).is_none());
        assert!(IpRateLimiter::from_config(&RateLimitConfig::default()).is_some());
    }

    #[test]
    fn client_ip_ignores_forwarded_for_without_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.0.2.7, 198.51.100.9"),
        );

        assert_eq!(client_ip(&headers, ip(1), false), ip(1));
        assert_eq!(
            client_ip(&headers, ip(1), true),
            "198.51.100.9".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn client_ip_falls_back_to_socket_on_unparseable_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));

        assert_eq!(client_ip(&headers, ip(1), true), ip(1));
        assert_eq!(client_ip(&HeaderMap::new(), ip(1), true), ip(1));
    }

    async fn call_service<S>(service: &mut S, request: Request<Body>) -> Response<Body>
    where
        S: Service<Request<Body>, Response = Response<Body>>,
        S::Error: std::fmt::Debug,
    {
        service
            .ready()
            .await
            .expect("service should become ready")
            .call(request)
            .await
            .expect("service call should succeed")
    }

    #[tokio::test]
    async fn over_limit_requests_get_429_with_retry_after() {
        let limiter = Some(Arc::new(IpRateLimiter::new(1, 1)));
        let mut service = RateLimitService::new(
            service_fn(|_request: Request<Body>| async move {
                Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
            }),
            limiter,
            ip(1),
            false,
            GatewayMetrics::for_tests(),
        );

        let first = call_service(&mut service, Request::new(Body::empty())).await;
        assert_eq!(first.status(), StatusCode::OK);

        let second = call_service(&mut service, Request::new(Body::empty())).await;
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = second
            .headers()
            .get(header::RETRY_AFTER)
            .expect("429 must carry Retry-After")
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(retry_after >= 1);
    }

    #[tokio::test]
    async fn disabled_limiter_admits_everything() {
        let mut service = RateLimitService::new(
            service_fn(|_request: Request<Body>| async move {
                Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
            }),
            None,
            ip(1),
            false,
            GatewayMetrics::for_tests(),
        );

        for _ in 0..50 {
            let response = call_service(&mut service, Request::new(Body::empty())).await;
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn trusted_proxy_buckets_by_forwarded_for() {
        let limiter = Some(Arc::new(IpRateLimiter::new(1, 1)));
        let mut service = RateLimitService::new(
            service_fn(|_request: Request<Body>| async move {
                Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
            }),
            limiter,
            ip(1),
            true,
            GatewayMetrics::for_tests(),
        );

        let request_from = |forwarded: &'static str| {
            Request::builder()
                .header("x-forwarded-for", forwarded)
                .body(Body::empty())
                .unwrap()
        };

        let first = call_service(&mut service, request_from("192.0.2.7")).await;
        assert_eq!(first.status(), StatusCode::OK);
        let duplicate = call_service(&mut service, request_from("192.0.2.7")).await;
        assert_eq!(duplicate.status(), StatusCode::TOO_MANY_REQUESTS);
        // A different forwarded client on the same socket gets its own bucket.
        let other = call_service(&mut service, request_from("192.0.2.8")).await;
        assert_eq!(other.status(), StatusCode::OK);
    }
}
