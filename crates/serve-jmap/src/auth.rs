use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use holtmail_core::{ArchiveId, DeviceId, DeviceProtocol};
use holtmail_device::{DeviceAuthError, Principal};
use holtmail_mailview::Viewer;
use holtmail_serve_net::TrustedProxies;
use holtmail_submission::Actor;

use crate::Inner;
use crate::error::Problem;

const CACHE_FOR: Duration = Duration::from_secs(10);
const CACHE_MAX: usize = 4096;

/// Verified app passwords, kept at most [`CACHE_FOR`] and dropped by the revocation feed.
#[derive(Default)]
pub(crate) struct Cache(Mutex<HashMap<[u8; 32], (Principal, Instant)>>);

impl Cache {
    fn key(login: &str, secret: &str) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(login.to_ascii_lowercase().as_bytes());
        hasher.update(&[0]);
        hasher.update(secret.as_bytes());
        *hasher.finalize().as_bytes()
    }

    fn get(&self, key: &[u8; 32]) -> Option<Principal> {
        let map = self.0.lock().expect("auth cache");
        map.get(key)
            .filter(|(_, at)| at.elapsed() < CACHE_FOR)
            .map(|(p, _)| p.clone())
    }

    fn put(&self, key: [u8; 32], principal: Principal) {
        let mut map = self.0.lock().expect("auth cache");
        if map.len() >= CACHE_MAX {
            map.retain(|_, (_, at)| at.elapsed() < CACHE_FOR);
        }
        map.insert(key, (principal, Instant::now()));
    }

    pub fn forget(&self, device: DeviceId) {
        self.0
            .lock()
            .expect("auth cache")
            .retain(|_, (p, _)| p.device != device);
    }
}

/// A signed-in device and the login it used.
#[derive(Debug, Clone)]
pub(crate) struct Authed {
    pub principal: Principal,
    pub login: String,
}

impl Authed {
    pub fn viewer(&self, archive: ArchiveId) -> Viewer {
        Viewer {
            user: self.principal.user,
            device: Some(self.principal.device),
            archive,
            scopes: self.principal.scopes.clone(),
        }
    }

    pub fn actor(&self) -> Actor {
        Actor {
            user: self.principal.user,
            device: Some(self.principal.device),
            scopes: self.principal.scopes.clone(),
        }
    }

    pub fn has(&self, archive: ArchiveId) -> bool {
        self.principal.archives.contains(&archive)
    }
}

/// The client address: the peer, or what a trusted proxy reports in `X-Forwarded-For` (the
/// rightmost hop that is not itself trusted).
pub(crate) fn client_ip(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    trusted: &TrustedProxies,
) -> IpAddr {
    let peer = peer
        .map(|p| p.ip().to_canonical())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if !trusted.contains(peer) {
        return peer;
    }
    let Some(hops) = headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|v| v.to_str().ok())
        .collect::<Option<Vec<_>>>()
        .map(|v| v.join(","))
    else {
        return peer;
    };
    let mut client = peer;
    for hop in hops.rsplit(',').map(str::trim) {
        let Some(ip) = hop
            .parse::<IpAddr>()
            .ok()
            .or_else(|| hop.parse::<SocketAddr>().ok().map(|s| s.ip()))
        else {
            break;
        };
        client = ip.to_canonical();
        if !trusted.contains(ip) {
            break;
        }
    }
    client
}

fn credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, rest) = value.split_once(' ')?;
    let pair = if scheme.eq_ignore_ascii_case("basic") {
        String::from_utf8(STANDARD.decode(rest.trim()).ok()?).ok()?
    } else if scheme.eq_ignore_ascii_case("bearer") {
        rest.trim().to_string()
    } else {
        return None;
    };
    let (login, secret) = pair.split_once(':')?;
    (!login.is_empty() && !secret.is_empty()).then(|| (login.to_string(), secret.to_string()))
}

pub(crate) fn unauthorized(headers: &HeaderMap) -> Response {
    let mut response = Problem {
        status: StatusCode::UNAUTHORIZED,
        kind: "about:blank",
        detail: "sign in with your email address and a device's app password".into(),
        limit: None,
    }
    .into_response();
    let challenge = match from_browser(headers) {
        true => "Bearer realm=\"Holtmail\"",
        false => "Basic realm=\"Holtmail\", charset=\"UTF-8\"",
    };
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(challenge),
    );
    response
}

/// Whether a browser sent the request: browsers send `Sec-Fetch-Mode` or `Origin`. A browser
/// gets no `Basic` challenge, so it never prompts for the app password and never keeps it for
/// other sites' requests to replay.
fn from_browser(headers: &HeaderMap) -> bool {
    headers.contains_key("sec-fetch-mode") || headers.contains_key(header::ORIGIN)
}

