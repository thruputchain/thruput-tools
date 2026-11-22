use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;

pub(crate) use thruput_telemetry::extract;

const SERVICE: thruput_telemetry::ServiceTelemetry = thruput_telemetry::ServiceTelemetry {
    crate_target: env!("CARGO_CRATE_NAME"),
    max_links_per_span: None,
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

/// Flush and stop on a blocking worker so tonic can continue using Tokio.
pub async fn shutdown(provider: Option<SdkTracerProvider>) {
    if let Some(provider) = provider {
        let _ = tokio::task::spawn_blocking(move || provider.shutdown()).await;
    }
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

/// Propagate the active request span through native RPC calls.
pub(crate) fn rpc_client(
    rpc_url: &str,
) -> Result<thruput_json_rpc_client::HttpClient, thruput_client::jsonrpsee::core::client::Error> {
    use opentelemetry::propagation::TextMapPropagator;
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let mut headers = OutgoingHeaders(axum::http::HeaderMap::new());
    opentelemetry_sdk::propagation::TraceContextPropagator::new()
        .inject_context(&tracing::Span::current().context(), &mut headers);
    thruput_json_rpc_client::HttpClientBuilder::default()
        .set_headers(headers.0)
        .build(rpc_url)
}
