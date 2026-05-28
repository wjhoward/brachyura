use std::{net::TcpListener, sync::OnceLock, thread, time::Duration};

use brachyura::run_server;
use reqwest::{header::HOST, Error, Method, Response};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

// All services (proxy + mock backends) share a single dedicated thread and runtime,
// so they outlive any individual test and are initialised exactly once
static SERVICES_STARTED: OnceLock<()> = OnceLock::new();

fn start_services() {
    SERVICES_STARTED.get_or_init(|| {
        // Install the rustls crypto provider once for the entire test process
        let _ = rustls::crypto::ring::default_provider().install_default();
        thread::spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    // Both backends are kept alive as locals for the duration of run_server
                    let _backend1 =
                        init_mock_backend("127.0.0.1:8000", "This is the mock backend!").await;
                    let _backend2 =
                        init_mock_backend("127.0.0.1:8001", "This is the mock backend 2!").await;
                    let _ = run_server("./tests/config.yaml".to_string()).await;
                });
        });
        // Wait for proxy and backends to bind and start accepting connections
        thread::sleep(Duration::from_millis(100));
    });
}

async fn init_mock_backend(address: &'static str, resp_body: &'static str) -> MockServer {
    let listener = TcpListener::bind(address).unwrap();
    let mock_server = MockServer::builder().listener(listener).start().await;

    Mock::given(method("GET"))
        .and(path("/test"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(resp_body, "text/plain"))
        .mount(&mock_server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/test"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/test"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/test"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/delay"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1000)))
        .mount(&mock_server)
        .await;

    mock_server
}

async fn assert_response(
    resp: Result<Response, Error>,
    expected_status: u16,
    expected_body: Option<&str>,
) {
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, expected_status);
    if let Some(expected_body) = expected_body {
        assert_eq!(body, expected_body);
    }
}

async fn http_request(
    protocol: &str,
    url: &str,
    host_header: Option<&str>,
    no_proxy: bool,
    method: Option<Method>,
) -> Result<Response, Error> {
    let mut client_builder = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .use_rustls_tls();
    if protocol == "http1" {
        client_builder = client_builder.http1_only();
    } else {
        client_builder = client_builder.http2_adaptive_window(true);
    }
    let client = client_builder.build().unwrap();

    let request_builder = match method {
        Some(Method::HEAD) => client.head(url),
        Some(Method::POST) => client.post(url),
        Some(Method::PUT) => client.put(url),
        _ => client.get(url),
    };

    if let Some(host) = host_header {
        request_builder.header(HOST, host).send().await
    } else if no_proxy {
        request_builder.header("x-no-proxy", "true").send().await
    } else {
        request_builder.send().await
    }
}

#[tokio::test]
async fn http1_get() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        None,
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, Some("This is the mock backend!")).await;
}

#[tokio::test]
async fn http1_get_no_host_header() {
    start_services();

    // Send a request to the proxy without a host header
    let resp = http_request("http1", "https://127.0.0.1:4000/test", None, false, None).await;

    // In this case the proxy should respond with a 404
    assert_response(resp, 404, Some("Host header not defined")).await;
}

#[tokio::test]
async fn http1_get_no_proxy_header_status() {
    start_services();

    // Send an internal /status request
    let resp = http_request("http1", "https://127.0.0.1:4000/status", None, true, None).await;

    // In this case the proxy should respond with a 200
    assert_response(resp, 200, Some("The proxy is running")).await;
}

#[tokio::test]
async fn http1_get_no_proxy_header_metrics() {
    start_services();

    // Send a request to ensure metrics exist
    let _ = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        None,
    )
    .await;

    // Send an internal /metrics request
    let resp = http_request("http1", "https://127.0.0.1:4000/metrics", None, true, None).await;
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 200);
    assert!(body.starts_with(b"# HELP http_request_duration_seconds"));
}

#[tokio::test]
async fn http1_head() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        Some(Method::HEAD),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;
}

#[tokio::test]
async fn http1_post() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        Some(Method::POST),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;
}

#[tokio::test]
async fn http1_put() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        Some(Method::PUT),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;
}

#[tokio::test]
async fn http2_get() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        None,
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, Some("This is the mock backend!")).await;
}

#[tokio::test]
async fn http2_get_no_host_header() {
    start_services();

    // Send a request to the proxy without a host header
    let resp = http_request("http2", "https://127.0.0.1:4000/test", None, false, None).await;

    // In this case the proxy should respond with a 404
    assert_response(resp, 404, Some("Host header not defined")).await;
}

#[tokio::test]
async fn http2_get_no_proxy_header_status() {
    start_services();

    // Send an internal /status request
    let resp = http_request("http2", "https://127.0.0.1:4000/status", None, true, None).await;

    // In this case the proxy should respond with a 200
    assert_response(resp, 200, Some("The proxy is running")).await;
}

#[tokio::test]
async fn http2_get_no_proxy_header_metrics() {
    start_services();

    // Send a request to ensure metrics exist
    let _ = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        None,
    )
    .await;

    // Send an internal /metrics request
    let resp = http_request("http2", "https://127.0.0.1:4000/metrics", None, true, None).await;
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 200);
    assert!(body.starts_with(b"# HELP http_request_duration_seconds"));
}

#[tokio::test]
async fn http2_head() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        Some(Method::HEAD),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;
}

#[tokio::test]
async fn http2_post() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        Some(Method::POST),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;
}

#[tokio::test]
async fn http2_put() {
    start_services();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        false,
        Some(Method::PUT),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;
}

#[tokio::test]
async fn load_balancing_round_robin() {
    start_services();

    // Response from the first mock backend
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test-lb.home"),
        false,
        None,
    )
    .await;

    assert_response(resp, 200, Some("This is the mock backend!")).await;

    // Response from the second mock backend
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test-lb.home"),
        false,
        None,
    )
    .await;
    assert_response(resp, 200, Some("This is the mock backend 2!")).await;
}

#[tokio::test]
async fn proxied_backend_timeout() {
    start_services();

    // Send a request which will timeout
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/delay",
        Some("test.home"),
        false,
        None,
    )
    .await;
    // In this case the proxy should respond with a 504
    assert_response(resp, 504, Some("Request timeout")).await;
}
