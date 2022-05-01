use hyper::http::{HeaderValue, Uri};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Method, Request, Response, Server};
use log::info;
use std::convert::Infallible;
use std::net::SocketAddr;

async fn proxy(mut req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let mut response = Response::new(Body::empty());

    let headers = req.headers();
    let host_header = headers.get("host").unwrap();
    let no_proxy = headers.contains_key("x-no-proxy");

    let client = Client::new();

    match (req.method(), req.uri().path(), no_proxy) {
        (&Method::GET, "/status", true) => {
            *response.body_mut() = Body::from("The proxy is running");
        }

        _ => {
            // Proxy to downstream
            let uri = Uri::builder()
                .scheme("http")
                .authority(host_header.to_str().unwrap())
                .path_and_query(req.uri().path())
                .build()
                .unwrap();

            // Simply take the existing request and mutate the uri and headers
            *req.uri_mut() = uri.clone();
            req.headers_mut()
                .insert("x-no-proxy", HeaderValue::from_static("true")); // Avoid loops

            let proxied_resp = client.request(req).await.unwrap();
            info!(
                "Proxied response to: {} | Status: {}",
                uri,
                proxied_resp.status()
            );

            *response.body_mut() = proxied_resp.into_body();
        }
    };
    Ok(response)
}

async fn sigint_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(proxy)) });

    let server = Server::bind(&addr).serve(make_svc);

    let graceful = server.with_graceful_shutdown(sigint_signal());

    if let Err(e) = graceful.await {
        eprintln!("Server error: {}", e);
    }
}
