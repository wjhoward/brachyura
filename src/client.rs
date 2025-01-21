use std::time::Duration;

use axum::{
    body::Body,
    extract::Request,
    response::{IntoResponse, Response},
};
use hyper::StatusCode;
use hyper_util::{client::legacy::connect::HttpConnector, rt::TokioExecutor};
use log::info;
use tokio::time::timeout;

type HttpClient = hyper_util::client::legacy::Client<HttpConnector, Body>;

pub struct Client {
    client: HttpClient,
    timeout: Option<u64>,
}

impl Client {
    pub fn new(timeout: Option<u64>) -> Client {
        let client: HttpClient =
            hyper_util::client::legacy::Client::<(), ()>::builder(TokioExecutor::new())
                .build(HttpConnector::new());
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
                Ok(response) => response.into_response(),
                Err(e) => {
                    let error_string;
                    let error_status: StatusCode;
                    if e.is_connect() {
                        error_string = "Cannot connect to backend";
                        error_status = StatusCode::SERVICE_UNAVAILABLE;
                    } else {
                        error_string = "Unhandled error, see logs";
                        error_status = StatusCode::INTERNAL_SERVER_ERROR;
                        info!("Unhandled error: {:?}", e);
                    }
                    (error_status, error_string).into_response()
                }
            },
            Err(_) => (StatusCode::GATEWAY_TIMEOUT, "Request timeout").into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

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
        assert_eq!(response.status(), 504);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body, "Request timeout");
    }
}
