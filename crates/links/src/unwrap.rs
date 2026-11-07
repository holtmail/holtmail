use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::percent::decode_strict;

/// How many wrappers [`unwrap`] removes at most. A deeper chain stops after this many and its
/// destination is the URL left at that point.
pub const MAX_DEPTH: usize = 4;

/// The longest input, and the longest decoded URL at any step, that [`unwrap`] works on, in bytes.
/// Anything longer is returned unchanged.
pub const MAX_INPUT_BYTES: usize = 8 * 1024;

/// The result of [`unwrap`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Unwrapped {
    /// The URL the wrappers point to, or the input unchanged when nothing was unwrapped.
    pub destination: String,
    /// The registrable domain of the outermost wrapper's service (`google.com`, `outlook.com`,
    /// `proofpoint.com`), or `None` when nothing was unwrapped.
    pub via: Option<String>,
}

enum Step {
    NotWrapper,
    Malformed,
    Unwrapped(String, &'static str),
}

/// Removes known redirect wrappers from `url`.
///
/// Only an exact allow-list of hosts is understood: `www.google.com/url` (`q`, then `url`),
/// any host ending in `.safelinks.protection.outlook.com` (`url`), `urldefense.proofpoint.com`
/// `/v1/url` and `/v2/url` (`u`), and `urldefense.com` `/v3/__…__;…!`. Hosts are compared after URL
/// parsing (so case does not matter) and never by substring; a URL with userinfo is never a
/// wrapper.
///
/// Guarantees:
///
/// - Wrappers are removed in a loop, at most [`MAX_DEPTH`] deep; `via` names the outermost one.
/// - Every decoded step must be an absolute `http` or `https` URL with a host and no userinfo, at
///   most [`MAX_INPUT_BYTES`] long; an unwrapped destination is in its normalised
///   (`url`-serialised, A-label host) form.
/// - Fail closed: when any step meets a wrapper it cannot decode, or the input is longer than
///   [`MAX_INPUT_BYTES`], the result is the input unchanged with `via == None`.
/// - For a URL whose host is not on the allow-list, the result is the input unchanged with
///   `via == None`.
/// - Deterministic for a given [`version`](crate::version); never panics.
pub fn unwrap(url: &str) -> Unwrapped {
    let identity = || Unwrapped {
        destination: url.to_owned(),
        via: None,
    };
    if url.len() > MAX_INPUT_BYTES {
        return identity();
    }
    let mut current = url.to_owned();
    let mut via = None;
    for _ in 0..MAX_DEPTH {
        match step(&current) {
            Step::NotWrapper => break,
            Step::Malformed => return identity(),
            Step::Unwrapped(destination, name) => {
                via.get_or_insert(name);
                current = destination;
            }
        }
    }
    match via {
        Some(name) => Unwrapped {
            destination: current,
            via: Some(name.to_owned()),
        },
        None => identity(),
    }
}

pub(crate) fn parse_web(input: &str) -> Option<url::Url> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    let parsed = url::Url::parse(input).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none()
    {
        return None;
    }
    Some(parsed)
}

fn step(input: &str) -> Step {
    let Some(parsed) = parse_web(input) else {
        return Step::NotWrapper;
    };
    let Some(url::Host::Domain(host)) = parsed.host() else {
        return Step::NotWrapper;
    };
    let path = parsed.path();
    let (decoded, via) = match host {
        "www.google.com" if path == "/url" => (
            query_param(&parsed, "q").or_else(|| query_param(&parsed, "url")),
            "google.com",
        ),
        "urldefense.proofpoint.com" if path == "/v1/url" => (
            raw_param(&parsed, "u").and_then(decode_strict),
            "proofpoint.com",
        ),
        "urldefense.proofpoint.com" if path == "/v2/url" => (
            raw_param(&parsed, "u")
                .and_then(|u| decode_strict(&u.replace('-', "%").replace('_', "/"))),
            "proofpoint.com",
        ),
        "urldefense.com" if path.starts_with("/v3/__") => (decode_v3(input), "proofpoint.com"),
        h if is_safelinks(h) => (query_param(&parsed, "url"), "outlook.com"),
        _ => return Step::NotWrapper,
    };
    match decoded.as_deref().and_then(validate) {
        Some(destination) => Step::Unwrapped(destination, via),
        None => Step::Malformed,
    }
}

fn is_safelinks(host: &str) -> bool {
    host.strip_suffix(".safelinks.protection.outlook.com")
        .is_some_and(|prefix| !prefix.is_empty() && !prefix.ends_with('.'))
}

