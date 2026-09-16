use std::time::{Duration, Instant};

use groundhog_json_rpc_types::error::outcome_for_code;
use jsonrpsee::{
    core::middleware::{Batch, BatchEntryErr, Notification, RpcServiceT},
    core::server::MethodResponse,
    types::{Id, Request, error::ErrorCode},
};

use crate::metrics::GatewayMetrics;

// Outside the batch dispatcher: observe array size and entries rejected during parsing.
#[derive(Clone)]
pub(crate) struct RpcMetricsService<S> {
    inner: S,
    metrics: GatewayMetrics,
}

impl<S> RpcMetricsService<S> {
    pub(crate) fn new(inner: S, metrics: GatewayMetrics) -> Self {
        Self { inner, metrics }
    }
}

impl<S> RpcServiceT for RpcMetricsService<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            BatchResponse = MethodResponse,
            NotificationResponse = MethodResponse,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    type MethodResponse = MethodResponse;
    type BatchResponse = MethodResponse;
    type NotificationResponse = MethodResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.inner.call(req)
    }

    fn batch<'a>(&self, mut batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        let metrics = self.metrics.clone();
        let inner = self.inner.clone();
        async move {
            let batch_len = batch.len();
            metrics.observe_rpc_batch_size(batch_len);
            // These entries never reach the per-call middleware. BatchEntryErr
            // exposes its code only by value; restore it after observing it.
            for entry in batch.iter_mut() {
                if let Err(error) = entry {
                    let (object, id) = std::mem::replace(
                        error,
                        BatchEntryErr::new(Id::Null, ErrorCode::InvalidRequest.into()),
                    )
                    .into_parts();
                    metrics.observe_rpc_request(
                        "invalid",
                        outcome_for_code(object.code()),
                        Duration::ZERO,
                    );
                    *error = BatchEntryErr::new(id, object);
                }
            }
            let started_at = Instant::now();
            let response = inner.batch(batch).await;
            tracing::debug!(
                batch_len,
                duration_ms = started_at.elapsed().as_millis(),
                "gateway JSON-RPC batch completed"
            );
            response
        }
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.inner.notification(notification)
    }
}

// Inside the dispatcher: each call records its own lifetime and typed outcome.
#[derive(Clone)]
pub(crate) struct RpcCallMetricsService<S> {
    inner: S,
    metrics: GatewayMetrics,
}

impl<S> RpcCallMetricsService<S> {
    pub(crate) fn new(inner: S, metrics: GatewayMetrics) -> Self {
        Self { inner, metrics }
    }
}

impl<S> RpcServiceT for RpcCallMetricsService<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            BatchResponse = MethodResponse,
            NotificationResponse = MethodResponse,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    type MethodResponse = MethodResponse;
    type BatchResponse = MethodResponse;
    type NotificationResponse = MethodResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        let metrics = self.metrics.clone();
        let inner = self.inner.clone();
        async move { observe_call(metrics, inner, req).await }
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.inner.batch(batch)
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.inner.notification(notification)
    }
}

