use hyper::header::HOST;
use lazy_static::lazy_static;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::{thread, time};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use brachyura::run_server;
pub struct MockBackend {
    pub mock_server: Option<MockServer>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self { mock_server: None }
    }

    pub async fn init(&mut self) {
        let listener = TcpListener::bind("127.0.0.1:8000").unwrap();
        let mock_server = MockServer::builder().listener(listener).start().await;
        let template =
            ResponseTemplate::new(200).set_body_raw("This is the mock backend!", "text/plain");

        Mock::given(method("GET"))
            .and(path("/test"))
            .respond_with(template)
            .mount(&mock_server)
            .await;
        self.mock_server = Some(mock_server);
    }
}

lazy_static! {
    pub static ref MOCK_BACKEND: Mutex<MockBackend> = Mutex::new(MockBackend::new());
    pub static ref PROXY_STARTED: AtomicBool = AtomicBool::new(false);
}

async fn mock_backend_init() {
    // Initialize the mock backend
    // First check we've not already done this
    if MOCK_BACKEND.lock().unwrap().mock_server.is_none() {
        MOCK_BACKEND.lock().unwrap().init().await;
    }
}

fn start_proxy() {
    // Assuming we've not already started it
    if !PROXY_STARTED.load(Ordering::Relaxed) {
        PROXY_STARTED.store(true, Ordering::Relaxed);
        tokio::spawn(async move {
            let config_path = String::from("./tests/config.yaml");
            run_server(config_path).await;
        });
        // Sleep so that the thread that starts the proxy server should
        // last longer than the other test threads, otherwise if the proxy server
        // parent thread finishes before the other tests run, the proxy server thread
        // is terminated
        // Must be a better way to solve this...
        thread::sleep(time::Duration::from_millis(1000));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_test() {
    mock_backend_init().await;
    start_proxy();

    // Sleep this thread while the server starts up
    thread::sleep(time::Duration::from_millis(1000));

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .http1_only()
        .build()
        .unwrap()
        .get("https://localhost:4000/test")
        .header(HOST, "test.home")
        .send()
        .await;

    // In this case the response should be from the mock backend
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "This is the mock backend!");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_no_host_header_test() {
    mock_backend_init().await;
    start_proxy();

    // Sleep this thread while the server starts up
    thread::sleep(time::Duration::from_millis(1000));

    // Send a request to the proxy without a host header
    let resp = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .http1_only()
        .build()
        .unwrap()
        .get("https://localhost:4000/test")
        .send()
        .await;

    // In this case the proxy should respond with a 404
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 404);
    assert_eq!(body, "Host header not defined");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http1_no_proxy_header_status() {
    mock_backend_init().await;
    start_proxy();

    // Sleep this thread while the server starts up
    thread::sleep(time::Duration::from_millis(1000));

    // Send an internal /status request
    let resp = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .http1_only()
        .build()
        .unwrap()
        .get("https://localhost:4000/status")
        .header("x-no-proxy", "true")
        .send()
        .await;

    // In this case the proxy should respond with a 200
    let response = resp.unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "The proxy is running");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[should_panic]
async fn http1_only_test() {
    mock_backend_init().await;
    start_proxy();

    // Sleep this thread while the server starts up
    thread::sleep(time::Duration::from_millis(1000));

    // Send an HTTP2 request
    let resp = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .http2_prior_knowledge()
        .build()
        .unwrap()
        .get("https://localhost:4000/status")
        .header("x-no-proxy", "true")
        .send()
        .await;

    let _ = resp.unwrap();
}

// TODO

// HTTP 2
// HTTP 2 no host header
// HTTP 2 no proxy header
