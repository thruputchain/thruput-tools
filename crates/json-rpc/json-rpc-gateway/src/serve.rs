//! JSON-RPC listener: TCP accept loop with per-connection admission.
//!
//! The gateway runs its own accept loop instead of jsonrpsee's built-in
//! `Server::start` so each connection's remote socket address is
//! available to the per-IP rate limiter. Every accepted connection gets
//! the jsonrpsee tower service (HTTP middleware, RPC middleware, method
//! dispatch, connection limit) wrapped in a [`RateLimitService`] and is
//! served with graceful shutdown, matching `Server::start` semantics:
//! `handle.stop()` stops accepting, `handle.stopped()` resolves once
//! the accept loop and every in-flight connection have finished.

use std::net::SocketAddr;

use jsonrpsee::server::{
    BatchRequestConfig, Methods, ServerConfig, ServerHandle, serve_with_graceful_shutdown,
    stop_channel,
};

use crate::config::GatewayConfig;
use crate::metrics::GatewayMetrics;
use crate::middleware::{self, InvalidCorsOrigin};
use crate::rate_limit::{IpRateLimiter, RateLimitService};
use crate::rpc_middleware;

/// Failure to start the JSON-RPC listener.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StartRpcServerError {
    /// The listen address could not be bound.
    #[error("failed to bind JSON-RPC listener: {0}")]
    Bind(#[from] std::io::Error),

    /// The configured CORS origins are invalid.
    #[error(transparent)]
    Cors(#[from] InvalidCorsOrigin),
}

/// Handle to the running JSON-RPC listener.
pub(crate) struct RpcServer {
    /// Stop/stopped handle with jsonrpsee `Server::start` semantics.
    pub handle: ServerHandle,
    /// Bound listen address (with the OS-assigned port when the
    /// configured port was 0).
    pub local_addr: SocketAddr,
}

/// Bind the configured listen address and serve `methods` on it.
pub(crate) async fn start_rpc_server(
    config: &GatewayConfig,
    metrics: GatewayMetrics,
    methods: impl Into<Methods>,
) -> Result<RpcServer, StartRpcServerError> {
    let methods: Methods = methods.into();
    let listener = tokio::net::TcpListener::bind(&config.listen_address).await?;
    let local_addr = listener.local_addr()?;

    let batch_config = if config.max_batch_requests == 0 {
        BatchRequestConfig::Disabled
    } else {
        BatchRequestConfig::Limit(config.max_batch_requests)
    };
    let server_cfg = ServerConfig::builder()
        .max_connections(config.max_connections)
        .max_request_body_size(config.max_request_body_bytes)
        .max_response_body_size(config.max_response_body_bytes)
        .set_batch_request_config(batch_config)
        .build();
    let http_mw = middleware::http_middleware(config, metrics.clone())?;
    let rpc_metrics = metrics.clone();
    let rpc_mw = jsonrpsee::core::middleware::RpcServiceBuilder::new()
        .layer_fn(move |inner| rpc_middleware::RpcMetricsService::new(inner, rpc_metrics.clone()));
    let svc_builder = jsonrpsee::server::Server::builder()
        .set_config(server_cfg)
        .set_http_middleware(http_mw)
        .set_rpc_middleware(rpc_mw)
        .to_service_builder();

    let limiter = IpRateLimiter::from_config(&config.rate_limit);
    let trusted_proxy = config.rate_limit.trusted_proxy;
    let (stop_handle, server_handle) = stop_channel();

    tokio::spawn(async move {
        let accept_stop = stop_handle.clone();
        loop {
            let (socket, remote_addr) = tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok(connection) => connection,
                    Err(error) => {
                        tracing::warn!(%error, "failed to accept JSON-RPC connection");
                        continue;
                    }
                },
                () = accept_stop.clone().shutdown() => break,
            };
            let rpc_service = svc_builder
                .clone()
                .build(methods.clone(), stop_handle.clone());
            let service = RateLimitService::new(
                rpc_service,
                limiter.clone(),
                remote_addr.ip(),
                trusted_proxy,
                metrics.clone(),
            );
            tokio::spawn(serve_with_graceful_shutdown(
                socket,
                service,
                stop_handle.clone().shutdown(),
            ));
        }
    });

    Ok(RpcServer {
        handle: server_handle,
        local_addr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RateLimitConfig;
    use jsonrpsee::RpcModule;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_methods() -> Methods {
        let mut module = RpcModule::new(());
        module
            .register_method("test_echo", |_params, (), _extensions| "ok")
            .expect("bug: test method should register");
        module.into()
    }

    fn test_config() -> GatewayConfig {
        GatewayConfig {
            listen_address: "127.0.0.1:0".into(),
            ..GatewayConfig::default()
        }
    }

    async fn start(config: GatewayConfig) -> RpcServer {
        start_rpc_server(&config, GatewayMetrics::for_tests(), test_methods())
            .await
            .expect("test server should start")
    }

    /// One HTTP/1.1 request on a fresh connection; returns the raw response.
    async fn raw_http_post(addr: SocketAddr, body: &str, extra_headers: &str) -> String {
        let request = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Connection: close\r\n{extra_headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to test server");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        String::from_utf8_lossy(&response).into_owned()
    }

    const CALL: &str = r#"{"jsonrpc":"2.0","id":1,"method":"test_echo"}"#;

    #[tokio::test]
    async fn rate_limit_trips_and_recovers() {
        let server = start(GatewayConfig {
            rate_limit: RateLimitConfig {
                enabled: true,
                requests_per_second: 2,
                burst: 2,
                trusted_proxy: false,
            },
            ..test_config()
        })
        .await;

        for _ in 0..2 {
            let response = raw_http_post(server.local_addr, CALL, "").await;
            assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        }
        let limited = raw_http_post(server.local_addr, CALL, "").await;
        assert!(limited.starts_with("HTTP/1.1 429"), "got: {limited}");
        assert!(
            limited.to_ascii_lowercase().contains("retry-after:"),
            "429 must carry Retry-After: {limited}"
        );

        // At 2 tokens/s one whole token is back within a second.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let recovered = raw_http_post(server.local_addr, CALL, "").await;
        assert!(recovered.starts_with("HTTP/1.1 200"), "got: {recovered}");

        server.handle.stop().expect("server should stop");
        server.handle.stopped().await;
    }

    #[tokio::test]
    async fn oversized_request_body_is_rejected() {
        let server = start(GatewayConfig {
            max_request_body_bytes: 256,
            rate_limit: RateLimitConfig {
                enabled: false,
                ..RateLimitConfig::default()
            },
            ..test_config()
        })
        .await;

        let oversized = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"test_echo","params":["{}"]}}"#,
            "aa".repeat(512)
        );
        let response = raw_http_post(server.local_addr, &oversized, "").await;
        assert!(response.starts_with("HTTP/1.1 413"), "got: {response}");

        let within = raw_http_post(server.local_addr, CALL, "").await;
        assert!(within.starts_with("HTTP/1.1 200"), "got: {within}");

        server.handle.stop().expect("server should stop");
        server.handle.stopped().await;
    }

    #[tokio::test]
    async fn batch_over_limit_is_rejected() {
        let server = start(GatewayConfig {
            max_batch_requests: 2,
            rate_limit: RateLimitConfig {
                enabled: false,
                ..RateLimitConfig::default()
            },
            ..test_config()
        })
        .await;

        let batch_of_two = format!("[{CALL},{CALL}]");
        let accepted = raw_http_post(server.local_addr, &batch_of_two, "").await;
        assert!(accepted.contains(r#""result":"ok""#), "got: {accepted}");

        let batch_of_three = format!("[{CALL},{CALL},{CALL}]");
        let rejected = raw_http_post(server.local_addr, &batch_of_three, "").await;
        assert!(
            rejected.contains("The batch request was too large"),
            "got: {rejected}"
        );

        server.handle.stop().expect("server should stop");
        server.handle.stopped().await;
    }

    #[tokio::test]
    async fn cors_config_is_wired_into_the_listener() {
        let server = start(GatewayConfig {
            cors_allowed_origins: vec!["http://localhost:3000".into()],
            ..test_config()
        })
        .await;

        let allowed =
            raw_http_post(server.local_addr, CALL, "Origin: http://localhost:3000\r\n").await;
        assert!(
            allowed
                .to_ascii_lowercase()
                .contains("access-control-allow-origin: http://localhost:3000"),
            "got: {allowed}"
        );

        let denied =
            raw_http_post(server.local_addr, CALL, "Origin: http://evil.example\r\n").await;
        assert!(
            !denied
                .to_ascii_lowercase()
                .contains("access-control-allow-origin"),
            "got: {denied}"
        );

        server.handle.stop().expect("server should stop");
        server.handle.stopped().await;
    }
}