async fn observe_call<S>(metrics: GatewayMetrics, inner: S, req: Request<'_>) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse>,
{
    let method = GatewayMetrics::bound_rpc_method(req.method_name());
    let _guard = metrics.rpc_in_flight_guard(method);
    let started_at = Instant::now();
    let response = inner.call(req).await;
    let outcome = response.as_error_code().map_or("ok", outcome_for_code);
    let duration = started_at.elapsed();
    tracing::debug!(
        method,
        outcome,
        duration_ms = duration.as_millis(),
        "gateway JSON-RPC call completed",
    );
    metrics.observe_rpc_request(method, outcome, duration);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::{
        core::{
            middleware::BatchEntry,
            server::{BatchResponseBuilder, ResponsePayload},
        },
        types::{ErrorObject, Id},
    };
    use std::{
        future,
        sync::{Arc, Mutex},
    };

    use crate::{
        health::{GatewayHealthSnapshot, HealthStatus},
        metrics::GatewayMetrics,
    };

    #[derive(Clone, Default)]
    struct MockRpcService {
        gate: Option<Arc<tokio::sync::Semaphore>>,
        completed: Arc<Mutex<Vec<String>>>,
    }

    impl RpcServiceT for MockRpcService {
        type MethodResponse = MethodResponse;
        type BatchResponse = MethodResponse;
        type NotificationResponse = MethodResponse;

        fn call<'a>(
            &self,
            req: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            let this = self.clone();
            async move {
                if req.method_name() == "groundhog_chainTip"
                    && let Some(gate) = &this.gate
                {
                    gate.acquire().await.unwrap().forget();
                }
                this.completed
                    .lock()
                    .unwrap()
                    .push(req.method_name().to_owned());
                if req.method_name() == "groundhog_chainTip" {
                    MethodResponse::response(
                        req.id().into_owned(),
                        ResponsePayload::success("ok"),
                        4096,
                    )
                } else {
                    MethodResponse::error(
                        req.id().into_owned(),
                        ErrorObject::owned(-32601, "method not found", None::<()>),
                    )
                }
            }
        }

        fn batch<'a>(
            &self,
            batch: Batch<'a>,
        ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
            let service = self.clone();
            async move {
                let mut builder = BatchResponseBuilder::new_with_limit(4096);
                for entry in batch {
                    match entry {
                        Ok(BatchEntry::Call(req)) => {
                            builder
                                .append(service.call(req).await)
                                .expect("test batch response should fit");
                        }
                        Ok(BatchEntry::Notification(_)) => {}
                        Err(err) => {
                            let (err, id) = err.into_parts();
                            builder
                                .append(MethodResponse::error(id, err))
                                .expect("test batch response should fit");
                        }
                    }
                }
                MethodResponse::from_batch(builder.finish())
            }
        }

        fn notification<'a>(
            &self,
            _notification: Notification<'a>,
        ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
            future::ready(MethodResponse::notification())
        }
    }

    #[tokio::test]
    async fn rpc_metrics_service_records_call_and_batch() {
        let metrics = GatewayMetrics::for_tests();
        let service = RpcMetricsService::new(
            groundhog_json_rpc_server::batch::ConcurrentBatchService::new(
                RpcCallMetricsService::new(MockRpcService::default(), metrics.clone()),
                4096,
                groundhog_json_rpc_server::batch::RpcBatchConcurrency::default(),
            ),
            metrics.clone(),
        );

        service
            .call(Request::borrowed("groundhog_chainTip", None, Id::Number(1)))
            .await;

        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(Request::borrowed(
                "groundhog_chainTip",
                None,
                Id::Number(2),
            ))),
            Ok(BatchEntry::Call(Request::borrowed(
                "evil_custom",
                None,
                Id::Number(3),
            ))),
        ]);
        service.batch(batch).await;

        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics.gather(&snapshot).expect("metrics should encode");

        assert!(body.contains(
            "groundhog_gateway_rpc_requests_total{method=\"groundhog_chainTip\",outcome=\"ok\"} 2"
        ));
        assert!(body.contains(
            "groundhog_gateway_rpc_requests_total{method=\"unknown\",outcome=\"not_implemented\"} 1"
        ));
        assert!(body.contains("groundhog_gateway_rpc_batch_size_bucket{le=\"2\"} 1"));
        assert!(body.contains("groundhog_gateway_rpc_in_flight{method=\"groundhog_chainTip\"} 0"));
    }

    #[tokio::test]
    async fn mixed_out_of_order_batch_metrics_match_methods_and_skip_notifications() {
        use jsonrpsee::core::middleware::BatchEntryErr;
        use jsonrpsee::types::error::ErrorCode;

        let metrics = GatewayMetrics::for_tests();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let inner = MockRpcService {
            gate: Some(gate.clone()),
            ..Default::default()
        };
        let service = jsonrpsee::core::middleware::RpcServiceBuilder::new()
            .layer_fn(|inner| RpcMetricsService::new(inner, metrics.clone()))
            .layer_fn(|inner| {
                groundhog_json_rpc_server::batch::ConcurrentBatchService::new(
                    inner,
                    4096,
                    groundhog_json_rpc_server::batch::RpcBatchConcurrency::default(),
                )
            })
            .layer_fn(|inner| RpcCallMetricsService::new(inner, metrics.clone()))
            .service(inner.clone());
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(Request::borrowed(
                "groundhog_chainTip",
                None,
                Id::Null,
            ))),
            Ok(BatchEntry::Notification(
                serde_json::from_str(r#"{"jsonrpc":"2.0","method":"groundhog_chainTip"}"#).unwrap(),
            )),
            Err(BatchEntryErr::new(
                Id::Str("bad".into()),
                ErrorCode::InvalidRequest.into(),
            )),
            Ok(BatchEntry::Call(Request::borrowed(
                "unknown",
                None,
                Id::Null,
            ))),
        ]);
        let mut response = Box::pin(service.batch(batch));
        future::poll_fn(|cx| {
            assert!(response.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(*inner.completed.lock().unwrap(), vec!["unknown"]);
        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let before = metrics.gather(&snapshot).unwrap();
        for (method, active) in [("groundhog_chainTip", 1), ("unknown", 0)] {
            assert!(before.contains(&format!(
                "groundhog_gateway_rpc_in_flight{{method=\"{method}\"}} {active}"
            )));
        }
        assert!(before.contains(
            "groundhog_gateway_rpc_requests_total{method=\"unknown\",outcome=\"not_implemented\"} 1"
        ));
        gate.add_permits(1);
        let response = response.await;
        let output: serde_json::Value = serde_json::from_str(response.as_json().get()).unwrap();
        assert_eq!(output[0]["result"], "ok");
        assert_eq!(output[1]["id"], "bad");
        assert_eq!(output[2]["error"]["code"], -32601);
        assert_eq!(
            *inner.completed.lock().unwrap(),
            vec!["unknown", "groundhog_chainTip"]
        );
        let body = metrics.gather(&snapshot).unwrap();
        for (method, outcome) in [
            ("groundhog_chainTip", "ok"),
            ("invalid", "invalid_params"),
            ("unknown", "not_implemented"),
        ] {
            assert!(body.contains(&format!(
                "groundhog_gateway_rpc_requests_total{{method=\"{method}\",outcome=\"{outcome}\"}} 1"
            )), "{body}");
            if method != "invalid" {
                assert!(body.contains(&format!(
                    "groundhog_gateway_rpc_in_flight{{method=\"{method}\"}} 0"
                )));
            }
        }
        assert!(body.contains("groundhog_gateway_rpc_batch_size_sum 4"));
        assert!(body.contains("groundhog_gateway_rpc_batch_size_count 1"));
        assert_eq!(
            body.lines()
                .filter(|line| line.starts_with("groundhog_gateway_rpc_requests_total{"))
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn concurrent_batch_metrics_count_each_entry_once() {
        let metrics = GatewayMetrics::for_tests();
        let service = jsonrpsee::core::middleware::RpcServiceBuilder::new()
            .layer_fn(|inner| RpcMetricsService::new(inner, metrics.clone()))
            .layer_fn(|inner| {
                groundhog_json_rpc_server::batch::ConcurrentBatchService::new(
                    inner,
                    4096,
                    groundhog_json_rpc_server::batch::RpcBatchConcurrency::default(),
                )
            })
            .layer_fn(|inner| RpcCallMetricsService::new(inner, metrics.clone()))
            .service(MockRpcService::default());
        let mut batch = Batch::new();
        for id in 0..3 {
            batch.push(Request::borrowed(
                "groundhog_getTransactionReceipt",
                None,
                Id::Number(id),
            ));
        }
        service.batch(batch).await;

        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics
            .gather(&snapshot)
            .expect("bug: metrics should encode");
        assert!(body.contains(
            "groundhog_gateway_rpc_requests_total{method=\"groundhog_getTransactionReceipt\",outcome=\"not_implemented\"} 3"
        ));
        assert!(body.contains("groundhog_gateway_rpc_batch_size_count 1"));
        assert!(body.contains(
            "groundhog_gateway_rpc_in_flight{method=\"groundhog_getTransactionReceipt\"} 0"
        ));
    }

    #[tokio::test]
    async fn gateway_config_controls_array_dispatch_window() {
        for window in [1, 2, 128] {
            let mut config = toml::from_str::<crate::config::GatewayConfig>(&format!(
                "rpc_batch_concurrency = {window}"
            ))
            .unwrap();
            let cli = <crate::Cli as clap::Parser>::parse_from(["hog-gateway"]);
            crate::apply_cli_overrides(&mut config, cli);
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            let inner = MockRpcService {
                gate: Some(gate.clone()),
                ..Default::default()
            };
            let service = config.batch_service(inner.clone(), 4096);
            let mut batch = Batch::new();
            batch.push(Request::borrowed("groundhog_chainTip", None, Id::Number(1)));
            batch.push(Request::borrowed("unknown", None, Id::Number(2)));
            let mut response = Box::pin(service.batch(batch));
            future::poll_fn(|cx| {
                assert!(response.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            assert_eq!(
                inner.completed.lock().unwrap().len(),
                usize::from(window > 1)
            );
            gate.add_permits(1);
            response.await;
            assert_eq!(inner.completed.lock().unwrap().len(), 2);
        }
    }
}
