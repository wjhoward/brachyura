use hyper::header::HOST;
use std::net::SocketAddr;
use std::{thread, time};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use brachyura::run_server;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_test() {
    // Setup the mock server
    let listener = std::net::TcpListener::bind("127.0.0.1:8000").unwrap();
    let mock_server = MockServer::builder().listener(listener).start().await;

    let template = ResponseTemplate::new(200).set_body_raw("This is the mock server", "text/plain");

    Mock::given(method("GET"))
        .and(path("/test"))
        .respond_with(template)
        .mount(&mock_server)
        .await;

    // Run the proxy server in a separate thread
    tokio::spawn(async move {
        let config_path = String::from("./tests/config.yaml");
        let addr = SocketAddr::from(([127, 0, 0, 1], 4000));
        run_server(addr, config_path).await;
    });

    // Sleep this thread while the server starts up
    thread::sleep(time::Duration::from_millis(1000));

    // Send a request to the proxy, which should be forwarded to the mock server
    let resp = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
        .get("https://localhost:4000/test")
        .header(HOST, "test.home")
        .send()
        .await;

    // Check that the response received from the proxy is from the mock server
    assert_eq!(
        resp.unwrap().text().await.unwrap(),
        "This is the mock server"
    )
}
