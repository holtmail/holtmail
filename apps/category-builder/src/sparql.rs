use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, RETRY_AFTER};
use serde::Deserialize;
use url::{Host, Url};

use crate::Error;

/// The public Wikidata Query Service.
pub(crate) const WIKIDATA_ENDPOINT: &str = "https://query.wikidata.org/sparql";

/// Requests made for one query before the build gives up.
pub(crate) const MAX_ATTEMPTS: u32 = 5;

/// The longest `Retry-After` honoured.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

const MAX_BODY: usize = 512 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(180);
const PAUSE: Duration = Duration::from_millis(500);

/// The User-Agent the Wikimedia policy asks for: tool, version, contact and library.
pub(crate) const USER_AGENT: &str = concat!(
    "holtmail-category-builder/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/holtmail/holtmail) reqwest/0.13"
);

/// One value of a solution: its RDF term type (`uri`, `literal`) and its text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Term {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

/// One solution: variable name to value. A variable left unbound is absent.
pub(crate) type Binding = HashMap<String, Term>;

#[derive(Deserialize)]
struct Document {
    head: Head,
    results: Results,
}

#[derive(Deserialize)]
struct Head {
    vars: Vec<String>,
}

#[derive(Deserialize)]
struct Results {
    bindings: Vec<Binding>,
}

/// Parses a complete SPARQL 1.1 JSON results document. A body that is cut off, has bytes
/// after the document, or lacks `head.vars` or `results.bindings` is refused.
pub(crate) fn parse_results(body: &[u8]) -> Result<Vec<Binding>, Error> {
    let document: Document = serde_json::from_slice(body).map_err(|e| {
        Error::Sparql(format!(
            "the answer is not complete SPARQL JSON (line {}, column {}), so it was probably truncated",
            e.line(),
            e.column()
        ))
    })?;
    let _vars = document.head.vars;
    Ok(document.results.bindings)
}

/// Runs SELECT queries. Answers are complete or the call fails; one call at a time.
pub(crate) trait Sparql {
    /// The solutions of `query`, in the order the endpoint gave them.
    async fn select(&self, query: &str) -> Result<Vec<Binding>, Error>;

    /// HTTP requests sent so far, retries included.
    fn requests(&self) -> usize {
        0
    }
}

/// SPARQL over HTTP GET with backoff on 429 and 503.
pub(crate) struct HttpSparql {
    client: reqwest::Client,
    endpoint: Url,
    pause: Duration,
    started: AtomicBool,
    requests: AtomicUsize,
}

impl HttpSparql {
    /// A client for `endpoint`: `https`, or plain `http` to a loopback address.
    pub(crate) fn new(endpoint: &str) -> Result<Self, Error> {
        Self::with_pause(endpoint, PAUSE)
    }

    pub(crate) fn with_pause(endpoint: &str, pause: Duration) -> Result<Self, Error> {
        let endpoint =
            Url::parse(endpoint).map_err(|_| Error::Config("the endpoint is not a URL".into()))?;
        let loopback = match endpoint.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && loopback) {
            return Err(Error::Config(
                "the endpoint must use https (plain http only to loopback)".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| Error::Config(format!("HTTP client: {e}")))?;
        Ok(Self {
            client,
            endpoint,
            pause,
            started: AtomicBool::new(false),
            requests: AtomicUsize::new(0),
        })
    }

    async fn attempt(&self, url: &Url) -> Result<Vec<Binding>, Retry> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let mut response = match self
            .client
            .get(url.clone())
            .header(ACCEPT, "application/sparql-results+json")
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => {
                let what = if e.is_timeout() {
                    "the endpoint did not answer in time"
                } else {
                    "the endpoint cannot be reached"
                };
                return Err(Retry::After(None, what.into()));
            }
        };
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            let wait = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_after(v, SystemTime::now()));
            return Err(Retry::After(wait, format!("status {}", status.as_u16())));
        }
        if matches!(status.as_u16(), 500 | 502 | 504) {
            return Err(Retry::After(None, format!("status {}", status.as_u16())));
        }
        if !status.is_success() {
            return Err(Retry::Fail(Error::Sparql(format!(
                "the endpoint answered status {}",
                status.as_u16()
            ))));
        }
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_BODY {
                        return Err(Retry::Fail(Error::Sparql(
                            "the answer is larger than the limit".into(),
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => {
                    return Err(Retry::Fail(Error::Sparql(
                        "the endpoint broke off its answer".into(),
                    )));
                }
            }
        }
        parse_results(&body).map_err(Retry::Fail)
    }
}

