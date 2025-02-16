use std::{
    collections::HashMap,
    convert::Infallible,
    env,
    net::{IpAddr, SocketAddr, SocketAddrV4},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Error, Result};
use axum::{
    body::Body,
    extract::Extension,
    http::{uri::Uri, HeaderValue, Method, Request, Response, StatusCode, Version},
    middleware,
    routing::get,
    Router,
};
use axum_server::tls_rustls::RustlsConfig;
use env_logger::Env;
use hyper::http::{
    header,
    header::{CONTENT_TYPE, HOST},
    HeaderName,
};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

mod client;
mod metrics;
mod routing;
use crate::{
    client::Client,
    metrics::{encode_metrics, record_metrics},
    routing::router,
};

#[allow(clippy::declare_interior_mutable_const)]
const HOP_BY_HOP_HEADERS: [HeaderName; 8] = [
    HeaderName::from_static("keep-alive"),
    header::TRANSFER_ENCODING,
    header::TE,
    header::CONNECTION,
    header::TRAILER,
    header::UPGRADE,
    header::PROXY_AUTHORIZATION,
    header::PROXY_AUTHENTICATE,
];

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Config {
    listen: SocketAddrV4,
    tls: HashMap<String, String>,
    timeout: Option<u64>,
    backends: Vec<Backend>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Clone)]
pub struct Backend {
    name: Option<String>,
    location: Option<String>,
    backend_type: Option<String>,
    locations: Option<Vec<String>>,
    #[serde(flatten)]
    extras: HashMap<String, String>,
}

#[derive(Debug)]
struct ProxyConfig {
    config: Config,
    client: Client,
}
impl ProxyConfig {
    fn new(config: Config, client: Client) -> ProxyConfig {
        ProxyConfig { config, client }
    }
}

#[derive(Debug)]
pub struct BackendState {
    rr_count: isize, // Round robin counter
}
pub struct ProxyState {
    backends: HashMap<String, Option<BackendState>>,
}

impl ProxyState {
    fn new(config: &Config) -> ProxyState {
        let mut backends: HashMap<String, Option<BackendState>> = HashMap::new();

        for backend_config in &config.backends {
            if backend_config.backend_type.as_deref() == Some("loadbalanced")
                && backend_config.name.is_some()
            {
                backends.insert(
                    backend_config.name.clone().unwrap(),
                    Some(BackendState { rr_count: -1 }),
                );
            } else if backend_config.name.is_some() {
                backends.insert(backend_config.name.clone().unwrap(), None);
            }
        }
        ProxyState { backends }
    }
}

#[derive(Debug, Clone)]
pub struct ResponseContext {
    backend_location: String,
}

async fn read_proxy_config_yaml(yaml_path: String) -> Result<Config, serde_yaml::Error> {
    let deserialized: Config =
        serde_yaml::from_reader(std::fs::File::open(yaml_path).expect("Unable to read config"))?;
    Ok(deserialized)
}

async fn adjust_proxied_headers(
    req: &mut Request<Body>,
    host_authority: Option<String>,
) -> Result<(), Error> {
    // Adjust headers for a request which is being proxied downstream

    // Remove hop by hop headers
    for h in HOP_BY_HOP_HEADERS {
        req.headers_mut().remove(h.to_string());
    }

    //Append a host header
    req.headers_mut().insert(
        HOST,
        HeaderValue::from_str(&host_authority.context("unexpected missing host_authority")?)?,
    );

    // Append a no-proxy header to avoid loops
    req.headers_mut()
        .insert("x-no-proxy", HeaderValue::from_static("true"));

    Ok(())
}

