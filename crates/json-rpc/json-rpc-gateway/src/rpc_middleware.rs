use std::time::Instant;

use groundhog_json_rpc_types::error::outcome_for_code;
use jsonrpsee::{
    core::middleware::{Batch, BatchEntry, Notification, RpcServiceT},
    core::server::MethodResponse,
    types::Request,
};

use crate::metrics::GatewayMetrics;

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
    use std::future;

    use crate::{
        health::{GatewayHealthSnapshot, HealthStatus},
        metrics::GatewayMetrics,
    };

    #[derive(Clone)]
    struct MockRpcService;

    impl RpcServiceT for MockRpcService {
        type MethodResponse = MethodResponse;
        type BatchResponse = MethodResponse;
        type NotificationResponse = MethodResponse;

        fn call<'a>(
            &self,
            req: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            future::ready(if req.method_name() == "groundhog_chainTip" {
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
            })
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
                groundhog_json_rpc_types::error::codes::BACKEND_TIMEOUT,
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
        let service = RpcMetricsService::new(MockRpcService, metrics.clone());

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
}
