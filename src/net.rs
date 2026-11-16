//! Network helpers for idempotent CNB requests.

use std::error::Error;
use std::future::Future;
use std::time::Duration;

use reqwest::{Response, StatusCode};

const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(300),
    Duration::from_millis(800),
    Duration::from_millis(1600),
];

/// A fully buffered response used by retryable idempotent requests.
#[derive(Debug)]
pub struct BufferedResponse {
    pub status: StatusCode,
    pub text: String,
}

/// Sends an idempotent request with retries for transport failures and
/// transient gateway responses.
pub async fn send_idempotent<F, Fut>(mut send: F) -> Result<BufferedResponse, reqwest::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Response, reqwest::Error>>,
{
    let mut retry = 0;
    loop {
        match send().await {
            Ok(response)
                if is_transient_status(response.status()) && retry < RETRY_DELAYS.len() =>
            {
                tokio::time::sleep(RETRY_DELAYS[retry]).await;
                retry += 1;
            }
            Ok(response) => {
                let status = response.status();
                match response.text().await {
                    Ok(text) => return Ok(BufferedResponse { status, text }),
                    Err(error) if is_retryable_transport(&error) && retry < RETRY_DELAYS.len() => {
                        tokio::time::sleep(RETRY_DELAYS[retry]).await;
                        retry += 1;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) if is_retryable_transport(&error) && retry < RETRY_DELAYS.len() => {
                tokio::time::sleep(RETRY_DELAYS[retry]).await;
                retry += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

fn is_transient_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    )
}

fn is_retryable_transport(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request() || error.is_body()
}

/// Formats an error together with all nested causes.
pub fn format_error_chain(error: &(dyn Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{Client, StatusCode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client() -> Client {
        Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn retries_transient_gateway_status_until_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let http = client();

        let response = send_idempotent(|| {
            http.get(format!("{}/user/sign-ssh-keys", server.uri()))
                .send()
        })
        .await
        .unwrap();

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.text, "ok");
    }

    #[tokio::test]
    async fn retries_closed_connections_until_success() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (first, _) = listener.accept().await.unwrap();
            drop(first);
            let (mut second, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = second.read(&mut request).await.unwrap();
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
        });
        let http = client();

        let response = send_idempotent(|| http.get(format!("http://{address}/probe")).send())
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.text, "ok");
    }

    #[derive(Debug)]
    struct ErrorLayer {
        message: &'static str,
        source: Option<Box<ErrorLayer>>,
    }

    impl std::fmt::Display for ErrorLayer {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl std::error::Error for ErrorLayer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source
                .as_deref()
                .map(|source| source as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn formats_the_complete_error_source_chain() {
        let error = ErrorLayer {
            message: "request failed",
            source: Some(Box::new(ErrorLayer {
                message: "proxy connect failed",
                source: Some(Box::new(ErrorLayer {
                    message: "connection refused",
                    source: None,
                })),
            })),
        };

        assert_eq!(
            format_error_chain(&error),
            "request failed: proxy connect failed: connection refused"
        );
    }
}
