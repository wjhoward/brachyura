use anyhow::{Error, Result};
use axum::{
    extract::Extension,
    http::{uri::Uri, Request, Response},
    routing::get,
    Router,
};
use axum_server::tls_rustls::RustlsConfig;
use env_logger::Env;
use hyper::http::{header, header::HeaderName, HeaderValue};
use hyper::{Body, Method, StatusCode, Version};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::env;
use std::net::{SocketAddr, SocketAddrV4, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Instant;

mod client;
mod metrics;
mod routing;
use crate::client::Client;
use crate::metrics::{encode_metrics, record_metrics};
use crate::routing::router;

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

async fn read_proxy_config_yaml(yaml_path: String) -> Result<Config, serde_yaml::Error> {
    let deserialized: Config =
        serde_yaml::from_reader(std::fs::File::open(yaml_path).expect("Unable to read config"))?;
    Ok(deserialized)
}

async fn adjust_proxied_headers(req: &mut Request<Body>) -> Result<(), Error> {
    // Adjust headers for a request which is being proxied downstream

    // Remove hop by hop headers
    for h in HOP_BY_HOP_HEADERS {
        req.headers_mut().remove(h);
    }

    // Append a no-proxy header to avoid loops
    req.headers_mut()
        .insert("x-no-proxy", HeaderValue::from_static("true"));

    Ok(())
}

fn get_host_header(req: &Request<Body>) -> Result<&str, Error> {
    match req.version() {
        Version::HTTP_09 | Version::HTTP_10 | Version::HTTP_11 => Ok(req
            .headers()
            .get("host")
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Unable to parse host header, version: {:?}",
                    req.version()
                ))
            })?
            .to_str()?),
        _ => Ok(req
            .uri()
            .authority()
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Unable to parse host header, version: {:?}",
                    req.version()
                ))
            })
            .unwrap()
            .as_ref()),
    }
}

fn host_header_set(host_header: String) -> bool {
    // For HTTP1, If the host header is not an IP address
    // we can probably assume its been set manually
    host_header.to_socket_addrs().is_err()
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
    let start = Instant::now();
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

    // Extract the host header
    let host_header_str = match get_host_header(&req) {
        Ok(host_header_str) => host_header_str,
        Err(e) => {
            return Ok(bad_request_handler(
                response,
                format!("Unable to parse host header: {:?}", e),
            ))
        }
    };
    let host_header_set = host_header_set(host_header_str.to_string());

    let no_proxy = req.headers().contains_key("x-no-proxy");

    match (req.method(), req.uri().path(), no_proxy, host_header_set) {
        // Proxy internal endpoints
        (&Method::GET, "/status", true, false) => {
            *response.body_mut() = Body::from("The proxy is running");
        }
        (&Method::GET, "/metrics", true, false) => match encode_metrics() {
            Ok(encoded_metrics) => {
                *response.body_mut() = Body::from(encoded_metrics);
            }
            Err(e) => {
                warn!("Error encoding metrics: {e}");
                *response.body_mut() = Body::from(format!("Error encoding metrics: {e}"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            }
        },

        // A non internal request, but the host header has not been defined
        (_, _, false, false) => {
            info!("Host header not defined");
            *response.body_mut() = Body::from("Host header not defined");
            *response.status_mut() = StatusCode::NOT_FOUND;
        }

        // Proxy the request
        _ => {
            debug!("Standard request proxy");

            let backend_location = router(
                &proxy_config.config.backends,
                proxy_state.clone(),
                host_header_str,
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
                        .path_and_query(req.uri().path())
                        .build()
                        .expect("Unable to extract URI");

                    // Simply take the existing request and mutate the uri and headers
                    *req.uri_mut() = uri.clone();
                    adjust_proxied_headers(&mut req)
                        .await
                        .expect("Unable to adjust headers");

                    // If the backend scheme is http, adjust the original request HTTP version to 1
                    // (It seems that the HTTP2 implementation requires TLS)
                    if scheme == "http" {
                        *req.version_mut() = Version::HTTP_11;
                    }

                    response = proxy_config.client.make_request(req).await;
                    info!(
                        "Proxied response from: {} | Status: {}",
                        uri,
                        response.status()
                    );
                    // Record metrics
                    if let Err(e) = record_metrics(&response, backend_location, start.elapsed()) {
                        warn!("Error recording metrics: {e}")
                    };
                }
            }
        }
    };
    debug!("Response headers: {:?}", response.headers());
    Ok(response)
}

pub async fn run_server(config_path: String) {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

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
        .route("/*path", get(proxy_handler))
        .layer(Extension(proxy_config))
        .layer(Extension(proxy_state));

    info!("Reverse proxy listening on {}", listen_address);

    axum_server::bind_rustls(listen_address, tls_config)
        .serve(app.into_make_service())
        .await
        .expect("Error starting axum server");
}

#[cfg(test)]
mod tests {
    use hyper::{
        header::{HOST, PROXY_AUTHENTICATE},
        Body, Request,
    };

    use super::*;

    #[tokio::test]
    async fn test_read_config_yaml() {
        let data = read_proxy_config_yaml("config.yaml".to_string())
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
        adjust_proxied_headers(&mut req).await.unwrap();
        assert!(req.headers().iter().count() == 2);
        assert!(req.headers().contains_key(HOST));
        assert!(req.headers().contains_key("x-no-proxy"));
    }

    #[tokio::test]
    async fn test_host_header_match_proxy_address() {
        // Not host headers
        let host_header_string = String::from("localhost:4000");
        let test_match = host_header_set(host_header_string);
        assert_eq!(test_match, false);

        let host_header_string = String::from("127.0.0.1:4000");
        let test_match = host_header_set(host_header_string);
        assert_eq!(test_match, false);

        let host_header_string = String::from("[::1]:4000");
        let test_match = host_header_set(host_header_string);
        assert_eq!(test_match, false);

        let host_header_string = String::from("192.168.1.100:1");
        let test_match = host_header_set(host_header_string);
        assert_eq!(test_match, false);

        // Is a host header
        let host_header_string = String::from("test.home");
        let test_match = host_header_set(host_header_string);
        assert_eq!(test_match, true);
    }
    #[tokio::test]
    async fn test_get_host_header() {
        // HTTP 1
        let request = Request::builder()
            .method("GET")
            .version(Version::HTTP_10)
            .uri("https://localhost:4000/test")
            .header(HOST, "test.home")
            .body(Body::from("test"))
            .unwrap();
        let host_header = get_host_header(&request);
        assert_eq!(host_header.unwrap(), "test.home");

        //TODO Test and HTTP2 request
    }
}
