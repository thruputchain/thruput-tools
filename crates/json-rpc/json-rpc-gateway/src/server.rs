//! Manual TCP serving records the peer and assigns one RPC connection ID per socket.
use std::{io, time::Duration};

use jsonrpsee::{
    RpcModule,
    server::{Server, ServerConfig, ServerHandle, serve_with_graceful_shutdown, stop_channel},
};
use tower::Service;

use crate::{
    admission::{Admission, RpcAdmissionService},
    config::GatewayConfig,
    metrics::{GatewayMetrics, admission::GaugeGuard},
    middleware::{self, PeerAddr},
    rpc_middleware,
};

pub(crate) fn start(
    listener: std::net::TcpListener,
    config: std::sync::Arc<GatewayConfig>,
    rpc: RpcModule<()>,
    metrics: GatewayMetrics,
    admission: std::sync::Arc<Admission>,
) -> io::Result<ServerHandle> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let max_response_body_size = jsonrpsee::core::TEN_MB_SIZE_BYTES;
    let server_config = ServerConfig::builder()
        .max_connections(config.max_connections)
        .max_response_body_size(max_response_body_size)
        .set_batch_request_config(jsonrpsee::server::BatchRequestConfig::Limit(
            thruput_json_rpc_server::batch::MAX_RPC_BATCH_REQUESTS,
        ))
        .build();
    let rpc_metrics = metrics.clone();
    let http_middleware = middleware::http_middleware(&config, metrics.clone());
    let batch_config = config;
    let rpc_middleware = jsonrpsee::core::middleware::RpcServiceBuilder::new()
        .layer_fn(move |inner| rpc_middleware::RpcMetricsService::new(inner, rpc_metrics.clone()))
        .layer_fn(move |inner| batch_config.batch_service(inner, max_response_body_size))
        .layer_fn(move |inner| RpcAdmissionService::new(inner, admission.clone()))
        .layer_fn(rpc_middleware::RpcTracingService::new);
    let builder = Server::builder()
        .set_config(server_config)
        .set_http_middleware(http_middleware)
        .set_rpc_middleware(rpc_middleware)
        .to_service_builder();
    let (stop, handle) = stop_channel();
    tokio::spawn(async move {
        loop {
            let (socket, peer) = tokio::select! {
                biased;
                () = stop.clone().shutdown() => break,
                result = listener.accept() => match result {
                    Ok(connection) => connection,
                    Err(error) => {
                        tracing::warn!(%error, "gateway TCP accept failed");
                        tokio::select! {
                            () = stop.clone().shutdown() => break,
                            () = tokio::time::sleep(Duration::from_millis(100)) => {},
                        }
                        continue;
                    }
                },
            };
            // Build once per socket, never once per HTTP request: the builder
            // assigns ConnectionId here, shared by HTTP/2 streams and WS calls.
            let service = builder.clone().build(rpc.clone(), stop.clone());
            let connection =
                std::sync::Arc::new(GaugeGuard::new(&metrics.admission().connections_open));
            let http_connection = connection.clone();
            let service = tower::service_fn(move |mut request: axum::http::Request<_>| {
                request.extensions_mut().insert(PeerAddr(peer));
                let mut service = service.clone();
                let connection = http_connection.clone();
                let session = jsonrpsee::server::ws::is_upgrade_request(&request)
                    .then(|| service.on_session_closed());
                let future: std::pin::Pin<
                    Box<
                        dyn Future<Output = Result<jsonrpsee::server::HttpResponse, io::Error>>
                            + Send,
                    >,
                > = Box::pin(async move {
                    if let Some(session) = session {
                        // The HTTP serving future finishes at upgrade. Keep the
                        // socket gauge alive until jsonrpsee closes the WS session.
                        tokio::spawn(async move {
                            let _connection = connection;
                            session.await;
                        });
                    }
                    service.call(request).await.map_err(io::Error::other)
                });
                future
            });
            let shutdown = stop.clone();
            tokio::spawn(async move {
                let _connection = connection;
                if let Err(error) =
                    serve_with_graceful_shutdown(socket, service, shutdown.shutdown()).await
                {
                    tracing::debug!(%error, %peer, "gateway TCP connection ended");
                }
            });
        }
    });
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        health::{GatewayHealthSnapshot, HealthStatus},
        middleware::ClientKey,
    };
    use axum::http::Extensions;
    use jsonrpsee::core::server::ConnectionId;
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::Semaphore,
    };

    struct TestServer {
        address: SocketAddr,
        handle: ServerHandle,
        admission: Arc<Admission>,
        metrics: GatewayMetrics,
    }

    impl TestServer {
        async fn new(settings: &str, module: RpcModule<()>) -> Self {
            let config: GatewayConfig = toml::from_str(settings).unwrap();
            let listener = crate::listener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let metrics = GatewayMetrics::for_tests();
            let admission = Admission::new(
                config.client_admission.clone(),
                config.view_batch.max_calls,
                metrics.clone(),
            );
            let handle = start(
                listener,
                Arc::new(config),
                module,
                metrics.clone(),
                admission.clone(),
            )
            .unwrap();
            Self {
                address,
                handle,
                admission,
                metrics,
            }
        }

        async fn stop(self) {
            self.handle.stop().unwrap();
            tokio::time::timeout(Duration::from_secs(5), self.handle.stopped())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.metrics.admission().connections_open.get() > 0.0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }

        fn metrics(&self) -> String {
            self.metrics
                .gather(&GatewayHealthSnapshot::for_test(
                    HealthStatus::Starting,
                    None,
                    &[],
                ))
                .unwrap()
        }
    }

    // Minimal real RFC6455 client, since this crate does not enable jsonrpsee's
    // ws-client feature. Client text frames are masked as required by the wire.
    struct Ws(TcpStream);

    impl Ws {
        async fn connect(address: SocketAddr) -> Self {
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket.write_all(format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nX-Forwarded-For: 192.0.2.42\r\n\r\n").as_bytes()).await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
            }
            assert!(
                String::from_utf8(headers)
                    .unwrap()
                    .starts_with("HTTP/1.1 101")
            );
            Self(socket)
        }

        async fn send(&mut self, value: serde_json::Value) {
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(bytes.len() < 126);
            let mut frame = vec![0x81, 0x80 | u8::try_from(bytes.len()).unwrap(), 1, 2, 3, 4];
            frame.extend(
                bytes
                    .iter()
                    .enumerate()
                    .map(|(i, byte)| byte ^ [1, 2, 3, 4][i % 4]),
            );
            self.0.write_all(&frame).await.unwrap();
        }

        async fn receive(&mut self) -> serde_json::Value {
            tokio::time::timeout(Duration::from_secs(5), async {
                assert_eq!(self.0.read_u8().await.unwrap() & 0x0f, 1);
                let size = self.0.read_u8().await.unwrap();
                assert_eq!(size & 0x80, 0);
                let size = match size & 0x7f {
                    126 => usize::from(self.0.read_u16().await.unwrap()),
                    127 => usize::try_from(self.0.read_u64().await.unwrap()).unwrap(),
                    size => usize::from(size),
                };
                let mut body = vec![0; size];
                self.0.read_exact(&mut body).await.unwrap();
                serde_json::from_slice(&body).unwrap()
            })
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn http_and_ws_keep_peer_client_and_socket_identity_and_shutdown() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let capture = captured.clone();
        let mut module = RpcModule::new(());
        module
            .register_method("identity", move |_, (), extensions: &Extensions| {
                capture.lock().unwrap().push((
                    *extensions.get::<PeerAddr>().unwrap(),
                    *extensions.get::<ClientKey>().unwrap(),
                    *extensions.get::<ConnectionId>().unwrap(),
                ));
                true
            })
            .unwrap();
        let server = TestServer::new("", module).await;
        let client = reqwest::Client::new();
        for _ in 0..2 {
            let result: serde_json::Value = client
                .post(format!("http://{}", server.address))
                .header("x-forwarded-for", "192.0.2.42")
                .json(&serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"identity"}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(result["result"], true);
        }
        let mut ws = Ws::connect(server.address).await;
        for id in 3..5 {
            ws.send(serde_json::json!({"jsonrpc":"2.0", "id":id, "method":"identity"}))
                .await;
            assert_eq!(ws.receive().await["result"], true);
        }
        {
            let captured = captured.lock().unwrap();
            assert_eq!(captured.len(), 4);
            for (peer, key, _) in captured.iter() {
                assert!(peer.0.ip().is_loopback());
                assert_eq!(
                    *key,
                    ClientKey::from("192.0.2.42".parse::<std::net::IpAddr>().unwrap())
                );
            }
            assert_eq!(captured[0].2, captured[1].2);
            assert_eq!(captured[2].2, captured[3].2);
            assert_ne!(captured[0].2, captured[2].2);
        }
        assert!(
            server
                .metrics()
                .contains("groundhog_gateway_connections_open 2")
        );
        // Leave both keep-alive HTTP and WebSocket sockets open at shutdown.
        server.stop().await;
    }

    #[tokio::test]
    async fn websocket_connection_cap_rejects_each_excess_message_then_cleans_up() {
        let gate = Arc::new(Semaphore::new(0));
        let entered = Arc::new(Semaphore::new(0));
        let mut module = RpcModule::new(());
        let handler_gate = gate.clone();
        let handler_entered = entered.clone();
        module
            .register_async_method("getChainTip", move |_, _, extensions| {
                assert!(extensions.get::<ClientKey>().is_some());
                assert!(extensions.get::<ConnectionId>().is_some());
                let gate = handler_gate.clone();
                handler_entered.add_permits(1);
                async move {
                    gate.acquire().await.unwrap().forget();
                    true
                }
            })
            .unwrap();
        let server = TestServer::new(
            "[client_admission]\nmax_in_flight_per_connection = 2",
            module,
        )
        .await;
        let mut ws = Ws::connect(server.address).await;
        for id in 0..2 {
            ws.send(serde_json::json!({"jsonrpc":"2.0", "id":id, "method":"getChainTip"}))
                .await;
        }
        tokio::time::timeout(Duration::from_secs(5), entered.acquire_many(2))
            .await
            .unwrap()
            .unwrap()
            .forget();
        ws.send(serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"getChainTip"}))
            .await;
        let rejected = ws.receive().await;
        assert_eq!(rejected["id"], 2);
        assert_eq!(rejected["error"]["code"], -32053);
        assert_eq!(
            rejected["error"]["data"],
            serde_json::json!({"kind":"overloaded"})
        );
        assert!(
            server
                .metrics()
                .contains("groundhog_gateway_connections_open 1")
        );
        gate.add_permits(2);
        assert_eq!(ws.receive().await["result"], true);
        assert_eq!(ws.receive().await["result"], true);
        assert_eq!(server.admission.connection_entries(), 0);
        assert!(
            server
                .metrics()
                .contains("groundhog_gateway_admission_in_flight 0")
        );
        server.stop().await;
    }

    #[tokio::test]
    async fn http_array_entries_exhaust_one_client_budget_individually() {
        let mut module = RpcModule::new(());
        module
            .register_method("getChainTip", |_, (), _| true)
            .unwrap();
        let server = TestServer::new(
            "[client_admission]\nclient_rate_per_sec = 1\nclient_burst = 2",
            module,
        )
        .await;
        let client = reqwest::Client::new();
        let batch: Vec<_> = (0..4)
            .map(|id| serde_json::json!({"jsonrpc":"2.0", "id":id, "method":"getChainTip"}))
            .collect();
        let response: serde_json::Value = client
            .post(format!("http://{}", server.address))
            .header("x-forwarded-for", "192.0.2.1")
            .json(&batch)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        for i in 0..2 {
            assert_eq!(response[i]["result"], true);
        }
        for i in 2..4 {
            assert_eq!(
                response[i]["error"]["code"],
                thruput_json_rpc_types::error::codes::RATE_LIMITED
            );
            assert_eq!(response[i]["error"]["data"]["kind"], "rateLimited");
            let delay = response[i]["error"]["data"]["retryAfterMs"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            assert!((1..=1000).contains(&delay));
            assert!(
                matches!(thruput_json_rpc_types::error::native::DecodedError::decode("getChainTip", response[i]["error"].clone()), thruput_json_rpc_types::error::native::DecodedError::Known { data, .. } if matches!(*data, thruput_json_rpc_types::error::native::ErrorData::RateLimited { retry_after_ms: Some(ms) } if ms == delay))
            );
        }
        let other: serde_json::Value = client
            .post(format!("http://{}", server.address))
            .header("x-forwarded-for", "192.0.2.2")
            .json(&batch)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(other[0]["result"], true);
        assert_eq!(other[1]["result"], true);
        server.stop().await;
    }

    #[tokio::test]
    async fn manual_server_preserves_connection_batch_body_and_timeout_limits() {
        let mut module = RpcModule::new(());
        module
            .register_method("getChainTip", |_, (), _| true)
            .unwrap();
        module
            .register_async_method("slow", |_, _, _| std::future::pending::<bool>())
            .unwrap();
        let server = TestServer::new("max_connections = 1\nrequest_timeout_ms = 100", module).await;
        let ws = Ws::connect(server.address).await;
        let client = reqwest::Client::new();
        let url = format!("http://{}", server.address);
        let call = serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"getChainTip"});
        let refused = client.post(&url).json(&call).send().await.unwrap();
        assert_eq!(refused.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        drop(ws);
        tokio::time::timeout(Duration::from_secs(5), async {
            while server.metrics.admission().connections_open.get() > 1.0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let batch = vec![
            call;
            usize::try_from(thruput_json_rpc_server::batch::MAX_RPC_BATCH_REQUESTS)
                .unwrap()
                + 1
        ];
        let rejected: serde_json::Value = client
            .post(&url)
            .json(&batch)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(rejected["error"]["code"], -32010);
        let oversized = client
            .post(&url)
            .header("content-type", "application/json")
            .body(" ".repeat(usize::try_from(jsonrpsee::core::TEN_MB_SIZE_BYTES).unwrap() + 1))
            .send()
            .await
            .unwrap();
        assert_eq!(oversized.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        let timed_out = client
            .post(&url)
            .json(&serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"slow"}))
            .send()
            .await
            .unwrap();
        assert_eq!(timed_out.status(), reqwest::StatusCode::REQUEST_TIMEOUT);
        assert_eq!(server.admission.connection_entries(), 0);
        assert!(
            server
                .metrics()
                .contains("groundhog_gateway_admission_in_flight 0")
        );
        server.stop().await;
    }
}
