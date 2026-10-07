use std::time::Instant;

use jsonrpsee::{
    core::middleware::{Batch, BatchEntry, Notification, RpcServiceT},
    core::server::MethodResponse,
    types::Request,
};
use thruput_json_rpc_types::error::outcome_for_code;
use tracing::Instrument;

use crate::metrics::GatewayMetrics;

/// Trace each RPC call and notification with its method and error outcome.
#[derive(Clone)]
pub(crate) struct RpcTracingService<S>(S);

impl<S> RpcTracingService<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self(inner)
    }
}

impl<S> RpcServiceT for RpcTracingService<S>
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
        let inner = self.0.clone();
        async move {
            let span =
                crate::telemetry::rpc_span(GatewayMetrics::bound_rpc_method(req.method_name()));
            let response = inner.call(req).instrument(span.clone()).await;
            if let Some(code) = response.as_error_code() {
                span.record("rpc.jsonrpc.error_code", code);
            }
            response
        }
    }
    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.0.batch(batch)
    }
    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = MethodResponse> + Send + 'a {
        let span = crate::telemetry::rpc_span(GatewayMetrics::bound_rpc_method(
            notification.method_name(),
        ));
        self.0.notification(notification).instrument(span)
    }
}

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

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let metrics = self.metrics.clone();
        let inner = self.inner.clone();
        async move { observe_call(metrics, inner, req).await }
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let metrics = self.metrics.clone();
        let inner = self.inner.clone();
        async move {
            let batch_len = batch.len();
            metrics.observe_rpc_batch_size(batch_len);
            let started_at = Instant::now();
            let mut call_methods = Vec::new();
            for entry in batch.iter() {
                match entry {
                    Ok(BatchEntry::Call(req)) => {
                        call_methods
                            .push(Some(GatewayMetrics::bound_rpc_method(req.method_name())));
                    }
                    Ok(BatchEntry::Notification(_)) => {}
                    Err(_) => call_methods.push(Some("invalid")),
                }
            }
            let guards = call_methods
                .iter()
                .filter_map(|method| method.map(|method| metrics.rpc_in_flight_guard(method)))
                .collect::<Vec<_>>();
            let response = inner.batch(batch).await;
            drop(guards);

            let duration = started_at.elapsed();
            let outcomes = batch_outcomes(&response, call_methods.len());
            tracing::debug!(
                batch_len,
                calls = call_methods.len(),
                duration_ms = duration.as_millis(),
                "gateway JSON-RPC batch completed",
            );
            for (method, outcome) in call_methods.into_iter().zip(outcomes) {
                if let Some(method) = method {
                    tracing::debug!(
                        method,
                        outcome,
                        duration_ms = duration.as_millis(),
                        "gateway JSON-RPC batch call observed",
                    );
                    metrics.observe_rpc_request(method, outcome, duration);
                }
            }
            response
        }
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
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

