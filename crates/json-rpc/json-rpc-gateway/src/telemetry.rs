//! Optional OTLP tracing; exporter failures never affect service availability.
use opentelemetry::{propagation::Extractor, trace::TracerProvider};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    Resource,
    trace::{Sampler, SdkTracerProvider, SpanData, SpanExporter},
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tracing_subscriber::filter::FilterExt;
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug)]
struct BestEffortExporter {
    inner: opentelemetry_otlp::SpanExporter,
    warned: AtomicBool,
}

impl SpanExporter for BestEffortExporter {
    async fn export(&self, batch: Vec<SpanData>) -> opentelemetry_sdk::error::OTelSdkResult {
        if let Err(error) = self.inner.export(batch).await
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(%error, "OTLP collector unavailable; continuing without exported spans");
        }
        Ok(())
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }
}

fn provider(
    default_service: &'static str,
    endpoint: Option<std::ffi::OsString>,
) -> Result<Option<SdkTracerProvider>, opentelemetry_otlp::ExporterBuildError> {
    if sdk_disabled(std::env::var("OTEL_SDK_DISABLED").ok().as_deref()) {
        return Ok(None);
    }
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint.to_string_lossy().into_owned())
        .build()?;
    // The default resource builder includes the SDK's environment detectors.
    let resource = Resource::builder()
        .with_service_name(default_service)
        .with_detector(Box::new(
            opentelemetry_sdk::resource::EnvResourceDetector::new(),
        ));
    let resource = match std::env::var("OTEL_SERVICE_NAME") {
        Ok(service) => resource.with_service_name(service),
        Err(_) => resource,
    }
    .build();
    Ok(Some(
        provider_builder()
            .with_resource(resource)
            .with_batch_exporter(BestEffortExporter {
                inner: exporter,
                warned: AtomicBool::new(false),
            })
            .build(),
    ))
}

fn provider_builder() -> opentelemetry_sdk::trace::TracerProviderBuilder {
    let sampler = std::env::var("OTEL_TRACES_SAMPLER").ok();
    // 0.33 reads sampler env vars in the builder's default Config. Suppress its
    // always-on fallback warning: init emits our corrected fallback warning.
    let builder = if invalid_sampler(sampler.as_deref()) {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            SdkTracerProvider::builder()
        })
    } else {
        SdkTracerProvider::builder()
    };
    // Batches and tip cohorts may exceed the SDK's default 128-link limit.
    // Their ingress bounds limit memory; retain every served request's link.
    let builder = builder.with_max_links_per_span(u32::MAX);
    if sampler.is_none() || invalid_sampler(sampler.as_deref()) {
        builder.with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            0.05,
        ))))
    } else {
        builder
    }
}

fn sdk_disabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn invalid_sampler(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        !matches!(
            value,
            "always_on"
                | "always_off"
                | "traceidratio"
                | "parentbased_always_on"
                | "parentbased_always_off"
                | "parentbased_traceidratio"
        )
    })
}

/// Install the existing log layers and optional batch OTLP exporter.
pub fn init(
    default_service: &'static str,
    filter: EnvFilter,
    events: tracing_subscriber::fmt::format::FmtSpan,
) -> Option<SdkTracerProvider> {
    let result = provider(
        default_service,
        std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT"),
    );
    let (provider, error) = match result {
        Ok(provider) => (provider, None),
        Err(error) => (None, Some(error)),
    };
    let layer = provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer().with_tracer(provider.tracer(default_service))
    });
    let enabled = provider.is_some();
    tracing_subscriber::registry()
        // An absent optional layer alone does not disable span construction.
        .with(tracing_subscriber::filter::filter_fn(move |metadata| {
            enabled || metadata.target() != "groundhog_otel"
        }))
        .with(
            tracing_subscriber::fmt::layer()
                .with_span_events(events)
                // OTLP-only spans must not change existing log context or close events.
                .with_filter(
                    filter.and(tracing_subscriber::filter::filter_fn(|metadata| {
                        metadata.target() != "groundhog_otel"
                    })),
                ),
        )
        .with(layer)
        .init();
    if provider.is_some() && invalid_sampler(std::env::var("OTEL_TRACES_SAMPLER").ok().as_deref()) {
        tracing::warn!(
            "invalid OTEL_TRACES_SAMPLER; using parentbased_traceidratio with ratio 0.05"
        );
    }
    if let Some(error) = error {
        tracing::warn!(%error, "cannot configure OTLP tracing; continuing");
    }
    provider
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

