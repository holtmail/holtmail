use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::ConnectInfo;
use axum::http::{Extensions, HeaderMap, header};
use base64::Engine;
use holtmail_serve_net::TrustedProxies;

/// The client address: the peer, or what a trusted proxy reports in `X-Forwarded-For` (the
/// rightmost hop that is not itself trusted).
pub(crate) fn client_ip(
    trusted: &TrustedProxies,
    headers: &HeaderMap,
    extensions: &Extensions,
) -> IpAddr {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().to_canonical())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if !trusted.contains(peer) {
        return peer;
    }
    forwarded(headers, trusted).unwrap_or(peer)
}

fn forwarded(headers: &HeaderMap, trusted: &TrustedProxies) -> Option<IpAddr> {
    let hops = headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|value| value.to_str().ok())
        .collect::<Option<Vec<_>>>()?
        .join(",");
    let mut client = None;
    for hop in hops.rsplit(',').map(str::trim) {
        let Some(ip) = hop
            .parse::<IpAddr>()
            .ok()
            .or_else(|| hop.parse::<SocketAddr>().ok().map(|s| s.ip()))
        else {
            break;
        };
        client = Some(ip.to_canonical());
        if !trusted.contains(ip) {
            break;
        }
    }
    client
}

/// The login and secret of an HTTP Basic `Authorization` header.
pub(crate) fn basic(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (login, secret) = text.split_once(':')?;
    Some((login.to_owned(), secret.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn reads_basic_credentials() {
        let mut headers = HeaderMap::new();
        let token = base64::engine::general_purpose::STANDARD.encode("ada@example.com:hm_x:y");
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {token}")).unwrap(),
        );
        assert_eq!(
            basic(&headers),
            Some(("ada@example.com".into(), "hm_x:y".into()))
        );
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer x"));
        assert_eq!(basic(&headers), None);
    }

    #[test]
    fn believes_only_trusted_proxies() {
        let trusted = TrustedProxies::parse(&["10.0.0.0/8"]).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.0.2.1, 198.51.100.7, 10.0.0.3"),
        );
        let mut ext = Extensions::new();
        ext.insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 2], 4000))));
        assert_eq!(
            client_ip(&trusted, &headers, &ext),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
        let mut ext = Extensions::new();
        ext.insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 4000))));
        assert_eq!(
            client_ip(&trusted, &headers, &ext),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
    }
}