/// Whether another site sent the request without a CORS check: a navigation, a form or a
/// `no-cors` fetch, which carry credentials a browser kept but are never preflighted.
fn cross_site_unchecked(headers: &HeaderMap) -> bool {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    matches!(value("sec-fetch-site").as_str(), "cross-site" | "same-site")
        && matches!(
            value("sec-fetch-mode").as_str(),
            "navigate" | "nested-navigate" | "no-cors"
        )
}

/// Whether a WebSocket handshake comes from a page of another origin than this host. Browsers
/// send `Origin` on every handshake and cannot set `Authorization`, so such a socket could only
/// sign in with credentials the browser kept; WebSockets bypass CORS.
pub(crate) fn foreign_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let authority = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    !authority.eq_ignore_ascii_case(host)
}

pub(crate) fn forbidden(detail: &str) -> Response {
    Problem {
        status: StatusCode::FORBIDDEN,
        kind: "about:blank",
        detail: detail.into(),
        limit: None,
    }
    .into_response()
}

/// Signs the request's device in, or answers the response that refuses it.
pub(crate) async fn authenticate(
    inner: &Inner,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> Result<Authed, Box<Response>> {
    if cross_site_unchecked(headers) {
        return Err(Box::new(forbidden(
            "a request from another site must pass a CORS check",
        )));
    }
    let ip = client_ip(headers, peer, &inner.settings.trusted_proxies);
    if !inner.settings.public_url.starts_with("https://") && !ip.is_loopback() {
        return Err(Box::new(
            Problem {
                status: StatusCode::FORBIDDEN,
                kind: "about:blank",
                detail: "JMAP signs in only over https".into(),
                limit: None,
            }
            .into_response(),
        ));
    }
    let Some((login, secret)) = credentials(headers) else {
        return Err(Box::new(unauthorized(headers)));
    };
    let key = Cache::key(&login, &secret);
    if let Some(principal) = inner.auth.get(&key) {
        return Ok(Authed { principal, login });
    }
    match inner
        .services
        .devices
        .verify(&login, &secret, ip, DeviceProtocol::Jmap)
        .await
    {
        Ok(principal) => {
            inner.auth.put(key, principal.clone());
            Ok(Authed { principal, login })
        }
        Err(DeviceAuthError::Invalid | DeviceAuthError::Revoked) => {
            Err(Box::new(unauthorized(headers)))
        }
        Err(DeviceAuthError::TooMany { retry_after }) => {
            let mut response = Problem {
                status: StatusCode::TOO_MANY_REQUESTS,
                kind: "about:blank",
                detail: "too many failed sign-ins".into(),
                limit: None,
            }
            .into_response();
            if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            Err(Box::new(response))
        }
        Err(DeviceAuthError::Unavailable(reason)) => {
            tracing::warn!(%reason, "device sign-in unavailable");
            Err(Box::new(
                Problem {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    kind: "about:blank",
                    detail: "sign-in is unavailable; try again later".into(),
                    limit: None,
                }
                .into_response(),
            ))
        }
    }
}

pub(crate) fn peer(request_extensions: &axum::http::Extensions) -> Option<SocketAddr> {
    request_extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0)
}

/// CORS for browser clients: any origin, no cookies, preflights answered here.
pub(crate) async fn cors(request: Request, next: Next) -> Response {
    let preflight = request.method() == Method::OPTIONS;
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    if preflight {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type, Accept, Last-Event-ID"),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("86400"),
        );
    } else {
        headers.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("WWW-Authenticate, Retry-After, Content-Disposition"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_basic_and_bearer() {
        let mut headers = HeaderMap::new();
        let basic = format!("Basic {}", STANDARD.encode("ada@example.com:hm_secret"));
        headers.insert(header::AUTHORIZATION, basic.parse().unwrap());
        assert_eq!(
            credentials(&headers),
            Some(("ada@example.com".into(), "hm_secret".into()))
        );
        headers.insert(
            header::AUTHORIZATION,
            "Bearer ada@example.com:hm_secret".parse().unwrap(),
        );
        assert_eq!(
            credentials(&headers),
            Some(("ada@example.com".into(), "hm_secret".into()))
        );
        headers.insert(header::AUTHORIZATION, "Bearer hm_secret".parse().unwrap());
        assert_eq!(credentials(&headers), None);
    }

    #[test]
    fn believes_forwarded_only_from_trusted_proxies() {
        let trusted = TrustedProxies::parse(&["192.0.2.0/24"]).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.9, 192.0.2.7".parse().unwrap());
        let proxy: SocketAddr = "192.0.2.1:443".parse().unwrap();
        let other: SocketAddr = "198.51.100.1:443".parse().unwrap();
        assert_eq!(
            client_ip(&headers, Some(proxy), &trusted),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
        assert_eq!(client_ip(&headers, Some(other), &trusted), other.ip());
    }
}
