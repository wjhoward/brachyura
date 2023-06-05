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
