use std::{
    collections::HashMap,
    convert::Infallible,
    env,
    net::{IpAddr, SocketAddr},
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{uri::{Authority, Uri}, HeaderValue, Method, Request, Response, StatusCode, Version},
    middleware,
    routing::any,
    Router,
};
use axum_server::{tls_rustls::RustlsConfig, Handle};
use env_logger::Env;
use hyper::http::{
    header,
    header::{CONTENT_TYPE, HOST},
    HeaderName,
};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use tokio::signal;
#[cfg(unix)]
use tokio::signal::unix::SignalKind;

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
struct TlsConfig {
    cert_path: String,
    key_path: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Config {
    listen: SocketAddr,
    tls: TlsConfig,
    timeout: Option<u64>,
    backends: Vec<Backend>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Clone)]
#[serde(tag = "backend_type", rename_all = "lowercase")]
pub enum Backend {
    Single {
        name: String,
        location: String,
    },
    LoadBalanced {
        name: String,
        locations: Vec<String>,
    },
}

impl Backend {
    fn name(&self) -> &str {
        match self {
            Backend::Single { name, .. } => name,
            Backend::LoadBalanced { name, .. } => name,
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backend::Single { name, location } => write!(f, "{name} -> {location}"),
            Backend::LoadBalanced { name, locations } => {
                write!(f, "{name} -> [{}] (load balanced)", locations.join(", "))
            }
        }
    }
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
    rr_count: AtomicUsize,
}

#[derive(Debug)]
pub struct RoutingState {
    backends: HashMap<String, Option<BackendState>>,
}

impl RoutingState {
    fn new(config: &Config) -> RoutingState {
        let mut backends: HashMap<String, Option<BackendState>> = HashMap::new();

        for backend in &config.backends {
            match backend {
                Backend::Single { name, .. } => {
                    backends.insert(name.clone(), None);
                }
                Backend::LoadBalanced { name, .. } => {
                    backends.insert(
                        name.clone(),
                        Some(BackendState {
                            rr_count: AtomicUsize::new(0),
                        }),
                    );
                }
            }
        }
        RoutingState { backends }
    }
}

#[derive(Clone)]
struct ProxyState {
    proxy_config: Arc<ProxyConfig>,
    routing_state: Arc<RoutingState>,
}

#[derive(Debug, Clone)]
pub struct ResponseContext {
    backend_location: String,
}

// Not async — uses blocking std::fs I/O which is acceptable as this runs once at startup
fn read_proxy_config_yaml(yaml_path: String) -> Result<Config> {
    let file = std::fs::File::open(&yaml_path)
        .with_context(|| format!("Unable to open config file: {yaml_path}"))?;
    let deserialized: Config = serde_yaml::from_reader(file)?;
    Ok(deserialized)
}

fn adjust_proxied_headers(req: &mut Request<Body>, host_authority: &str) -> Result<()> {
    // Adjust headers for a request which is being proxied downstream

    // Remove hop by hop headers
    for h in HOP_BY_HOP_HEADERS {
        req.headers_mut().remove(h);
    }

    // Append a host header
    req.headers_mut()
        .insert(HOST, HeaderValue::from_str(host_authority)?);

    // Append a no-proxy header to avoid loops
    req.headers_mut()
        .insert("x-no-proxy", HeaderValue::from_static("true"));

    Ok(())
}

/// Extracts the virtual hostname from the request for use in backend routing.
/// Returns None if the request has no usable host, or if the host is an IP address
/// or localhost — indicating the client is addressing the proxy directly rather than
/// a named virtual backend.
fn get_host(req: &Request<Body>) -> Option<String> {
    // Try and parse host header first
    // If not, extract the HTTP authority pseudo header
    // Authority::host() strips the port in both cases
    let host = match req.headers().get("host") {
        Some(header) => header
            .to_str()
            .ok()
            .and_then(|s| s.parse::<Authority>().ok())
            .map(|a| a.host().to_owned()),
        None => req.uri().authority().map(|a| a.host().to_owned()),
    }?; // Returns None if no host header or URI authority is present

    // Filter out localhost and IP addresses — these indicate direct proxy access,
    // not a request intended for a named virtual backend
    if host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    // Strip IPv6 brackets before trying to parse as an IP address
    if host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().is_ok() {
        return None;
    }
    Some(host)
}

fn error_response(mut response: Response<Body>, status: StatusCode, message: String) -> Response<Body> {
    *response.body_mut() = Body::from(message);
    *response.status_mut() = status;
    response
}