enum Retry {
    After(Option<Duration>, String),
    Fail(Error),
}

impl Sparql for HttpSparql {
    async fn select(&self, query: &str) -> Result<Vec<Binding>, Error> {
        let mut url = self.endpoint.clone();
        url.query_pairs_mut()
            .append_pair("query", query)
            .append_pair("format", "json");
        let mut last = String::new();
        for attempt in 1..=MAX_ATTEMPTS {
            if self.started.swap(true, Ordering::Relaxed) && !self.pause.is_zero() {
                tokio::time::sleep(self.pause).await;
            }
            match self.attempt(&url).await {
                Ok(bindings) => return Ok(bindings),
                Err(Retry::Fail(e)) => return Err(e),
                Err(Retry::After(wait, why)) => {
                    last = why;
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(wait.unwrap_or_else(|| backoff(attempt))).await;
                    }
                }
            }
        }
        Err(Error::Sparql(format!(
            "gave up after {MAX_ATTEMPTS} attempts, the last one: {last}"
        )))
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(6)).min(60))
}

/// The wait a `Retry-After` value asks for, as delay-seconds or an HTTP date, capped at
/// [`MAX_RETRY_AFTER`]. A date in the past is no wait; an unreadable value is `None`.
pub(crate) fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        let secs = value.parse::<u64>().unwrap_or(u64::MAX);
        return Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let at = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::try_from(at.timestamp()).ok()?))?;
    Some(
        at.duration_since(now)
            .unwrap_or(Duration::ZERO)
            .min(MAX_RETRY_AFTER),
    )
}

/// The number of a Wikidata entity IRI or bare id (`http://www.wikidata.org/entity/Q42`,
/// `Q42`).
pub(crate) fn qid(value: &str) -> Option<u64> {
    let id = value.rsplit('/').next()?.strip_prefix('Q')?;
    if id.is_empty()
        || id.len() > 18
        || !id.bytes().all(|b| b.is_ascii_digit())
        || id.starts_with('0')
    {
        return None;
    }
    id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_documents_only() {
        let ok = br#"{"head":{"vars":["c"]},"results":{"bindings":[{"c":{"type":"uri","value":"http://www.wikidata.org/entity/Q9000001"}}]}}"#;
        let rows = parse_results(ok).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(qid(&rows[0]["c"].value), Some(9_000_001));

        let truncated = include_bytes!("../tests/fixtures/truncated.json");
        assert!(matches!(parse_results(truncated), Err(Error::Sparql(_))));
        let trailing = [ok.as_slice(), b"\njava.lang.IllegalStateException"].concat();
        assert!(parse_results(&trailing).is_err());
        assert!(parse_results(br#"{"head":{"vars":[]}}"#).is_err());
        assert!(parse_results(b"").is_err());
    }

    #[test]
    fn retry_after_values() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(784_111_777);
        assert_eq!(retry_after("1", now), Some(Duration::from_secs(1)));
        assert_eq!(retry_after(" 120 ", now), Some(Duration::from_secs(120)));
        assert_eq!(retry_after("100000", now), Some(MAX_RETRY_AFTER));
        assert_eq!(
            retry_after("99999999999999999999999", now),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:49:47 GMT", now),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:00:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(
            retry_after("Mon, 07 Nov 1994 08:49:37 GMT", now),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(retry_after("soon", now), None);
        assert_eq!(retry_after("-1", now), None);
    }

    #[test]
    fn qids() {
        assert_eq!(qid("http://www.wikidata.org/entity/Q42"), Some(42));
        assert_eq!(qid("Q9000001"), Some(9_000_001));
        for bad in [
            "",
            "Q",
            "Q0",
            "Q01",
            "P31",
            "Q4x",
            "http://www.wikidata.org/entity/L1",
        ] {
            assert_eq!(qid(bad), None, "{bad}");
        }
    }

    #[test]
    fn endpoints() {
        assert!(HttpSparql::new(WIKIDATA_ENDPOINT).is_ok());
        assert!(HttpSparql::new("http://127.0.0.1:9/sparql").is_ok());
        assert!(HttpSparql::new("http://query.example.org/sparql").is_err());
        assert!(HttpSparql::new("ftp://query.example.org/").is_err());
        assert!(USER_AGENT.starts_with("holtmail-category-builder/"));
    }
}