fn query_param(parsed: &url::Url, name: &str) -> Option<String> {
    parsed
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn raw_param<'a>(parsed: &'a url::Url, name: &str) -> Option<&'a str> {
    parsed
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

fn validate(decoded: &str) -> Option<String> {
    let parsed = parse_web(decoded)?;
    let out = String::from(parsed);
    (out.len() <= MAX_INPUT_BYTES).then_some(out)
}

const RUN_CODES: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

const V3_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

fn decode_v3(input: &str) -> Option<String> {
    let trimmed = input.trim_matches(|c: char| c <= ' ');
    if trimmed.contains(['\t', '\n', '\r']) {
        return None;
    }
    let (_, after_scheme) = trimmed.split_once("://")?;
    let start = after_scheme.find(['/', '\\', '?', '#'])?;
    let body = after_scheme.get(start..)?.strip_prefix("/v3/__")?;
    let (encoded_url, tail) = body.split_once("__;")?;
    if encoded_url.is_empty() {
        return None;
    }
    let (encoded_markers, _) = tail.split_once('!')?;
    let url = decode_strict(encoded_url)?;
    let markers = if encoded_markers.is_empty() {
        String::new()
    } else {
        String::from_utf8(V3_BASE64.decode(encoded_markers).ok()?).ok()?
    };
    let mut markers = markers.chars();
    let mut out = String::with_capacity(url.len());
    let mut chars = url.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '*' {
            out.push(c);
            continue;
        }
        if chars.next_if_eq(&'*').is_some() {
            let code = chars.next()?;
            let run = RUN_CODES.find(code)? + 2;
            for _ in 0..run {
                out.push(markers.next()?);
            }
        } else {
            out.push(markers.next()?);
        }
    }
    markers.next().is_none().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn via(url: &str) -> (String, Option<String>) {
        let u = unwrap(url);
        (u.destination, u.via)
    }

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
    }

    #[test]
    fn google() {
        assert_eq!(
            via("https://www.google.com/url?q=https%3A%2F%2Fexample.com%2Freset%3Ft%3D1&sa=D"),
            (
                "https://example.com/reset?t=1".to_owned(),
                Some("google.com".to_owned())
            )
        );
        assert_eq!(
            via("https://WWW.GOOGLE.COM/url?url=https://example.org/a"),
            (
                "https://example.org/a".to_owned(),
                Some("google.com".to_owned())
            )
        );
    }

    #[test]
    fn safelinks() {
        assert_eq!(
            via(
                "https://nam12.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2Fverify&data=05%7C01&reserved=0"
            ),
            (
                "https://example.com/verify".to_owned(),
                Some("outlook.com".to_owned())
            )
        );
        assert_eq!(
            via(
                "https://eur01.safelinks.protection.outlook.com/ap/t-59584e83/?url=https%3A%2F%2Fexample.org%2F"
            )
            .1
            .as_deref(),
            Some("outlook.com")
        );
    }

    #[test]
    fn proofpoint_v1() {
        assert_eq!(
            via(
                "https://urldefense.proofpoint.com/v1/url?u=https%3A%2F%2Fexample.com%2Fsign-in%3Fa%3Db&k=abc%3D%0A&r=x&m=y&s=z"
            ),
            (
                "https://example.com/sign-in?a=b".to_owned(),
                Some("proofpoint.com".to_owned())
            )
        );
    }

    #[test]
    fn proofpoint_v2() {
        assert_eq!(
            via(
                "https://urldefense.proofpoint.com/v2/url?u=https-3A__example.com_path-5Fname_page-3Fid-3D7-26x-3Dy&d=DwMFaQ&c=abc&r=def&m=ghi&s=jkl&e="
            ),
            (
                "https://example.com/path_name/page?id=7&x=y".to_owned(),
                Some("proofpoint.com".to_owned())
            )
        );
    }

    #[test]
    fn proofpoint_v3() {
        let url = format!(
            "https://urldefense.com/v3/__https://example.com/confirm?id=*1&next=**Edone__;{}!!AbCdEf!GhIjKl$",
            b64("2/a/b/c")
        );
        assert_eq!(
            via(&url),
            (
                "https://example.com/confirm?id=21&next=/a/b/cdone".to_owned(),
                Some("proofpoint.com".to_owned())
            )
        );
        assert_eq!(
            via("https://urldefense.com/v3/__https://example.org/plain__;!!AbC!dEf$"),
            (
                "https://example.org/plain".to_owned(),
                Some("proofpoint.com".to_owned())
            )
        );
        let padded = format!(
            "https://urldefense.com/v3/__https://example.com/*__;{}!!x",
            base64::engine::general_purpose::URL_SAFE.encode("é")
        );
        assert_eq!(via(&padded).0, "https://example.com/%C3%A9");
    }

    #[test]
    fn nested() {
        let inner =
            "https://nam01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2Fmagic";
        let outer = format!(
            "https://www.google.com/url?q={}",
            url::form_urlencoded::byte_serialize(inner.as_bytes()).collect::<String>()
        );
        assert_eq!(
            via(&outer),
            (
                "https://example.com/magic".to_owned(),
                Some("google.com".to_owned())
            )
        );
    }

    fn wrap_google(url: &str) -> String {
        format!(
            "https://www.google.com/url?q={}",
            url::form_urlencoded::byte_serialize(url.as_bytes()).collect::<String>()
        )
    }

    #[test]
    fn stops_at_depth_four() {
        let mut chain = vec!["https://example.com/end".to_owned()];
        for _ in 0..5 {
            let next = wrap_google(chain.last().map(String::as_str).unwrap_or_default());
            chain.push(next);
        }
        let five = chain.last().cloned().unwrap_or_default();
        let one = chain.get(1).cloned().unwrap_or_default();
        let u = unwrap(&five);
        assert_eq!(
            u.destination,
            url::Url::parse(&one).map(String::from).unwrap_or_default()
        );
        assert_eq!(u.via.as_deref(), Some("google.com"));
        let four = chain.get(4).cloned().unwrap_or_default();
        assert_eq!(unwrap(&four).destination, "https://example.com/end");
    }

    #[test]
    fn malformed_fails_closed() {
        let cases = [
            "https://www.google.com/url?sa=D",
            "https://www.google.com/url?q=not%20a%20url",
            "https://www.google.com/url?q=%2Frelative",
            "https://www.google.com/url?q=javascript%3Aalert(1)",
            "https://www.google.com/url?q=ftp%3A%2F%2Fexample.com%2F",
            "https://www.google.com/url?q=https%3A%2F%2Fuser%3Apw%40example.com%2F",
            "https://nam12.safelinks.protection.outlook.com/?data=x",
            "https://urldefense.proofpoint.com/v1/url?u=https%3A%2F%2Fexample.com%zz",
            "https://urldefense.proofpoint.com/v2/url?u=https-3A__example.com-ZZ",
            "https://urldefense.proofpoint.com/v2/url?d=x",
            "https://urldefense.com/v3/__https://example.com/*__;!!x",
            "https://urldefense.com/v3/__https://example.com/__;YQ!!x",
            "https://urldefense.com/v3/__https://example.com/**?__;YWJj!!x",
            "https://urldefense.com/v3/__https://example.com/*__;@@@!!x",
            "https://urldefense.com/v3/__https://example.com/__;",
            "https://urldefense.com/v3/____;!!x",
            "https://urldefense.com/v3/__mailto:a@example.com__;!!x",
        ];
        for case in cases {
            assert_eq!(via(case), (case.to_owned(), None), "{case}");
        }
        let inner_bad = wrap_google("https://www.google.com/url?sa=D");
        assert_eq!(via(&inner_bad), (inner_bad.clone(), None));
    }

    #[test]
    fn look_alikes_stay_identity() {
        let cases = [
            "https://evil.example/url?q=https%3A%2F%2Fexample.com%2F",
            "https://safelinks.protection.outlook.com.evil.example/?url=https%3A%2F%2Fexample.com%2F",
            "https://safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2F",
            "https://evilsafelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2F",
            "https://www.google.com.evil.example/url?q=https%3A%2F%2Fexample.com%2F",
            "https://google.com/url?q=https%3A%2F%2Fexample.com%2F",
            "https://www.google.com/search?q=https%3A%2F%2Fexample.com%2F",
            "https://urldefense.com@evil.example/v3/__https://example.com/__;!!x",
            "https://user@www.google.com/url?q=https%3A%2F%2Fexample.com%2F",
            "https://urldefense.com.evil.example/v3/__https://example.com/__;!!x",
            "https://urldefense.proofpoint.com/v3/__https://example.com/__;!!x",
            "https://urldefense.com/v2/url?u=https-3A__example.com",
            "ftp://www.google.com/url?q=https%3A%2F%2Fexample.com%2F",
            "https://www.google.com./url?q=https%3A%2F%2Fexample.com%2F",
            "not a url",
            "",
        ];
        for case in cases {
            assert_eq!(via(case), (case.to_owned(), None), "{case}");
        }
    }

    #[test]
    fn long_input_is_identity() {
        let long = format!(
            "https://www.google.com/url?q=https%3A%2F%2Fexample.com%2F{}",
            "a".repeat(MAX_INPUT_BYTES)
        );
        assert_eq!(via(&long), (long.clone(), None));
    }
}
