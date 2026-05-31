use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    env,
    net::{IpAddr, SocketAddr},
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{
        header,
        header::{CONTENT_TYPE, HOST},
        uri::{Authority, Uri},
        HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Version,
    },
    middleware,
    routing::any,
    Router,
};
use axum_server::{tls_rustls::RustlsConfig, Handle};
use env_logger::Env;
use log::{debug, info, warn};
use serde::Deserialize;
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
const HOP_BY_HOP_HEADERS: [HeaderName; 9] = [
    HeaderName::from_static("keep-alive"),
    HeaderName::from_static("proxy-connection"), // non standard, sent by HTTP/1.0 clients
    header::TRANSFER_ENCODING,
    header::TE,
    header::CONNECTION,
    header::TRAILER,
    header::UPGRADE,
    header::PROXY_AUTHORIZATION,
    header::PROXY_AUTHENTICATE,
];

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsConfig {
    cert_path: String,
    key_path: String,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    listen: SocketAddr,
    tls: TlsConfig,
    timeout_ms: Option<u64>,
    drain_timeout_secs: Option<u64>,
    backends: Vec<Backend>,
}

#[derive(Debug, Eq, PartialEq, Deserialize, Clone)]
#[serde(tag = "backend_type", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum Backend {
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
pub(crate) struct BackendState {
    rr_count: AtomicUsize,
}

#[derive(Debug)]
pub(crate) struct RoutingState {
    backends: HashMap<String, BackendState>, // keyed by name, LoadBalanced backends only
}

impl RoutingState {
    fn new(config: &Config) -> RoutingState {
        let mut backends: HashMap<String, BackendState> = HashMap::new();

        for backend in &config.backends {
            if let Backend::LoadBalanced { name, .. } = backend {
                backends.insert(
                    name.clone(),
                    BackendState {
                        rr_count: AtomicUsize::new(0),
                    },
                );
            }
        }
        RoutingState { backends }
    }
}

