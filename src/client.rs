use hyper::{client::HttpConnector, http::StatusCode, Body, Request, Response};
use log::info;
type HttpClient = hyper::client::Client<HttpConnector, Body>;

pub struct Client {
    client: HttpClient,
}

impl Client {
    pub fn new() -> Client {
        let client = HttpClient::new();
        Client { client }
    }

    pub async fn make_request(&self, req: Request<Body>) -> Response<Body> {
        // TODO add timeout
        match self.client.request(req).await {
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

                // Construct the error Response
                let mut response = Response::new(error_string.into());
                *response.status_mut() = error_status;
                response
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn test_client_make_request_503() {
        let client = Client::new();
        let uri = "http://localhost:10001/"; // A host/port that doesn't exist
        let mut request = Request::new(Body::empty());
        *request.uri_mut() = uri.parse().unwrap();
        let response = client.make_request(request).await;
        assert_eq!(response.status(), 503);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(body, "Cannot connect to backend");
    }
}
