use std::time::Duration;

use anyhow::Error;
use axum::body::Body;
use hyper::http::Response;
use once_cell::sync::Lazy;
use prometheus::{
    self, register_histogram_vec, register_int_counter_vec, Encoder, HistogramVec, IntCounterVec,
    TextEncoder,
};

pub static METRICS: Lazy<Metrics> = Lazy::new(Metrics::new);

pub struct Metrics {
    pub http_request_counter: IntCounterVec,
    pub http_request_duration: HistogramVec,
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

pub fn encode_metrics() -> Result<String, Error> {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer)?;
    Ok(String::from_utf8(buffer.clone())?)
}

pub fn record_metrics(
    response: &Response<Body>,
    backend_location: String,
    duration: Duration,
) -> Result<(), Error> {
    METRICS
        .http_request_counter
        .with_label_values(&[response.status().as_str(), backend_location.as_str()])
        .inc_by(1);

    METRICS
        .http_request_duration
        .with_label_values(&[response.status().as_str(), backend_location.as_str()])
        .observe(duration.as_secs_f64());
    Ok(())
}

mod tests {
    #![allow(unused_imports)]
    use super::*;

    #[tokio::test]
    async fn test_metrics_struct() {
        METRICS
            .http_request_counter
            .with_label_values(&["200", "test"])
            .inc_by(1);
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
            .inc_by(1);
        assert!(encode_metrics().unwrap().contains(
            "# HELP http_request_total Number of http requests received\n\
                # TYPE http_request_total counter\nhttp_request_total"
        ));
    }

    #[tokio::test]
    async fn test_record_metrics() {
        let response = Response::builder().body(Body::from("test")).unwrap();
        assert_eq!(
            record_metrics(
                &response,
                "127.0.0.1:10000".to_string(),
                Duration::from_micros(10)
            )
            .is_ok(),
            true
        );
        assert!(encode_metrics().unwrap().contains("127.0.0.1:10000"));
    }
}
