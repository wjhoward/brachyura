use std::{sync::LazyLock, time::Instant};

use anyhow::Error;
use axum::{extract::Request, middleware::Next, response::IntoResponse};
use prometheus::{
    self, register_histogram_vec, register_int_counter_vec, Encoder, HistogramVec, IntCounterVec,
    TextEncoder,
};

use crate::ResponseContext;

pub(crate) static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::new);

pub(crate) struct Metrics {
    pub(crate) http_request_counter: IntCounterVec,
    pub(crate) http_request_duration: HistogramVec,
}

impl Metrics {
    fn new() -> Metrics {
        Metrics {
            http_request_counter: register_int_counter_vec!(
                "http_request_total",
                "Number of http requests received",
                &["status", "backend"]
            )
            .expect("Error creating prometheus counter"),

            http_request_duration: register_histogram_vec!(
                "http_request_duration_seconds",
                "The HTTP request latencies in seconds.",
                &["status", "backend"]
            )
            .expect("Error creating histogram counter"),
        }
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
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed();
    let backend = response
        .extensions()
        .get::<ResponseContext>()
        .map(|ctx| ctx.backend_location.as_str())
        .unwrap_or("internal");

    METRICS
        .http_request_counter
        .with_label_values(&[response.status().as_str(), backend])
        .inc();

    METRICS
        .http_request_duration
        .with_label_values(&[response.status().as_str(), backend])
        .observe(duration.as_secs_f64());

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_metrics_struct() {
        METRICS
            .http_request_counter
            .with_label_values(&["200", "test"])
            .inc();
        assert!(
            METRICS
                .http_request_counter
                .with_label_values(&["200", "test"])
                .get()
                >= 1
        );
    }

    #[tokio::test]
    async fn test_encode_metrics() {
        METRICS
            .http_request_counter
            .with_label_values(&["200", "test"])
            .inc();
        assert!(encode_metrics().unwrap().contains(
            "# HELP http_request_total Number of http requests received\n\
                # TYPE http_request_total counter\nhttp_request_total"
        ));
    }
}
