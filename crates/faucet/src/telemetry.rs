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
    subscriber(default_service, filter, events, provider.as_ref()).init();
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

fn subscriber(
    default_service: &'static str,
    filter: EnvFilter,
    events: tracing_subscriber::fmt::format::FmtSpan,
    provider: Option<&SdkTracerProvider>,
) -> Box<dyn tracing::Subscriber + Send + Sync> {
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_span_events(events)
            // Keep OTLP-only spans out of log context and close events.
            .with_filter(
                filter.and(tracing_subscriber::filter::filter_fn(|metadata| {
                    metadata.target() != "groundhog_otel"
                })),
            ),
    );
    match provider {
        Some(provider) => Box::new(
            subscriber
                .with(tracing_opentelemetry::layer().with_tracer(provider.tracer(default_service))),
        ),
        None => Box::new(subscriber),
    }
}

/// Flush and stop on a blocking worker so tonic can continue using Tokio.
pub async fn shutdown(provider: Option<SdkTracerProvider>) {
    if let Some(provider) = provider {
        let _ = tokio::task::spawn_blocking(move || provider.shutdown()).await;
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

struct OutgoingHeaders(axum::http::HeaderMap);

impl opentelemetry::propagation::Injector for OutgoingHeaders {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(key), Ok(value)) = (
            axum::http::HeaderName::try_from(key),
            axum::http::HeaderValue::try_from(value),
        ) {
            self.0.insert(key, value);
        }
    }
}

pub(crate) async fn submit_transaction(
    rpc_url: &str,
    tx_hex: &str,
) -> Result<
    thruput_json_rpc_types::transaction::TxHash,
    thruput_json_rpc_client::SubmitTransactionError,
> {
    use thruput_json_rpc_client::{
        HttpClientBuilder, SubmitTransactionError, submit_transaction_with_client,
    };
    use opentelemetry::propagation::TextMapPropagator;
    use tracing::Instrument;
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let span = tracing::info_span!(
        target: "groundhog_otel",
        "jsonrpc.submit",
        otel.name = "groundhog_sendRawTransaction",
        otel.kind = "client",
        rpc.system = "jsonrpc",
        rpc.method = "groundhog_sendRawTransaction"
    );
    async {
        let mut headers = OutgoingHeaders(axum::http::HeaderMap::new());
        opentelemetry_sdk::propagation::TraceContextPropagator::new()
            .inject_context(&tracing::Span::current().context(), &mut headers);
        let client = HttpClientBuilder::default()
            .set_headers(headers.0)
            .build(rpc_url)
            .map_err(|source| SubmitTransactionError::Connect {
                rpc_url: rpc_url.to_owned(),
                source,
            })?;
        submit_transaction_with_client(&client, tx_hex)
            .await
            .map_err(SubmitTransactionError::SendRawTransaction)
    }
    .instrument(span)
    .await
}
#[cfg(test)]
mod tests {
    use super::*;

    fn assert_disabled_tracing() {
        assert!(tracing::info_span!("ordinary").is_disabled());
        assert!(!tracing::enabled!(tracing::Level::DEBUG));
        assert!(!tracing::enabled!(tracing::Level::TRACE));
        assert!(tracing::info_span!(target: "groundhog_otel", "disabled").is_disabled());
        assert!(tracing::enabled!(tracing::Level::WARN));
    }

    #[test]
    fn disabled_subscriber_respects_filter() {
        tracing::subscriber::with_default(
            subscriber(
                "test",
                EnvFilter::new("warn"),
                tracing_subscriber::fmt::format::FmtSpan::NONE,
                None,
            ),
            assert_disabled_tracing,
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
                    EnvFilter::new("warn"),
                    tracing_subscriber::fmt::format::FmtSpan::NONE,
                )
                .is_none()
            );
            assert_disabled_tracing();
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
        if std::env::var_os("GROUNDHOG_TEST_NO_ENDPOINT").is_some() {
            assert!(
                init(
                    "test",
                    EnvFilter::new("warn"),
                    tracing_subscriber::fmt::format::FmtSpan::NONE,
                )
                .is_none()
            );
            assert_disabled_tracing();
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "telemetry::tests::disabled_without_endpoint_builds_no_exporter",
            ])
            .env("GROUNDHOG_TEST_NO_ENDPOINT", "1")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .env_remove("OTEL_SDK_DISABLED")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    #[tokio::test]
    async fn enabled_with_endpoint_builds_provider_without_collector() {
        let provider = provider("test", Some("http://127.0.0.1:4317".into())).unwrap();
        assert!(provider.is_some());
        shutdown(provider).await;
    }
}
