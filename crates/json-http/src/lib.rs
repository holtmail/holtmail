//! JSON over HTTPS for the APIs of outside services, such as alias services: one base address,
//! fixed headers, a timeout, a size limit and status codes turned into errors.
//!
//! A library, not a contract: provider crates use it and map [`HttpError`] into their own
//! contract's error.

use std::net::IpAddr;
use std::time::Duration;

use holtmail_core::TlsRoots;
pub use reqwest::Method;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::{Host, Url};

/// The largest answer read, in bytes.
pub const MAX_ANSWER: usize = 16 * 1024 * 1024;

/// Why a request gave no usable answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    /// The base address or a header cannot be used. Nothing was sent.
    #[error("{0}")]
    Invalid(String),
    /// The service answered with a status other than 2xx. `message` is the `error`, `message`
    /// or `detail` text of a JSON answer, when it has one.
    #[error("status {status}")]
    Status {
        /// The HTTP status.
        status: u16,
        /// The service's own words.
        message: Option<String>,
    },
    /// No answer: the service could not be reached or took too long.
    #[error("{0}")]
    Unreachable(String),
    /// The answer was not the JSON expected, or larger than [`MAX_ANSWER`].
    #[error("{0}")]
    Unexpected(String),
}

/// Whether `url` may be used: `https`, or plain `http` to a loopback address.
pub fn allowed(url: &Url) -> bool {
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        },
        _ => false,
    }
}

/// A client for one service. Cheap to clone. Follows no redirects, keeps no cookies and uses no
/// proxy.
#[derive(Clone)]
pub struct JsonHttp {
    client: reqwest::Client,
    exact: Url,
    base: Url,
    headers: Vec<(HeaderName, HeaderValue)>,
}

