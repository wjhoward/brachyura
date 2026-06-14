// Sets up the global tracing subscriber: log events plus an optional
// OpenTelemetry span export pipeline, gated on the OTEL_TRACES_EXPORTER env var
//
//   app code (info!, span!)
//        |  emits events (logs) + spans (traces)
//        v
//   subscriber (registry of layers)
//     EnvFilter    drop anything below RUST_LOG
//     fmt layer    -> stdout (human readable logs)
//     otel bridge  -> SdkTracerProvider -> exporter -> stdout / Jaeger (spans)
//
// Logs and spans are both produced by app code and consumed by the subscriber.
// Spans are additionally exported onward by the provider

use axum::{extract::Request, middleware::Next, response::Response};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::SdkTracerProvider, Resource};
use tracing::Instrument;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Identifies this service in exported traces (otherwise spans show as
/// "unknown_service").
fn resource() -> Resource {
    Resource::builder().with_service_name("brachyura").build()
}

/// Builds a tracer provider for the configured exporter, or None when export is
/// disabled. Separate from init so the exporter selection can be unit tested
/// without installing the global subscriber.
fn build_provider(exporter: Option<&str>) -> Option<SdkTracerProvider> {
    match exporter {
        // Write each finished span to stdout, useful for local debugging with no collector
        Some("stdout") => Some(
            SdkTracerProvider::builder()
                .with_simple_exporter(opentelemetry_stdout::SpanExporter::default())
                .with_resource(resource())
                .build(),
        ),
        // Batch and export spans over OTLP/gRPC, defaults to localhost:4317 (e.g. Jaeger)
        Some("otlp") => {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .build()
                .expect("failed to build OTLP exporter");
            Some(
                SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource())
                    .build(),
            )
        }
        // No exporter configured, logs only
        _ => None,
    }
}

/// Installs the global tracing subscriber.
///
/// Returns the OpenTelemetry provider when span export is enabled so the caller
/// can shut it down on exit to flush buffered spans. Returns None when only
/// local logging is active.
pub(crate) fn init() -> Option<SdkTracerProvider> {
    // RUST_LOG controls verbosity, defaulting to info when unset or unparseable
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Human readable log output to stdout
    let fmt_layer = tracing_subscriber::fmt::layer().compact();

    let exporter = std::env::var("OTEL_TRACES_EXPORTER").ok();
    let provider = build_provider(exporter.as_deref());

    // Install the subscriber, adding the OTel bridge layer only when exporting.
    // try_init does not panic if a subscriber is already set, and also bridges the
    // old log crate into tracing so logs from dependencies using log are captured
    if let Some(provider) = &provider {
        // Propagate trace context to backends via the W3C traceparent header
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let otel_layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("brachyura"));
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(otel_layer)
            .try_init();
    } else {
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .try_init();
    }

    provider
}

/// Wraps each request in a span, so log events emitted while handling the
/// request carry its context, and the span itself becomes the per request
/// OpenTelemetry span.
pub(crate) async fn trace_request(req: Request, next: Next) -> Response {
    let span = tracing::info_span!(
        "request",
        method = %req.method(),
        path = req.uri().path(),
        // declared now but filled in once the response is known
        status = tracing::field::Empty,
    );

    // instrument is the async safe way to scope the span to the request
    // clone so the original handle survives to record status below
    let response = next.run(req).instrument(span.clone()).await;

    span.record("status", response.status().as_str());

    response
    // the span closes when its last handle drops at the end of this scope, which
    // is when its end timestamp is recorded, there is no explicit end call
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_service_name() {
        let name = resource()
            .get(&opentelemetry::Key::new("service.name"))
            .map(|v| v.as_str().to_string());
        assert_eq!(name.as_deref(), Some("brachyura"));
    }

    #[test]
    fn test_build_provider_disabled() {
        assert!(build_provider(None).is_none());
        assert!(build_provider(Some("unknown")).is_none());
    }

    // stdout and otlp are smoke tests: the provider internals are not inspectable,
    // so we can only assert a valid exporter name builds a provider without panicking
    #[test]
    fn test_build_provider_stdout() {
        assert!(build_provider(Some("stdout")).is_some());
    }

    #[tokio::test]
    async fn test_build_provider_otlp() {
        // The batch exporter spawns a background task, so it needs a tokio runtime
        let provider = build_provider(Some("otlp"));
        assert!(provider.is_some());
        // Shut down explicitly so the background task stops before the runtime ends
        if let Some(provider) = provider {
            let _ = provider.shutdown();
        }
    }
}
