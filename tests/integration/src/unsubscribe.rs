//! The stack's one-click unsubscribe endpoint: the `images` nginx also serves HTTPS on 443 with
//! the stack's certificate, on two internal networks with private subnets. On `unsubscribe`
//! (`10.88.79.0/24`) it is `10.88.79.10`, named `unsubscribe.it.test`; on `unlisted`
//! (`10.88.80.0/24`) it is `10.88.80.10`, named `outside.it.test`. A test image allows only the
//! first subnet, so the same server is reachable by one name and must be refused by the other.
//!
//! Paths: `/accept/…` answers `200`, `/broken/…` `500`, `/moved/…` a `302` to
//! `/landing/moved/…`, anything else `200`. nginx logs each request on 443 with its method,
//! path, a few headers and its body ([`requests`]).

use std::time::{Duration, Instant};

/// The endpoint's name on [`ALLOWED_NETWORK`].
pub const ENDPOINT_NAME: &str = "unsubscribe.it.test";
/// The same server's name on [`UNLISTED_NETWORK`].
pub const OUTSIDE_NAME: &str = "outside.it.test";
/// The Docker network whose subnet the test image allows.
pub const ALLOWED_NETWORK: &str = "holtmail-it_unsubscribe";
/// Its subnet, for `[insights] one_click_test_networks`.
pub const ALLOWED_SUBNET: &str = "10.88.79.0/24";
/// A Docker network with a private subnet the test image does not allow.
pub const UNLISTED_NETWORK: &str = "holtmail-it_unlisted";

/// One request nginx logged on 443.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// `POST`, `GET`, ...
    pub method: String,
    /// The path and query.
    pub uri: String,
    /// `Content-Type`.
    pub content_type: String,
    /// `Cookie`.
    pub cookie: String,
    /// `Authorization`.
    pub authorization: String,
    /// `Referer`.
    pub referer: String,
    /// `Origin`.
    pub origin: String,
    /// The request body.
    pub body: String,
}

/// Every request nginx logged on 443 whose path starts with `prefix`, oldest first.
pub fn requests(prefix: &str) -> Vec<Request> {
    let logs = crate::services::compose(&["logs", "--no-log-prefix", "images"]);
    logs.lines()
        .filter_map(|line| line.strip_prefix("unsubscribe "))
        .filter_map(parse)
        .filter(|request| request.uri.starts_with(prefix))
        .collect()
}

/// Waits up to 10 seconds until nginx logged `count` requests under `prefix`, and returns them.
pub async fn wait_requests(prefix: &str, count: usize) -> Vec<Request> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = requests(prefix);
        if seen.len() >= count || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn parse(line: &str) -> Option<Request> {
    let (method, rest) = line.split_once(' ')?;
    let (uri, mut rest) = rest.split_once(' ')?;
    let mut request = Request {
        method: method.to_owned(),
        uri: uri.to_owned(),
        ..Request::default()
    };
    while let Some((key, after)) = rest.trim_start().split_once("=\"") {
        let (value, tail) = quoted(after)?;
        match key {
            "type" => request.content_type = value,
            "cookie" => request.cookie = value,
            "authorization" => request.authorization = value,
            "referer" => request.referer = value,
            "origin" => request.origin = value,
            "body" => request.body = value,
            _ => {}
        }
        rest = tail;
    }
    Some(request)
}

pub(crate) fn quoted(text: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Some((out, &text[at + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = (0..4)
                        .filter_map(|_| chars.next().map(|(_, c)| c))
                        .collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nginx_json_escaped_lines() {
        let line = r#"POST /accept/a-1 type="multipart/form-data; boundary=x" cookie="" authorization="" referer="" origin="" body="--x\r\nContent-Disposition: form-data; name=\"List-Unsubscribe\"\r\n\r\nOne-Click\r\n--x--\r\n""#;
        let request = parse(line).expect("a request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.uri, "/accept/a-1");
        assert_eq!(request.content_type, "multipart/form-data; boundary=x");
        assert!(request.cookie.is_empty() && request.origin.is_empty());
        assert!(
            request
                .body
                .contains("name=\"List-Unsubscribe\"\r\n\r\nOne-Click\r\n")
        );
    }
}