impl JsonHttp {
    /// A client for paths below `base` that sends `headers` with every request and gives up
    /// after `timeout`. `Invalid` for an address [`allowed`] refuses, a header that cannot be
    /// sent, or an extra root that is not a usable certificate.
    pub fn new(
        base: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
        roots: &TlsRoots,
    ) -> Result<Self, HttpError> {
        let exact = Url::parse(base.trim())
            .map_err(|_| HttpError::Invalid("the service address is not a URL".into()))?;
        let mut base = exact.clone();
        if !allowed(&base) {
            return Err(HttpError::Invalid(
                "the service address must use https".into(),
            ));
        }
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let headers = headers
            .iter()
            .map(|(name, value)| {
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| HttpError::Invalid(format!("header {name}")))?;
                let mut value = HeaderValue::from_str(value)
                    .map_err(|_| HttpError::Invalid(format!("the value of {name}")))?;
                value.set_sensitive(true);
                Ok((name, value))
            })
            .collect::<Result<Vec<_>, HttpError>>()?;
        let certificates = roots
            .iter()
            .map(reqwest::Certificate::from_der)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| HttpError::Invalid(format!("an extra trusted certificate: {e}")))?;
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent(concat!("Holtmail/", env!("CARGO_PKG_VERSION")))
            .tls_certs_merge(certificates)
            .build()
            .map_err(|e| HttpError::Invalid(e.to_string()))?;
        Ok(Self {
            client,
            exact,
            base,
            headers,
        })
    }

    /// The host requests go to, for messages.
    pub fn host(&self) -> &str {
        self.base.host_str().unwrap_or("the service")
    }

    /// `GET` `path` (relative to the base, query included) and reads the answer as `T`.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, HttpError> {
        self.call(Method::GET, path, None::<&()>).await
    }

    /// Sends `method` to `path` with `body` as JSON, when given, and reads the answer as `T`. An
    /// empty `path` is the base address exactly as given. An empty answer reads as JSON `null`,
    /// so `T = ()` or `Option<_>` accepts `204`.
    pub async fn call<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, HttpError> {
        let url = if path.is_empty() {
            self.exact.clone()
        } else {
            self.base
                .join(path.trim_start_matches('/'))
                .map_err(|_| HttpError::Invalid(format!("path {path}")))?
        };
        let mut request = self
            .client
            .request(method, url)
            .header(ACCEPT, "application/json");
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            let bytes = serde_json::to_vec(body)
                .map_err(|e| HttpError::Invalid(format!("request body: {e}")))?;
            request = request.header(CONTENT_TYPE, "application/json").body(bytes);
        }
        let host = self.host().to_owned();
        let mut response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                HttpError::Unreachable(format!("{host} did not answer in time"))
            } else {
                HttpError::Unreachable(format!("{host} cannot be reached"))
            }
        })?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|n| n > MAX_ANSWER as u64)
        {
            return Err(HttpError::Unexpected(format!(
                "{host} sent an answer that is too large"
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| HttpError::Unreachable(format!("{host} broke off its answer")))?
        {
            if bytes.len() + chunk.len() > MAX_ANSWER {
                return Err(HttpError::Unexpected(format!(
                    "{host} sent an answer that is too large"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(HttpError::Status {
                status: status.as_u16(),
                message: message(&bytes),
            });
        }
        let text = if bytes.iter().all(u8::is_ascii_whitespace) {
            b"null".as_slice()
        } else {
            &bytes
        };
        serde_json::from_slice(text).map_err(|e| {
            HttpError::Unexpected(format!("{host} sent an answer not understood: {e}"))
        })
    }
}

/// Milliseconds since the Unix epoch of a date-time as APIs write it: `2025-03-03 18:00:00`,
/// `2025-03-03T18:00:00Z`, `2025-03-03T18:00:00.123456+02:00`. Without a zone it is UTC. `None`
/// for anything else.
pub fn utc_millis(text: &str) -> Option<i64> {
    let text = text.trim();
    let digits = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
    };
    if text.len() < 19 {
        return None;
    }
    let (date, rest) = text.split_at(10);
    let mut parts = date.split('-');
    let (year, month, day) = (
        digits(parts.next()?)?,
        digits(parts.next()?)?,
        digits(parts.next()?)?,
    );
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let (time, mut zone) = rest.split_at(8);
    let mut clock = time.split(':');
    let (hour, minute, second) = (
        digits(clock.next()?)?,
        digits(clock.next()?)?,
        digits(clock.next()?)?,
    );
    let mut millis = 0;
    if let Some(fraction) = zone.strip_prefix('.') {
        let end = fraction
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(fraction.len());
        let first: String = fraction[..end]
            .chars()
            .chain("000".chars())
            .take(3)
            .collect();
        millis = digits(&first)?;
        zone = &fraction[end..];
    }
    let offset = match zone {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match zone.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hm = zone[1..].replace(':', "");
            if hm.len() != 4 {
                return None;
            }
            sign * (digits(&hm[..2])? * 60 + digits(&hm[2..])?)
        }
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset * 60;
    Some(seconds * 1000 + millis)
}

fn message(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let text = ["error", "message", "detail"]
        .iter()
        .find_map(|key| value.get(key))
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(map) => {
                map.values().find_map(|v| v.as_str().map(str::to_owned))
            }
            serde_json::Value::Array(items) => {
                items.first().and_then(|v| v.as_str()).map(str::to_owned)
            }
            _ => None,
        })
        .or_else(|| {
            value
                .get("errors")
                .and_then(|e| e.as_object())
                .and_then(|map| map.values().next())
                .and_then(|v| v.as_array().and_then(|a| a.first()).or(Some(v)))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })?;
    let text: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{delete, get, post};
    use serde_json::{Value, json};

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}/api")
    }

    #[test]
    fn reads_api_date_times() {
        assert_eq!(utc_millis("1970-01-01 00:00:00"), Some(0));
        assert_eq!(utc_millis("2025-03-03 18:00:00"), Some(1_741_024_800_000));
        assert_eq!(utc_millis("2025-03-03T18:00:00Z"), Some(1_741_024_800_000));
        assert_eq!(
            utc_millis("2025-03-03T20:00:00.250999+02:00"),
            Some(1_741_024_800_250)
        );
        assert_eq!(
            utc_millis("2024-02-29T00:00:00-01:30"),
            Some(1_709_170_200_000)
        );
        assert_eq!(utc_millis("2025-13-03 18:00:00"), None);
        assert_eq!(utc_millis("yesterday"), None);
        assert_eq!(utc_millis("2025-03-03T18:00:00+2"), None);
    }

    #[test]
    fn only_https_or_loopback() {
        let ok = |u: &str| allowed(&Url::parse(u).unwrap());
        assert!(ok("https://app.example.com/api"));
        assert!(ok("http://127.0.0.1:8080"));
        assert!(ok("http://localhost:8080"));
        assert!(ok("http://[::1]:8080"));
        assert!(!ok("http://app.example.com"));
        assert!(!ok("ftp://app.example.com"));
        let refused = JsonHttp::new(
            "http://app.example.com",
            &[],
            Duration::from_secs(1),
            &TlsRoots::default(),
        );
        assert!(matches!(refused, Err(HttpError::Invalid(_))));
    }

    #[tokio::test]
    async fn sends_headers_and_bodies_and_maps_statuses() {
        let router = Router::new()
            .route(
                "/api/v1/echo",
                post(|headers: HeaderMap, body: axum::Json<Value>| async move {
                    axum::Json(json!({
                        "key": headers.get("authentication").and_then(|v| v.to_str().ok()),
                        "body": body.0,
                    }))
                }),
            )
            .route(
                "/api/v1/refused",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({"error": "prefix taken"})),
                    )
                }),
            )
            .route(
                "/api/v1/laravel",
                get(|| async {
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        axum::Json(json!({"errors": {"local_part": ["The local part is taken."]}})),
                    )
                }),
            )
            .route("/api/v1/gone", delete(|| async { StatusCode::NO_CONTENT }))
            .route("/api/v1/html", get(|| async { "<html>".into_response() }));
        let base = serve(router).await;
        let http = JsonHttp::new(
            &base,
            &[("Authentication", "secret-key")],
            Duration::from_secs(5),
            &TlsRoots::default(),
        )
        .unwrap();
        let echo: Value = http
            .call(Method::POST, "v1/echo", Some(&json!({"note": "hi"})))
            .await
            .unwrap();
        assert_eq!(echo, json!({"key": "secret-key", "body": {"note": "hi"}}));
        assert_eq!(
            http.get::<Value>("v1/refused").await,
            Err(HttpError::Status {
                status: 400,
                message: Some("prefix taken".into())
            })
        );
        assert_eq!(
            http.get::<Value>("v1/laravel").await,
            Err(HttpError::Status {
                status: 422,
                message: Some("The local part is taken.".into())
            })
        );
        http.call::<(), ()>(Method::DELETE, "v1/gone", None)
            .await
            .unwrap();
        assert!(matches!(
            http.get::<Value>("v1/html").await,
            Err(HttpError::Unexpected(_))
        ));
        assert!(matches!(
            http.get::<Value>("v1/missing").await,
            Err(HttpError::Status { status: 404, .. })
        ));
    }

    #[tokio::test]
    async fn an_unreachable_service_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let http = JsonHttp::new(
            &format!("http://{addr}"),
            &[],
            Duration::from_secs(2),
            &TlsRoots::default(),
        )
        .unwrap();
        assert!(matches!(
            http.get::<Value>("x").await,
            Err(HttpError::Unreachable(_))
        ));
    }
}
