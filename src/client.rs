use hyper::{client::HttpConnector, http::StatusCode, Body, Request, Response};
use log::info;
use std::time::Duration;
use tokio::time::timeout;
type HttpClient = hyper::client::Client<HttpConnector, Body>;

pub struct Client {
    client: HttpClient,
    timeout: Option<u64>,
}

impl Client {
    pub fn new(timeout: Option<u64>) -> Client {
        let client = HttpClient::new();
        Client { client, timeout }
    }

    pub async fn make_request(&self, req: Request<Body>) -> Response<Body> {
        match timeout(
            Duration::from_millis(self.timeout.unwrap_or(60)),
            self.client.request(req),
        )
        .await
        {
            Ok(result) => match result {
                Ok(response) => response,
                Err(e) => {
                    let error_string;
                    let error_status;
                    if e.is_connect() {
                        error_string = "Cannot connect to backend";
                        error_status = StatusCode::SERVICE_UNAVAILABLE;
                    } else if e.is_timeout() {
                        error_string = "Connection timeout";
                        error_status = StatusCode::GATEWAY_TIMEOUT;
                    } else {
                        error_string = "Unhandled error, see logs";
                        error_status = StatusCode::INTERNAL_SERVER_ERROR;
                        info!("Unhandled error: {:?}", e);
                    }
                    let mut response = Response::new(error_string.into());
                    *response.status_mut() = error_status;
                    response
                }
            },
            Err(_) => {
                let mut response = Response::new("Request timeout".into());
                *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                response
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_client_make_request_ok() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let client = Client::new(Some(500));
        let mut request = Request::new(Body::empty());
        *request.uri_mut() = format!("{}/ok", &mock_server.uri()).parse().unwrap();
        let response = client.make_request(request).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn test_client_make_request_timeout() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/delay"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1000)))
            .mount(&mock_server)
            .await;

        let client = Client::new(Some(500)); // This will timeout before the mock server responds
        let mut request = Request::new(Body::empty());
        *request.uri_mut() = format!("{}/delay", &mock_server.uri()).parse().unwrap();
        let response = client.make_request(request).await;
        assert_eq!(response.status(), 503);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(body, "Request timeout");
    }
}