fn get_host(req: &Request<Body>) -> Option<String> {
    // Look for a host header first, otherwise fallback to checking the HTTP Authority (http2+)
    let get_host_header = req.headers().get("host");
    let host = match get_host_header {
        Some(header) => header.to_str().ok().map(|s| s.to_string()),
        _ => req.uri().authority().map(|authority| authority.to_string()),
    };
    // Parse the HTTP authority, removing port numbers
    let ip_or_host = host.clone().unwrap_or_else(|| "".to_string());
    let ip_or_host_no_port = ip_or_host.split(":").next().map(|s| s.to_string());

    // If the authority is "localhost" or an IP address, it's not a host for the purpose of proxying
    if ip_or_host_no_port == Some("localhost".to_string()) {
        return None;
    }
    let ipv4: Option<IpAddr> = ip_or_host_no_port
        .clone()
        .unwrap_or_else(|| "".to_string())
        .parse()
        .ok();
    if ipv4.is_some() {
        return None;
    }
    ip_or_host_no_port
}

fn bad_request_handler(mut response: Response<Body>, message: String) -> Response<Body> {
    *response.body_mut() = Body::from(message);
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
}

async fn proxy_handler(
    Extension(proxy_config): Extension<Arc<ProxyConfig>>,
    Extension(proxy_state): Extension<Arc<Mutex<ProxyState>>>,
    mut req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let mut response = Response::new(Body::empty());

    debug!(
        "Request version: {:?} method: {} uri: {} headers: {:?}",
        req.version(),
        req.method(),
        req.uri(),
        req.headers()
    );

    // Currently only testing HTTP1/2 support
    match req.version() {
        Version::HTTP_10 | Version::HTTP_11 | Version::HTTP_2 => {}
        _ => {
            return Ok(bad_request_handler(
                response,
                format!("Unsupported HTTP version: {:?}", req.version()),
            ))
        }
    }

    // Extract the host header / authority
    let host_authority = get_host(&req);

    let no_proxy = req.headers().contains_key("x-no-proxy");

    debug!(
        "no_proxy header: {}, host header: {:?}",
        no_proxy,
        host_authority.clone()
    );

    match (
        req.method(),
        req.uri().path(),
        no_proxy,
        host_authority.clone(),
    ) {
        // Proxy internal endpoints
        (&Method::GET, "/status", true, _) => {
            *response.body_mut() = Body::from("The proxy is running");
        }
        (&Method::GET, "/metrics", true, _) => match encode_metrics() {
            Ok(encoded_metrics) => {
                *response.body_mut() = Body::from(encoded_metrics);
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, "text/plain; charset=utf-8".parse().unwrap());
            }
            Err(e) => {
                warn!("Error encoding metrics: {e}");
                *response.body_mut() = Body::from(format!("Error encoding metrics: {e}"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            }
        },

        // A non internal request, but the host header has not been defined
        (_, _, false, None) => {
            debug!("Host header not defined");
            *response.body_mut() = Body::from("Host header not defined");
            *response.status_mut() = StatusCode::NOT_FOUND;
        }

        // Proxy the request
        _ => {
            debug!("Standard request proxy");
            let backend_location = router(
                &proxy_config.config.backends,
                proxy_state.clone(),
                host_authority
                    .clone()
                    .expect("unexpected missing host_authority"),
            );

            match backend_location {
                None => {
                    *response.status_mut() = StatusCode::NOT_FOUND;
                }
                Some(backend_location) => {
                    // Proxy to backend

                    // Scheme currently hardcoded to http (given this is a TLS terminating proxy)
                    let scheme = "http";

                    let uri = Uri::builder()
                        .scheme(scheme)
                        .authority(backend_location.clone())
                        .path_and_query(
                            req.uri()
                                .path_and_query()
                                .expect("Unable to extract path and query")
                                .clone(),
                        )
                        .build()
                        .expect("Unable to extract URI");

                    // Simply take the existing request and mutate the uri and headers
                    *req.uri_mut() = uri.clone();
                    adjust_proxied_headers(&mut req, host_authority)
                        .await
                        .expect("Unable to adjust headers");

                    // If the backend scheme is http, adjust the original request HTTP version to 1
                    // (It seems that the HTTP2 implementation requires TLS)
                    if scheme == "http" {
                        *req.version_mut() = Version::HTTP_11;
                    }
                    response = proxy_config.client.make_request(req).await;
                    debug!(
                        "Proxied response from: {} | Status: {} | Response headers: {:?}",
                        uri,
                        response.status(),
                        response.headers()
                    );
                    response
                        .extensions_mut()
                        .insert(ResponseContext { backend_location });
                }
            }
        }
    };
    Ok(response)
}

pub async fn run_server(config_path: String) {
    let _ = env_logger::Builder::from_env(Env::default().default_filter_or("info")).try_init();

    let config = read_proxy_config_yaml(config_path)
        .await
        .expect("Error loading yaml proxy config");

    let listen_address = SocketAddr::from(config.listen);

    let client = client::Client::new(config.timeout);

    let proxy_state = Arc::new(Mutex::new(ProxyState::new(&config)));

    let proxy_config = Arc::new(ProxyConfig::new(config, client));

    let current_dir = env::current_dir().unwrap();
    let tls_config = RustlsConfig::from_pem_file(
        current_dir.join(
            proxy_config
                .config
                .tls
                .get("cert_path")
                .expect("Unable to read cert_path"),
        ),
        current_dir.join(
            proxy_config
                .config
                .tls
                .get("key_path")
                .expect("Unable to read key_path"),
        ),
    )
    .await
    .expect("TLS config error");

    let app = Router::new()
        .route(
            "/",
            get(proxy_handler).post(proxy_handler).put(proxy_handler),
        )
        .route(
            "/{*wildcard}",
            get(proxy_handler).post(proxy_handler).put(proxy_handler),
        )
        .route_layer(middleware::from_fn(record_metrics))
        .layer(Extension(proxy_config))
        .layer(Extension(proxy_state));

    info!("proxy listening on {}", listen_address);

    axum_server::bind_rustls(listen_address, tls_config)
        .serve(app.into_make_service())
        .await
        .expect("Error starting axum server");
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use hyper::{
        header::{HOST, PROXY_AUTHENTICATE},
        Request,
    };

    use super::*;

    #[tokio::test]
    async fn test_read_config_yaml() {
        let data = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        assert_eq!(
            data.backends[0].name.as_ref().unwrap(),
            &String::from("test.home")
        );
    }

    #[tokio::test]
    async fn test_adjust_proxied_headers() {
        let mut req = Request::new(Body::from("test"));
        req.headers_mut().insert(HOST, "test_host".parse().unwrap());
        req.headers_mut()
            .insert(PROXY_AUTHENTICATE, "true".parse().unwrap());
        adjust_proxied_headers(&mut req, Some("test".to_string()))
            .await
            .unwrap();
        assert!(req.headers().iter().count() == 2);
        assert!(req.headers().contains_key(HOST));
        assert!(req.headers().contains_key("x-no-proxy"));
    }

    #[tokio::test]
    async fn test_get_host_http1() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_10)
            .uri("https://localhost:4000/test")
            .header(HOST, "test.home")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host.unwrap(), "test.home");
    }

    #[tokio::test]
    async fn test_get_host_http1_none() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_10)
            .uri("https://localhost:4000/test")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host, None);
    }

    #[tokio::test]
    async fn test_get_host_http2() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_2)
            .uri("https://localhost:4000/test")
            .header(HOST, "test.home")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host.unwrap(), "test.home");
    }

    #[tokio::test]
    async fn test_get_host_http2_none() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_2)
            .uri("https://localhost:4000/test")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host, None);
    }

    #[tokio::test]
    async fn test_bad_request_handler() {
        let original_response = Response::new(Body::from("test"));
        let response = bad_request_handler(original_response, "test error".to_string());
        assert_eq!(response.status(), 400);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body, "test error");
    }
}
