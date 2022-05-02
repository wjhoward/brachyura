use hyper::http::{HeaderValue, Uri};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Method, Request, Response, Server, StatusCode};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Backend {
    name: Option<String>,
    location: Option<String>,
    #[serde(flatten)]
    extras: HashMap<String, String>,
}

async fn proxy(mut req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let mut response = Response::new(Body::empty());

    let headers = req.headers();
    let host_header = headers.get("host").unwrap();
    let no_proxy = headers.contains_key("x-no-proxy");

    let client = Client::new();

    debug!(
        "Request method: {} host: {:?} uri: {} headers: {:?}",
        req.method(),
        host_header,
        req.uri(),
        req.headers()
    );

    match (req.method(), req.uri().path(), no_proxy) {
        (&Method::GET, "/status", true) => {
            *response.body_mut() = Body::from("The proxy is running");
        }

        _ => {
            let proxy_config = read_config_yaml("config.yaml".to_string())
                .await
                .expect("Error loading yaml config");
            let host_header_str = host_header.to_str().expect("Unable to parse host header");

            let mut backend_location = None;
            for backend in proxy_config {
                if backend.name.is_some() & backend.location.is_some()
                    && *host_header_str == backend.name.unwrap()
                {
                    backend_location = backend.location;
                    break;
                }
            }
            if backend_location.is_none() {
                *response.status_mut() = StatusCode::NOT_FOUND;
            } else {
                // Proxy to backend
                let uri = Uri::builder()
                    .scheme("http")
                    .authority(backend_location.unwrap())
                    .path_and_query(req.uri().path())
                    .build()
                    .unwrap();

                // Simply take the existing request and mutate the uri and headers
                *req.uri_mut() = uri.clone();
                req.headers_mut()
                    .insert("x-no-proxy", HeaderValue::from_static("true")); // Avoid loops

                response = client.request(req).await.unwrap();
                info!(
                    "Proxied response from: {} | Status: {}",
                    uri,
                    response.status()
                );
            }
        }
    };
    debug!("Response headers: {:?}", response.headers());
    Ok(response)
}

async fn sigint_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
}

async fn read_config_yaml(yaml_path: String) -> Result<Vec<Backend>, serde_yaml::Error> {
    let deserialized: Vec<Backend> =
        serde_yaml::from_reader(std::fs::File::open(yaml_path).unwrap())?;
    Ok(deserialized)
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

#[cfg(test)]
mod tests {
    use crate::read_config_yaml;

    #[tokio::test]
    async fn test_read_config_yaml() {
        let data = read_config_yaml("config.yaml".to_string()).await.unwrap();
        assert_eq!(data[0].name.as_ref().unwrap(), &String::from("test.home"));
    }
}
