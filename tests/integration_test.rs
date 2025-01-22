use std::{
    net::TcpListener,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread, time,
    time::Duration,
};

use brachyura::run_server;
use reqwest::{header::HOST, Error, Method, Response};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};
pub struct MockBackend {
    pub mock_server: Option<MockServer>,
}

impl MockBackend {
    pub const fn new() -> Self {
        Self { mock_server: None }
    }

    pub async fn init(&mut self, address: &str, resp_body: &str) {
        if self.mock_server.is_none() {
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
            self.mock_server = Some(mock_server);
        }
    }
}

static MOCK_BACKEND: Mutex<MockBackend> = Mutex::new(MockBackend::new());
static MOCK_BACKEND2: Mutex<MockBackend> = Mutex::new(MockBackend::new());
static PROXY_STARTED: Mutex<bool> = Mutex::new(false);
static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn finish(proxy_parent: bool) {
    TEST_COUNTER.fetch_sub(1, Ordering::SeqCst);
    let mut limit = 0; // Prevents an infinite loop if a test thread panics
    if proxy_parent {
        // If dependent tests are still running wait
        while TEST_COUNTER.load(Ordering::SeqCst) != 0 {
            if limit > 25 {
                break;
            }
            thread::sleep(time::Duration::from_millis(100));
            limit += 1;
        }
    }
}

fn start_proxy() -> bool {
    // Track the number of dependant tests, plus the parent thread
    TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let parent;

    // Get the mutex guard/lock until proxy_started goes out of scope
    let mut proxy_started = PROXY_STARTED.lock().unwrap();

    if !*proxy_started {
        parent = true;
        *proxy_started = true;
        tokio::spawn(async move {
            let config_path = String::from("./tests/config.yaml");
            run_server(config_path).await;
        });
    } else {
        // Proxy already running, not started by this thread
        parent = false;
    }
    parent
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
    no_proxy: Option<bool>,
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

    let client_method;
    if method == Some(Method::HEAD) {
        client_method = client.head(url);
    } else if method == Some(Method::POST) {
        client_method = client.post(url);
    } else if method == Some(Method::PUT) {
        client_method = client.put(url);
    } else {
        client_method = client.get(url);
    }
    if host_header.is_some() {
        client_method
            .header(HOST, host_header.unwrap())
            .send()
            .await
    } else if no_proxy.is_some() && no_proxy.unwrap() {
        client_method.header("x-no-proxy", "true").send().await
    } else {
        client_method.send().await
    }
}

fn test_init() {
    // Common test initialization
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Sleep this thread while the server starts up
    thread::sleep(time::Duration::from_millis(1000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_get() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        None,
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, Some("This is the mock backend!")).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_get_no_host_header() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy without a host header
    let resp = http_request("http1", "https://127.0.0.1:4000/test", None, None, None).await;

    // In this case the proxy should respond with a 404
    assert_response(resp, 404, Some("Host header not defined")).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_get_no_proxy_header_status() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send an internal /status request
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/status",
        None,
        Some(true),
        None,
    )
    .await;

    // In this case the proxy should respond with a 200
    assert_response(resp, 200, Some("The proxy is running")).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_get_no_proxy_header_metrics() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to ensure metrics exist
    let _ = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        None,
    )
    .await;

    // Send an internal /metrics request
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/metrics",
        None,
        Some(true),
        None,
    )
    .await;
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 200);
    assert!(body.starts_with(b"# HELP http_request_duration_seconds"));

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_head() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        Some(Method::HEAD),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_post() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        Some(Method::POST),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_put() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        Some(Method::PUT),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http2_get() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        None,
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, Some("This is the mock backend!")).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http2_get_no_host_header() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy without a host header
    let resp = http_request("http2", "https://127.0.0.1:4000/test", None, None, None).await;

    // In this case the proxy should respond with a 404
    assert_response(resp, 404, Some("Host header not defined")).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http2_get_no_proxy_header_status() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send an internal /status request
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/status",
        None,
        Some(true),
        None,
    )
    .await;

    // In this case the proxy should respond with a 200
    assert_response(resp, 200, Some("The proxy is running")).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http2_get_no_proxy_header_metrics() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to ensure metrics exist
    let _ = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        None,
    )
    .await;

    // Send an internal /metrics request
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/metrics",
        None,
        Some(true),
        None,
    )
    .await;
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 200);
    assert!(body.starts_with(b"# HELP http_request_duration_seconds"));

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http2_head() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        Some(Method::HEAD),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http2_post() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        Some(Method::POST),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http2_put() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = http_request(
        "http2",
        "https://127.0.0.1:4000/test",
        Some("test.home"),
        None,
        Some(Method::PUT),
    )
    .await;

    // In this case the response should be a 200 from the mock backend
    assert_response(resp, 200, None).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn load_balancing_round_robin() {
    MOCK_BACKEND
        .lock()
        .unwrap()
        .init("127.0.0.1:8000", "This is the mock backend!")
        .await;
    MOCK_BACKEND2
        .lock()
        .unwrap()
        .init("127.0.0.1:8001", "This is the mock backend 2!")
        .await;
    let proxy_parent = start_proxy();
    test_init();

    // Response from the first mock backend
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test-lb.home"),
        None,
        None,
    )
    .await;

    assert_response(resp, 200, Some("This is the mock backend!")).await;

    // Response from the second mock backend
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/test",
        Some("test-lb.home"),
        None,
        None,
    )
    .await;
    assert_response(resp, 200, Some("This is the mock backend 2!")).await;

    finish(proxy_parent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn proxied_backend_timeout() {
    let proxy_parent = start_proxy();
    test_init();

    // Send a request which will timeout
    let resp = http_request(
        "http1",
        "https://127.0.0.1:4000/delay",
        Some("test.home"),
        None,
        None,
    )
    .await;
    // In this case the proxy should respond with a 504
    assert_response(resp, 504, Some("Request timeout")).await;

    finish(proxy_parent);
}
