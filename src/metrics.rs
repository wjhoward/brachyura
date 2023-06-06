use anyhow::Error;
use once_cell::sync::Lazy;
use prometheus::register_int_counter;
use prometheus::{self, Encoder, IntCounter, TextEncoder};

pub static METRICS: Lazy<Metrics> = Lazy::new(Metrics::new);

pub struct Metrics {
    pub http_request_counter: IntCounter,
}

impl Metrics {
    fn new() -> Metrics {
        Metrics {
            http_request_counter: register_int_counter!(
                "http_requests_total",
                "Number of http requests received"
            )
            .unwrap(),
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

mod tests {
    #![allow(unused_imports)]
    use super::*;

    #[tokio::test]
    async fn test_metrics_struct() {
        METRICS.http_request_counter.inc();
        assert!(METRICS.http_request_counter.get() >= 1);
    }

    #[tokio::test]
    async fn test_encode_metrics() {
        METRICS.http_request_counter.inc();
        assert!(encode_metrics().unwrap().starts_with(
            "# HELP http_requests_total Number of http requests received\n\
                # TYPE http_requests_total counter\nhttp_requests_total"
        ));
    }
}