#[derive(Clone)]
struct ProxyState {
    config: Arc<Config>,
    client: Client,
    routing_state: Arc<RoutingState>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResponseContext {
    backend_location: String,
}

fn validate_config(config: &Config) -> Result<()> {
    let mut seen = HashSet::new();
    for backend in &config.backends {
        let name = backend.name();
        if !seen.insert(name) {
            anyhow::bail!("duplicate backend name: {}", name);
        }
    }
    Ok(())
}

// Not async — uses blocking std::fs I/O which is acceptable as this runs once at startup
fn read_proxy_config_yaml(yaml_path: &str) -> Result<Config> {
    let file = std::fs::File::open(yaml_path)
        .with_context(|| format!("Unable to open config file: {yaml_path}"))?;
    let deserialized: Config = serde_yaml::from_reader(file)?;
    validate_config(&deserialized)?;
    Ok(deserialized)
}

fn remove_hop_by_hop_headers(headers: &mut HeaderMap) {
    // RFC 7230 §6.1: the Connection header value lists additional headers that are
    // hop by hop for this connection only — remove those first, before Connection itself
    // is removed by the fixed list below
    if let Some(connection) = headers.get(header::CONNECTION).cloned() {
        if let Ok(connection_str) = connection.to_str() {
            for name in connection_str.split(',') {
                headers.remove(name.trim());
            }
        }
    }

    // Remove the standard hop by hop headers defined by RFC 7230
    for h in HOP_BY_HOP_HEADERS {
        headers.remove(h);
    }
}

fn adjust_backend_request_headers(
    req: &mut Request<Body>,
    host_authority: &str,
    client_ip: IpAddr,
) {
    // Remove hop by hop headers before forwarding to the backend
    remove_hop_by_hop_headers(req.headers_mut());

    // Rewrite the Host header to the backend address
    req.headers_mut().insert(
        HOST,
        HeaderValue::from_str(host_authority)
            .expect("host_authority from parsed Authority is always a valid HeaderValue"),
    );

    // Mark as already proxied to prevent forwarding loops
    req.headers_mut()
        .insert("x-no-proxy", HeaderValue::from_static("true"));

    // Inform the backend of the original client IP
    let ip_header = HeaderValue::from_str(&client_ip.to_string())
        .expect("IP address is always a valid HeaderValue");
    req.headers_mut().insert("x-forwarded-for", ip_header);
}

fn adjust_backend_response_headers(res: &mut Response<Body>) {
    // Remove hop by hop headers from the backend response before returning to the client
    remove_hop_by_hop_headers(res.headers_mut());
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
    if host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok()
    {
        return None;
    }
    Some(host)
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    let mut response = Response::new(Body::from(message.to_owned()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

async fn proxy_handler(
    State(state): State<ProxyState>,
    ConnectInfo(client_addr): ConnectInfo<SocketAddr>,
    mut req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let ProxyState {
        config,
        client,
        routing_state,
    } = state;
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
                StatusCode::HTTP_VERSION_NOT_SUPPORTED,
                &format!("Unsupported HTTP version: {:?}", req.version()),
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

    let response = match (req.method(), req.uri().path(), no_proxy, host_authority) {
        // Proxy internal endpoints
        (&Method::GET, "/status", true, _) => Response::new(Body::from("The proxy is running")),
        (&Method::GET, "/metrics", true, _) => match encode_metrics() {
            Ok(encoded_metrics) => {
                let mut response = Response::new(Body::from(encoded_metrics));
                response.headers_mut().insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                response
            }
            Err(e) => {
                warn!("Error encoding metrics: {e}");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Error encoding metrics: {e}"),
                )
            }
        },

        // x-no-proxy request to an unknown internal path
        (_, _, true, _) => error_response(StatusCode::NOT_FOUND, ""),

        // A non internal request, but the host header has not been defined
        (_, _, false, None) => {
            debug!("Host header not defined");
            error_response(StatusCode::NOT_FOUND, "Host header not defined")
        }

        // Proxy the request
        (_, _, false, Some(host)) => {
            debug!("Standard request proxy");
            let backend_location = router(&config.backends, routing_state.clone(), &host);

            match backend_location {
                None => error_response(StatusCode::NOT_FOUND, ""),
                Some(backend_location) => {
                    // Backend connections are plain HTTP — TLS is terminated at the proxy
                    let scheme = "http";

                    // Default to "/" if the URI has no path component
                    let path_and_query = req
                        .uri()
                        .path_and_query()
                        .map(|pq| pq.as_str())
                        .unwrap_or("/");

                    let uri = match Uri::builder()
                        .scheme(scheme)
                        .authority(backend_location.as_str())
                        .path_and_query(path_and_query)
                        .build()
                    {
                        Ok(uri) => uri,
                        Err(e) => {
                            warn!("Failed to build backend URI: {e}");
                            return Ok(error_response(StatusCode::INTERNAL_SERVER_ERROR, ""));
                        }
                    };

                    // Simply take the existing request and mutate the uri and headers
                    debug!("Proxying request to: {}", uri);
                    *req.uri_mut() = uri;
                    adjust_backend_request_headers(&mut req, &host, client_addr.ip());

                    // Downgrade to HTTP/1.1 for backend connections
                    *req.version_mut() = Version::HTTP_11;
                    let mut response = client.make_request(req).await;
                    adjust_backend_response_headers(&mut response);
                    debug!(
                        "Proxied response | Status: {} | Headers: {:?}",
                        response.status(),
                        response.headers()
                    );
                    response
                        .extensions_mut()
                        .insert(ResponseContext { backend_location });
                    response
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

pub async fn run_server(config_path: &str) -> Result<()> {
    let _ = env_logger::Builder::from_env(Env::default().default_filter_or("info")).try_init();

    let config = read_proxy_config_yaml(config_path)?;

    let listen_address = config.listen;

    let client = client::Client::new(config.timeout_ms);

    let routing_state = Arc::new(RoutingState::new(&config));

    let config = Arc::new(config);

    let current_dir = env::current_dir().context("Unable to determine current directory")?;
    let tls_config = RustlsConfig::from_pem_file(
        current_dir.join(&config.tls.cert_path),
        current_dir.join(&config.tls.key_path),
    )
    .await
    .context("Failed to load TLS config")?;

    for backend in &config.backends {
        info!("backend: {backend}");
    }

    let drain_timeout_secs = config.drain_timeout_secs.unwrap_or(10);

    let proxy_state = ProxyState {
        config,
        client,
        routing_state,
    };

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
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(drain_timeout_secs)));
    });

    info!("proxy listening on {}", listen_address);

    axum_server::bind_rustls(listen_address, tls_config)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
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
        let data = read_proxy_config_yaml("tests/config.yaml").unwrap();
        assert_eq!(data.backends[0].name(), "test.home");
    }

    #[test]
    fn test_read_config_yaml_unknown_field_errors() {
        // serde(deny_unknown_fields) — a typo in any config key should be an error,
        // not silently ignored (e.g. "timout" instead of "timeout")
        let yaml = r#"
listen: "127.0.0.1:4000"
tls:
  key_path: "tests/self-signed-cert/test.key"
  cert_path: "tests/self-signed-cert/test.crt"
timout: 500
backends: []
"#;
        let result: Result<Config, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_config_yaml_duplicate_backend_name_errors() {
        let yaml = r#"
listen: "127.0.0.1:4000"
tls:
  key_path: "tests/self-signed-cert/test.key"
  cert_path: "tests/self-signed-cert/test.crt"
backends:
  - name: "test.home"
    backend_type: "single"
    location: "127.0.0.1:8000"
  - name: "test.home"
    backend_type: "single"
    location: "127.0.0.1:8001"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_adjust_backend_request_headers() {
        let mut req = Request::new(Body::from("test"));
        req.headers_mut().insert(HOST, "test_host".parse().unwrap());
        req.headers_mut()
            .insert(PROXY_AUTHENTICATE, "true".parse().unwrap());
        adjust_backend_request_headers(&mut req, "test", "127.0.0.1".parse().unwrap());
        assert_eq!(req.headers().iter().count(), 3);
        assert!(req.headers().contains_key(HOST));
        assert!(req.headers().contains_key("x-no-proxy"));
        assert!(req.headers().contains_key("x-forwarded-for"));
        assert!(!req.headers().contains_key(PROXY_AUTHENTICATE));
    }

    #[test]
    fn test_adjust_backend_response_headers() {
        let mut res = Response::new(Body::empty());
        res.headers_mut()
            .insert(header::TRANSFER_ENCODING, "chunked".parse().unwrap());
        res.headers_mut()
            .insert(header::CONNECTION, "keep-alive".parse().unwrap());
        res.headers_mut()
            .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
        adjust_backend_response_headers(&mut res);
        assert!(!res.headers().contains_key(header::TRANSFER_ENCODING));
        assert!(!res.headers().contains_key(header::CONNECTION));
        assert!(res.headers().contains_key(header::CONTENT_TYPE));
    }

    #[test]
    fn test_remove_hop_by_hop_connection_header_names() {
        // Connection header may list one or more hop by hop headers as a comma separated list
        // only those headers should be removed, unlisted headers must survive
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, "x-first, x-second".parse().unwrap());
        headers.insert("x-first", "a".parse().unwrap());
        headers.insert("x-second", "b".parse().unwrap());
        headers.insert("x-third", "c".parse().unwrap()); // not listed — must survive
        remove_hop_by_hop_headers(&mut headers);
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-first"));
        assert!(!headers.contains_key("x-second"));
        assert!(headers.contains_key("x-third"));
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
        let response = error_response(StatusCode::BAD_REQUEST, "test error");
        assert_eq!(response.status(), 400);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body, "test error");
    }
}