fn batch_outcomes(response: &MethodResponse, expected_len: usize) -> Vec<&'static str> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(response.as_json().get()) else {
        return vec!["internal_error"; expected_len];
    };
    let Some(items) = value.as_array() else {
        let outcome = response
            .as_error_code()
            .map_or("internal_error", outcome_for_code);
        return vec![outcome; expected_len];
    };
    items
        .iter()
        .map(|item| {
            item.get("error")
                .and_then(|error| error.get("code"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|code| i32::try_from(code).ok())
                .map_or("ok", outcome_for_code)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::{
        core::server::{BatchResponseBuilder, ResponsePayload},
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

    #[derive(Clone, Debug, Default)]
    struct CaptureExporter(Arc<Mutex<Vec<opentelemetry_sdk::trace::SpanData>>>);

    impl opentelemetry_sdk::trace::SpanExporter for CaptureExporter {
        async fn export(
            &self,
            spans: Vec<opentelemetry_sdk::trace::SpanData>,
        ) -> opentelemetry_sdk::error::OTelSdkResult {
            self.0.lock().unwrap().extend(spans);
            Ok(())
        }
    }

    #[tokio::test]
    async fn rpc_spans_bound_names_record_errors_and_preserve_remote_parent() {
        use opentelemetry::trace::{SpanKind, TracerProvider};
        use tracing::instrument::WithSubscriber;
        use tracing_subscriber::layer::SubscriberExt;
        let exporter = CaptureExporter::default();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
                .parse()
                .unwrap(),
        );
        let trace = crate::telemetry::HttpTrace {
            parent: crate::telemetry::extract(&headers),
            ..Default::default()
        };
        crate::telemetry::HTTP_TRACE
            .scope(trace.clone(), async {
                let service = RpcTracingService::new(MockRpcService::default());
                service
                    .call(Request::borrowed("groundhog_chainTip", None, Id::Number(1)))
                    .await;
                service
                    .call(Request::borrowed("untrusted_method", None, Id::Number(2)))
                    .await;
                for span in trace.spans.lock().unwrap().drain(..) {
                    span.record("http.response.status_code", 200);
                }
            })
            .with_subscriber(subscriber)
            .await;
        provider.force_flush().unwrap();
        let spans = exporter.0.lock().unwrap();
        let method = spans
            .iter()
            .find(|span| span.name == "groundhog_chainTip")
            .unwrap();
        assert_eq!(method.span_kind, SpanKind::Server);
        assert!(method.parent_span_is_remote);
        assert_eq!(method.parent_span_id.to_string(), "0123456789abcdef");
        for (key, value) in [
            ("rpc.system", "jsonrpc"),
            ("rpc.method", "groundhog_chainTip"),
            ("http.response.status_code", "200"),
        ] {
            assert!(
                method
                    .attributes
                    .iter()
                    .any(|attribute| attribute.key.as_str() == key
                        && attribute.value.to_string() == value)
            );
        }
        let unknown = spans.iter().find(|span| span.name == "unknown").unwrap();
        assert!(
            unknown
                .attributes
                .iter()
                .any(
                    |attribute| attribute.key.as_str() == "rpc.jsonrpc.error_code"
                        && attribute.value.to_string() == "-32601"
                )
        );
    }

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

    #[test]
    fn batch_outcomes_extracts_response_codes() {
        let ok = MethodResponse::response(
            Id::Number(1),
            jsonrpsee::core::server::ResponsePayload::success("ok"),
            1024,
        );
        let err = MethodResponse::error(
            Id::Number(2),
            ErrorObject::owned(
                thruput_json_rpc_types::error::codes::BACKEND_TIMEOUT,
                "backend timeout",
                None::<()>,
            ),
        );
        let mut builder = BatchResponseBuilder::new_with_limit(4096);
        builder.append(ok).expect("ok response should fit");
        builder.append(err).expect("error response should fit");
        let response = MethodResponse::from_batch(builder.finish());

        assert_eq!(batch_outcomes(&response, 2), vec!["ok", "backend_timeout"]);
    }

    #[tokio::test]
    async fn rpc_metrics_service_records_call_and_batch() {
        let metrics = GatewayMetrics::for_tests();
        let service = RpcMetricsService::new(MockRpcService::default(), metrics.clone());

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
                thruput_json_rpc_server::batch::ConcurrentBatchService::new(
                    inner,
                    4096,
                    thruput_json_rpc_server::batch::RpcBatchConcurrency::default(),
                )
            })
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
        for method in ["groundhog_chainTip", "invalid", "unknown"] {
            assert!(before.contains(&format!(
                "groundhog_gateway_rpc_in_flight{{method=\"{method}\"}} 1"
            )));
        }
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
            assert!(body.contains(&format!(
                "groundhog_gateway_rpc_in_flight{{method=\"{method}\"}} 0"
            )));
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
                thruput_json_rpc_server::batch::ConcurrentBatchService::new(
                    inner,
                    4096,
                    thruput_json_rpc_server::batch::RpcBatchConcurrency::default(),
                )
            })
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
