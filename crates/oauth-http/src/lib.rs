//! A [`TokenEndpoint`] over HTTPS on reqwest and rustls.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use holtmail_oauth::{OAuthError, TokenAnswer, TokenEndpoint};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use url::{Host, Url};

const MAX_BODY: usize = 64 * 1024;

/// Posts token requests over HTTPS. Plain HTTP is used only for loopback addresses, so tests
/// can run a token endpoint in the process. Redirects are not followed.
#[derive(Clone)]
pub struct HttpTokenEndpoint {
    client: reqwest::Client,
}

impl HttpTokenEndpoint {
    /// An endpoint client that gives up after `timeout` per request.
    pub fn new(timeout: Duration) -> Result<Self, OAuthError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| OAuthError::Connection(e.to_string()))?;
        Ok(Self { client })
    }
}

fn allowed(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        },
        _ => false,
    }
}

#[async_trait]
impl TokenEndpoint for HttpTokenEndpoint {
    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<TokenAnswer, OAuthError> {
        let url = Url::parse(url)
            .map_err(|_| OAuthError::Connection("the token endpoint is not a URL".into()))?;
        if !allowed(&url) {
            return Err(OAuthError::Connection(
                "token endpoints must use https".into(),
            ));
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let host = url.host_str().unwrap_or_default().to_owned();
        let mut response = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(ACCEPT, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| {
                OAuthError::Connection(if e.is_timeout() {
                    format!("{host} did not answer in time")
                } else {
                    format!("{host} could not be reached")
                })
            })?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| OAuthError::Connection(format!("the answer from {host} broke off")))?
        {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(OAuthError::Protocol(format!(
                    "the answer from {host} is too large"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(TokenAnswer { status, body })
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn endpoint() -> HttpTokenEndpoint {
        HttpTokenEndpoint::new(Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn keeps_the_token_endpoint_contract() {
        holtmail_oauth::testing::conformance(&endpoint()).await;
    }

    async fn serve_once(
        status: &'static str,
        body: &'static str,
    ) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if rest.len() >= length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let answer = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(answer.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (port, task)
    }

    #[tokio::test]
    async fn posts_the_form_and_returns_any_status() {
        let (port, request) = serve_once("400 Bad Request", r#"{"error":"invalid_grant"}"#).await;
        let answer = endpoint()
            .post_form(
                &format!("http://127.0.0.1:{port}/token"),
                &[
                    ("grant_type".into(), "refresh_token".into()),
                    ("refresh_token".into(), "a/b c".into()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(answer.status, 400);
        assert_eq!(answer.body, br#"{"error":"invalid_grant"}"#);
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /token HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        assert!(request.ends_with("grant_type=refresh_token&refresh_token=a%2Fb+c"));
    }

    #[tokio::test]
    async fn works_with_the_oauth_client() {
        let (port, _) = serve_once(
            "200 OK",
            r#"{"access_token":"a1","refresh_token":"r1","expires_in":60}"#,
        )
        .await;
        let mut client = holtmail_oauth::Client::thunderbird(holtmail_oauth::Provider::Google);
        client.token_url = format!("http://localhost:{port}/token");
        let tokens = client
            .exchange(&endpoint(), "code", "verifier")
            .await
            .unwrap();
        assert_eq!(tokens.access_token, "a1");
        assert_eq!(tokens.expires_in, Duration::from_secs(60));
    }
}
