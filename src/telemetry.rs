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
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::Instrument;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

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

    // try_init does not panic if a subscriber is already set. It also bridges the
    // old log crate into tracing, so logs from dependencies still using log are captured
    if std::env::var("OTEL_TRACES_EXPORTER").as_deref() == Ok("stdout") {
        // The provider owns the export pipeline. The simple exporter writes each
        // finished span straight to stdout, which is enough for local dev
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(opentelemetry_stdout::SpanExporter::default())
            .build();
        let tracer = provider.tracer("brachyura");

        // The bridge layer turns tracing spans into OpenTelemetry spans
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(otel_layer)
            .try_init();

        Some(provider)
    } else {
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .try_init();

        None
    }
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