struct Headers<'a>(&'a axum::http::HeaderMap);
impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.to_str().ok()
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(axum::http::HeaderName::as_str).collect()
    }
}

pub(crate) fn extract(headers: &axum::http::HeaderMap) -> opentelemetry::Context {
    use opentelemetry::propagation::TextMapPropagator;
    opentelemetry_sdk::propagation::TraceContextPropagator::new().extract(&Headers(headers))
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
    use std::sync::{Arc, Mutex};

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
        let provider = provider_builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_span_processor(capture.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        (capture, provider, subscriber)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_passes_separate_exporter_budget() {
        let (capture, provider, _) = test_support::subscriber();
        shutdown(Some(provider)).await;
        assert_eq!(
            *capture.shutdown_timeout.lock().unwrap(),
            Some(Duration::from_secs(1))
        );
    }
    // Child processes isolate SDK env parsing from concurrently running tests.
    #[test]
    fn sampler_environment() {
        if let Ok(expected) = std::env::var("GROUNDHOG_TEST_SAMPLER") {
            assert!(format!("{:?}", provider_builder()).contains(&expected));
            return;
        }
        for (sampler, expected) in [
            (None, "ParentBased(TraceIdRatioBased(0.05))"),
            (Some("always_on"), "sampler: AlwaysOn"),
            (Some("always_off"), "sampler: AlwaysOff"),
            (Some("traceidratio"), "sampler: TraceIdRatioBased(0.25)"),
            (Some("parentbased_always_on"), "ParentBased(AlwaysOn)"),
            (Some("parentbased_always_off"), "ParentBased(AlwaysOff)"),
            (
                Some("parentbased_traceidratio"),
                "ParentBased(TraceIdRatioBased(0.25))",
            ),
            (Some("invalid"), "ParentBased(TraceIdRatioBased(0.05))"),
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "telemetry::tests::sampler_environment",
                    "--nocapture",
                ])
                .env_remove("OTEL_TRACES_SAMPLER")
                .env("OTEL_TRACES_SAMPLER_ARG", "0.25")
                .env("GROUNDHOG_TEST_SAMPLER", expected);
            if let Some(sampler) = sampler {
                child.env("OTEL_TRACES_SAMPLER", sampler);
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{:?}: {}",
                sampler,
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    fn sdk_disabled_with_endpoint() {
        if std::env::var_os("GROUNDHOG_TEST_DISABLED").is_some() {
            assert!(
                provider("test", Some("invalid endpoint".into()))
                    .unwrap()
                    .is_none()
            );
            assert!(
                init(
                    "test",
                    EnvFilter::new("trace"),
                    tracing_subscriber::fmt::format::FmtSpan::NONE,
                )
                .is_none()
            );
            assert!(tracing::info_span!(target: "groundhog_otel", "disabled").is_disabled());
            return;
        }
        assert!(sdk_disabled(Some("true")));
        assert!(sdk_disabled(Some("TRUE")));
        assert!(!sdk_disabled(Some("false")));
        assert!(!sdk_disabled(None));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "telemetry::tests::sdk_disabled_with_endpoint"])
            .env("GROUNDHOG_TEST_DISABLED", "1")
            .env("OTEL_SDK_DISABLED", "true")
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", "invalid endpoint")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[tokio::test]
    async fn invalid_sampler_warns_once() {
        if std::env::var_os("GROUNDHOG_TEST_INVALID").is_some() {
            let provider = init(
                "test",
                EnvFilter::new("warn"),
                tracing_subscriber::fmt::format::FmtSpan::NONE,
            );
            assert!(provider.is_some());
            shutdown(provider).await;
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "telemetry::tests::invalid_sampler_warns_once",
                "--nocapture",
            ])
            .env("GROUNDHOG_TEST_INVALID", "1")
            .env_remove("OTEL_SDK_DISABLED")
            .env("OTEL_TRACES_SAMPLER", "invalid")
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317")
            .output()
            .unwrap();
        let logs = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{logs}");
        assert_eq!(
            logs.matches("invalid OTEL_TRACES_SAMPLER").count(),
            1,
            "{logs}"
        );
        assert!(
            !logs.contains("TracerProvider.Config.InvalidSamplerType"),
            "{logs}"
        );
    }
    #[test]
    fn disabled_without_endpoint_builds_no_exporter() {
        assert!(provider("test", None).unwrap().is_none());
    }
    #[tokio::test]
    async fn enabled_with_endpoint_builds_provider_without_collector() {
        let provider = provider("test", Some("http://127.0.0.1:4317".into())).unwrap();
        assert!(provider.is_some());
        shutdown(provider).await;
    }
}
