use opentelemetry_sdk::trace::SdkTracerProvider;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

pub(crate) use thruput_telemetry::extract;

const SERVICE: thruput_telemetry::ServiceTelemetry = thruput_telemetry::ServiceTelemetry {
    crate_target: env!("CARGO_CRATE_NAME"),
    // Batch and tip spans link every request served, including cohorts over 128.
    max_links_per_span: Some(u32::MAX),
    warn: |message, error| tracing::warn!(error = error.map(tracing::field::display), "{message}"),
};

/// Install the existing log layers and optional batch OTLP exporter.
pub fn init(
    default_service: &'static str,
    filter: EnvFilter,
    events: tracing_subscriber::fmt::format::FmtSpan,
) -> Option<SdkTracerProvider> {
    SERVICE.init(default_service, filter, events)
}

/// Flush on a blocking worker, with a separate one-second exporter stop budget.
pub async fn shutdown(provider: Option<SdkTracerProvider>) {
    if let Some(provider) = provider {
        let _ = tokio::task::spawn_blocking(move || {
            provider.shutdown_with_timeout(Duration::from_secs(1))
        })
        .await;
    }
}

#[derive(Clone, Default)]
pub(crate) struct HttpTrace {
    pub(crate) parent: opentelemetry::Context,
    pub(crate) spans: std::sync::Arc<std::sync::Mutex<Vec<tracing::Span>>>,
}
tokio::task_local! {
    pub(crate) static HTTP_TRACE: HttpTrace;
}

pub(crate) fn rpc_span(method: &'static str) -> tracing::Span {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let span = tracing::info_span!(
        target: "groundhog_otel",
        "jsonrpc",
        otel.name = method,
        otel.kind = "server",
        rpc.system = "jsonrpc",
        rpc.method = method,
        rpc.jsonrpc.error_code = tracing::field::Empty,
        http.response.status_code = tracing::field::Empty
    );
    if span.is_disabled() {
        return span;
    }
    let _ = HTTP_TRACE.try_with(|trace| {
        let _ = span.set_parent(trace.parent.clone());
        if let Ok(mut spans) = trace.spans.lock() {
            spans.push(span.clone());
        }
    });
    span
}

pub(crate) fn backend_span(backend: &str, operation: &str) -> tracing::Span {
    tracing::info_span!(
        target: "groundhog_otel",
        "backend",
        otel.name = %format!("{backend}.{operation}"),
        otel.kind = "client",
        rpc.system = "x23",
        rpc.service = backend,
        rpc.method = operation
    )
}

/// Shared work inherits the first request's sampling and links every served request.
pub(crate) fn shared_work_span(component: &str, parent: &tracing::Span) -> tracing::Span {
    tracing::info_span!(target: "groundhog_otel", parent: parent, "shared_work", component)
}

pub(crate) fn link_request(work: &tracing::Span, request: &tracing::Span) {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let context = request.context();
    let span_context = context.span().span_context().clone();
    if span_context.is_valid() {
        work.add_link(span_context);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_sdk::trace::{Sampler, SpanData};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone, Debug, Default)]
    pub(crate) struct Capture {
        pub(crate) spans: Arc<Mutex<Vec<SpanData>>>,
        pub(crate) shutdown_timeout: Arc<Mutex<Option<Duration>>>,
    }

    impl opentelemetry_sdk::trace::SpanProcessor for Capture {
        fn on_start(&self, _: &mut opentelemetry_sdk::trace::Span, _: &opentelemetry::Context) {}
        fn on_end(&self, span: SpanData) {
            self.spans.lock().unwrap().push(span);
        }
        fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
            Ok(())
        }
        fn shutdown_with_timeout(
            &self,
            timeout: Duration,
        ) -> opentelemetry_sdk::error::OTelSdkResult {
            *self.shutdown_timeout.lock().unwrap() = Some(timeout);
            Ok(())
        }
    }

    pub(crate) fn subscriber() -> (
        Capture,
        SdkTracerProvider,
        impl tracing::Subscriber + Send + Sync,
    ) {
        let capture = Capture::default();
        let provider = SERVICE
            .provider_builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_span_processor(capture.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        (capture, provider, subscriber)
    }
}

#[cfg(test)]
mod tests;
