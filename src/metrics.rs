use std::{sync::LazyLock, time::Instant};

use anyhow::Error;
use axum::{extract::Request, middleware::Next, response::IntoResponse};
use prometheus::{
    self, register_histogram_vec, register_int_counter_vec, register_int_gauge, Encoder,
    HistogramVec, IntCounterVec, IntGauge, TextEncoder,
};

use crate::ResponseContext;

pub(crate) static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::new);

pub(crate) struct Metrics {
    pub(crate) http_requests_total: IntCounterVec,
    pub(crate) http_request_duration_seconds: HistogramVec,
    pub(crate) http_requests_in_flight: IntGauge,
}

impl Metrics {
    fn new() -> Metrics {
        Metrics {
            http_requests_total: register_int_counter_vec!(
                "http_requests_total",
                "Number of http requests received",
                &["status", "method", "location", "backend_name"]
            )
            .expect("Error creating prometheus counter"),

            http_request_duration_seconds: register_histogram_vec!(
                "http_request_duration_seconds",
                "The HTTP request latencies in seconds.",
                &["status", "method", "location", "backend_name"]
            )
            .expect("Error creating histogram"),

            http_requests_in_flight: register_int_gauge!(
                "http_requests_in_flight",
                "Number of HTTP requests currently being handled"
            )
            .expect("Error creating in flight gauge"),
        }
    }
}

// Increments the in flight gauge on creation and decrements on drop, so the count
// stays correct even if the request future is cancelled (client disconnect) or panics
struct InFlightGuard;

impl InFlightGuard {
    fn new() -> Self {
        METRICS.http_requests_in_flight.inc();
        InFlightGuard
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        METRICS.http_requests_in_flight.dec();
    }
}

pub(crate) fn encode_metrics() -> Result<String, Error> {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer)?;
    Ok(String::from_utf8(buffer)?)
}

pub(crate) async fn record_metrics(req: Request, next: Next) -> impl IntoResponse {
    // this counts the current request too, so during a metrics scrape the gauge has
    // a floor of 1 (it sees itself) rather than reading 0
    let _in_flight = InFlightGuard::new();
    let method = req.method().clone();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed();
    let status = response.status();
    let ctx = response.extensions().get::<ResponseContext>();
    let location = ctx
        .map(|c| c.backend_location.as_str())
        .unwrap_or("internal");
    let backend_name = ctx.map(|c| c.backend_name.as_str()).unwrap_or("internal");

    METRICS
        .http_requests_total
        .with_label_values(&[status.as_str(), method.as_str(), location, backend_name])
        .inc();

    METRICS
        .http_request_duration_seconds
        .with_label_values(&[status.as_str(), method.as_str(), location, backend_name])
        .observe(duration.as_secs_f64());

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_metrics_struct() {
        METRICS
            .http_requests_total
            .with_label_values(&["200", "GET", "127.0.0.1:8000", "test.home"])
            .inc();
        assert!(
            METRICS
                .http_requests_total
                .with_label_values(&["200", "GET", "127.0.0.1:8000", "test.home"])
                .get()
                >= 1
        );
    }

    #[tokio::test]
    async fn test_encode_metrics() {
        METRICS
            .http_requests_total
            .with_label_values(&["200", "GET", "127.0.0.1:8000", "test.home"])
            .inc();
        assert!(encode_metrics().unwrap().contains(
            "# HELP http_requests_total Number of http requests received\n\
                # TYPE http_requests_total counter\nhttp_requests_total"
        ));
    }
}