async fn proxy_handler(
    State(state): State<ProxyState>,
    mut req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let ProxyState {
        proxy_config,
        routing_state,
    } = state;
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
            return Ok(error_response(
                response,
                StatusCode::HTTP_VERSION_NOT_SUPPORTED,
                format!("Unsupported HTTP version: {:?}", req.version()),
            ))
        }
    }

    // Extract the host header / authority
    let host_authority = get_host(&req);

    let no_proxy = req.headers().contains_key("x-no-proxy");

    debug!(
        "no_proxy header: {}, host header: {:?}",
        no_proxy, host_authority
    );

    match (req.method(), req.uri().path(), no_proxy, host_authority) {
        // Proxy internal endpoints
        (&Method::GET, "/status", true, _) => {
            *response.body_mut() = Body::from("The proxy is running");
        }
        (&Method::GET, "/metrics", true, _) => match encode_metrics() {
            Ok(encoded_metrics) => {
                *response.body_mut() = Body::from(encoded_metrics);
                response.headers_mut().insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
            }
            Err(e) => {
                warn!("Error encoding metrics: {e}");
                *response.body_mut() = Body::from(format!("Error encoding metrics: {e}"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            }
        },

        // x-no-proxy request to an unknown internal path
        (_, _, true, _) => {
            *response.status_mut() = StatusCode::NOT_FOUND;
        }

        // A non internal request, but the host header has not been defined
        (_, _, false, None) => {
            debug!("Host header not defined");
            *response.body_mut() = Body::from("Host header not defined");
            *response.status_mut() = StatusCode::NOT_FOUND;
        }

        // Proxy the request
        (_, _, false, Some(host)) => {
            debug!("Standard request proxy");
            let backend_location = router(
                &proxy_config.config.backends,
                routing_state.clone(),
                host.clone(),
            );

            match backend_location {
                None => {
                    *response.status_mut() = StatusCode::NOT_FOUND;
                }
                Some(backend_location) => {
                    // Proxy to backend

                    // Scheme currently hardcoded to http (given this is a TLS terminating proxy)
                    let scheme = "http";

                    // Default to "/" if the URI has no path component
                    let path_and_query = req
                        .uri()
                        .path_and_query()
                        .map(|pq| pq.as_str())
                        .unwrap_or("/");

                    let uri = match Uri::builder()
                        .scheme(scheme)
                        .authority(backend_location.clone())
                        .path_and_query(path_and_query)
                        .build()
                    {
                        Ok(uri) => uri,
                        Err(e) => {
                            warn!("Failed to build backend URI: {e}");
                            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                            return Ok(response);
                        }
                    };

                    // Simply take the existing request and mutate the uri and headers
                    *req.uri_mut() = uri.clone();
                    if let Err(e) = adjust_proxied_headers(&mut req, &host) {
                        warn!("Failed to adjust proxy headers: {e}");
                        *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                        return Ok(response);
                    }

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

async fn shutdown_signal() {
    // Wait for SIGINT or SIGTERM
    let sigint = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install SIGINT handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        signal::unix::signal(SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(unix)]
    tokio::select! {
        _ = sigint => {},
        _ = sigterm => {},
    }

    #[cfg(not(unix))]
    sigint.await;
}

pub async fn run_server(config_path: String) -> Result<()> {
    let _ = env_logger::Builder::from_env(Env::default().default_filter_or("info")).try_init();

    let config = read_proxy_config_yaml(config_path)?;

    let listen_address = config.listen;

    let client = client::Client::new(config.timeout);

    let routing_state = Arc::new(RoutingState::new(&config));

    let proxy_config = Arc::new(ProxyConfig::new(config, client));

    let proxy_state = ProxyState {
        proxy_config: proxy_config.clone(),
        routing_state,
    };

    let current_dir = env::current_dir().context("Unable to determine current directory")?;
    let tls_config = RustlsConfig::from_pem_file(
        current_dir.join(&proxy_config.config.tls.cert_path),
        current_dir.join(&proxy_config.config.tls.key_path),
    )
    .await
    .context("Failed to load TLS config")?;

    for backend in &proxy_config.config.backends {
        info!("backend: {backend}");
    }

    let app = Router::new()
        .route("/", any(proxy_handler))
        .route("/{*wildcard}", any(proxy_handler))
        .route_layer(middleware::from_fn(record_metrics))
        .with_state(proxy_state);

    let handle = Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        info!("Shutdown signal received, draining in flight requests");
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(30)));
    });

    info!("proxy listening on {}", listen_address);

    axum_server::bind_rustls(listen_address, tls_config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .context("Server error")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use hyper::{
        header::{HOST, PROXY_AUTHENTICATE},
        Request,
    };

    use super::*;

    #[test]
    fn test_read_config_yaml() {
        let data = read_proxy_config_yaml("tests/config.yaml".to_string()).unwrap();
        assert_eq!(data.backends[0].name(), "test.home");
    }

    #[test]
    fn test_adjust_proxied_headers() {
        let mut req = Request::new(Body::from("test"));
        req.headers_mut().insert(HOST, "test_host".parse().unwrap());
        req.headers_mut()
            .insert(PROXY_AUTHENTICATE, "true".parse().unwrap());
        adjust_proxied_headers(&mut req, "test").unwrap();
        assert_eq!(req.headers().iter().count(), 2);
        assert!(req.headers().contains_key(HOST));
        assert!(req.headers().contains_key("x-no-proxy"));
    }

    #[test]
    fn test_get_host_http1() {
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

    #[test]
    fn test_get_host_http1_none() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_10)
            .uri("https://localhost:4000/test")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host, None);
    }

    #[test]
    fn test_get_host_http2() {
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

    #[test]
    fn test_get_host_http2_none() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_2)
            .uri("https://localhost:4000/test")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host, None);
    }

    #[test]
    fn test_get_host_http1_ipv6_ip() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_11)
            .uri("https://[::1]:4000/test")
            .header(HOST, "[::1]:4000")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host, None);
    }

    #[test]
    fn test_get_host_http2_ipv6_ip() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_2)
            .uri("https://[::1]:4000/test")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host, None);
    }

    #[test]
    fn test_get_host_http1_with_port() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_11)
            .uri("https://localhost:4000/test")
            .header(HOST, "test.home:8080")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host.unwrap(), "test.home");
    }

    #[test]
    fn test_get_host_http2_with_port() {
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_2)
            .uri("https://localhost:4000/test")
            .header(HOST, "test.home:8080")
            .body(Body::from("test"))
            .unwrap();
        let host = get_host(&request);
        assert_eq!(host.unwrap(), "test.home");
    }

    #[tokio::test]
    async fn test_error_response() {
        let original_response = Response::new(Body::from("test"));
        let response = error_response(original_response, StatusCode::BAD_REQUEST, "test error".to_string());
        assert_eq!(response.status(), 400);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body, "test error");
    }
}
