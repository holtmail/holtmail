//! Conformance suites every [`Fetcher`], [`OneClick`] and [`DataDownload`] implementation runs in
//! its own tests.

mod server;

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use holtmail_core::TlsRoots;
use holtmail_core::tls::testing::TestCa;

use crate::{DataDownload, Download, FetchError, Fetcher, OneClick, OneClickOutcome};

/// URLs a fetcher must refuse without contacting anything outside the machine.
pub const REFUSED: &[&str] = &[
    "not a url",
    "ftp://example.com/logo.png",
    "file:///etc/passwd",
    "data:image/png;base64,AAAA",
    "http://127.0.0.1/logo.png",
    "http://127.1/logo.png",
    "http://2130706433/logo.png",
    "http://0.0.0.0/logo.png",
    "http://localhost/logo.png",
    "http://localhost:8080/api/archives",
    "http://[::1]/logo.png",
    "http://[::ffff:127.0.0.1]/logo.png",
    "http://10.1.2.3/logo.png",
    "http://172.16.0.1/logo.png",
    "http://192.168.1.1/logo.png",
    "http://100.64.0.1/logo.png",
    "http://169.254.169.254/latest/meta-data/",
    "http://[fd00::1]/logo.png",
    "http://[fe80::1]/logo.png",
];

/// Checks that `fetcher` refuses every URL in [`REFUSED`] with [`FetchError::Refused`].
/// Panics on the first violation. Needs no network.
pub async fn conformance(fetcher: &dyn Fetcher) {
    for url in REFUSED {
        match fetcher.image(url).await {
            Err(FetchError::Refused(_)) => {}
            other => panic!("{url} was not refused: {other:?}"),
        }
    }
}

/// URLs a one-click implementation must refuse without resolving or contacting anything.
pub const ONE_CLICK_REFUSED: &[&str] = &[
    "not a url",
    "http://news.example.com/unsubscribe",
    "ftp://news.example.com/unsubscribe",
    "file:///etc/passwd",
    "data:text/plain,One-Click",
    "mailto:unsubscribe@example.com",
    "https://user@news.example.com/unsubscribe",
    "https://user:secret@news.example.com/unsubscribe",
    "https://:secret@news.example.com/unsubscribe",
    "https://news.example.com:8443/unsubscribe",
    "https://news.example.com:80/unsubscribe",
    "https://127.0.0.1/unsubscribe",
    "https://127.1/unsubscribe",
    "https://2130706433/unsubscribe",
    "https://0.0.0.0/unsubscribe",
    "https://10.1.2.3/unsubscribe",
    "https://172.16.0.1/unsubscribe",
    "https://192.168.1.1/unsubscribe",
    "https://100.64.0.1/unsubscribe",
    "https://169.254.169.254/latest/meta-data/",
    "https://[::1]/unsubscribe",
    "https://[::ffff:127.0.0.1]/unsubscribe",
    "https://[fd00::1]/unsubscribe",
    "https://[fe80::1]/unsubscribe",
    "https://[2002:a00:1::1]/unsubscribe",
    "https://[2002:c0a8:101::1]/unsubscribe",
    "https://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/unsubscribe",
    "https://[64:ff9b::a00:1]/unsubscribe",
    "https://localhost/unsubscribe",
    "https://news.localhost/unsubscribe",
    "https://printer.local/unsubscribe",
    "https://db.internal/unsubscribe",
    "https://intranet/unsubscribe",
];

/// Checks that `one_click` refuses every URL in [`ONE_CLICK_REFUSED`] with
/// [`FetchError::Refused`]. Panics on the first violation. Needs no network.
pub async fn one_click_refusals(one_click: &dyn OneClick) {
    for url in ONE_CLICK_REFUSED {
        match one_click.post(url).await {
            Err(FetchError::Refused(_)) => {}
            other => panic!("{url} was not refused: {other:?}"),
        }
    }
}

/// What a [`OneClick`] under [`one_click_cases`] uses instead of the real network.
///
/// An implementation built from it resolves names only from `answers`, never finishes resolving
/// the names in `unanswered`, treats the addresses in
/// `public` as public on top of its own rules, connects to `port` instead of 443, trusts `roots`
/// and gives up after `timeout`. Everything else, including every refusal rule, stays as in
/// production.
#[derive(Debug, Clone)]
pub struct OneClickHarness {
    /// Every A and AAAA answer per host name; a host that is not listed fails to resolve.
    pub answers: Vec<(String, Vec<IpAddr>)>,
    /// Host names whose resolution never finishes, as with a resolver that does not answer.
    pub unanswered: Vec<String>,
    /// Addresses to treat as public.
    pub public: Vec<IpAddr>,
    /// The port the test server listens on, used in place of 443.
    pub port: u16,
    /// Roots that sign the test server's certificate.
    pub roots: TlsRoots,
    /// The timeout to use in place of the implementation's own.
    pub timeout: Duration,
}

const HOST: &str = "news.example.com";
const STALLED: &str = "stalled.example.com";
const TIMEOUT: Duration = Duration::from_secs(3);

/// Runs `one_click` implementations built by `make` against an HTTPS server in the process.
///
/// Checks refusal of mixed and all-private answers without connecting, unresolvable names and
/// certificates for another name as [`FetchError::Unavailable`], status classes, that redirects
/// are not followed, the multipart body and absent headers, that the timeout covers both a slow
/// server and a resolver that never answers, and that a large body is not an error. Panics on the first violation. Needs no network beyond loopback.
pub async fn one_click_cases(make: &dyn Fn(OneClickHarness) -> Box<dyn OneClick>) {
    let ca = TestCa::new(&[HOST]);
    let server = server::Server::start(&ca, TIMEOUT * 4).await;
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let private: IpAddr = "10.0.0.7".parse().expect("address");
    let ula: IpAddr = "fd00::7".parse().expect("address");
    let build = |answers: Vec<(&str, Vec<IpAddr>)>| {
        make(OneClickHarness {
            answers: answers
                .into_iter()
                .map(|(host, addrs)| (host.to_owned(), addrs))
                .collect(),
            unanswered: vec![STALLED.to_owned()],
            public: vec![loopback],
            port: server.port(),
            roots: ca.roots(),
            timeout: TIMEOUT,
        })
    };
    let url = |path: &str| format!("https://{HOST}{path}");

    for answers in [
        vec![loopback, private],
        vec![private, loopback],
        vec![private, ula],
    ] {
        let before = server.connections();
        match build(vec![(HOST, answers.clone())]).post(&url("/ok")).await {
            Err(FetchError::Refused(_)) => {}
            other => panic!("answers {answers:?} were not refused: {other:?}"),
        }
        assert_eq!(
            server.connections(),
            before,
            "connected despite {answers:?}"
        );
    }
    let one_click = build(vec![(HOST, vec![])]);
    match one_click.post(&url("/ok")).await {
        Err(FetchError::Refused(_)) => {}
        other => panic!("an empty answer was not refused: {other:?}"),
    }

    let one_click = build(vec![(HOST, vec![loopback])]);
    match one_click.post("https://missing.example.com/ok").await {
        Err(FetchError::Unavailable(_)) => {}
        other => panic!("an unknown host was not unavailable: {other:?}"),
    }

    let other = build(vec![("other.example.org", vec![loopback])]);
    match other.post("https://other.example.org/wrong-name").await {
        Err(FetchError::Unavailable(_)) => {}
        other => panic!("a certificate for another name was accepted: {other:?}"),
    }
    assert!(
        server.requests("/wrong-name").is_empty(),
        "a request went out over TLS for the wrong name"
    );

    for (path, outcome, status) in [
        ("/ok", OneClickOutcome::Accepted, 200),
        ("/status/202", OneClickOutcome::Accepted, 202),
        ("/status/204", OneClickOutcome::Accepted, 204),
        ("/redirect", OneClickOutcome::Redirected, 302),
        ("/status/301", OneClickOutcome::Redirected, 301),
        ("/status/404", OneClickOutcome::HttpError, 404),
        ("/status/500", OneClickOutcome::HttpError, 500),
        ("/large", OneClickOutcome::Accepted, 200),
    ] {
        match one_click.post(&url(path)).await {
            Ok(response) => {
                assert_eq!(response.outcome, outcome, "{path}");
                assert_eq!(response.status, status, "{path}");
            }
            Err(e) => panic!("{path} failed: {e:?}"),
        }
        assert_eq!(server.requests(path).len(), 1, "{path} was not sent once");
    }
    assert!(
        server.requests("/followed").is_empty(),
        "the redirect was followed"
    );

    let requests = server.requests("/ok");
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.header("host"), Some(HOST));
    for header in ["cookie", "authorization", "referer", "proxy-authorization"] {
        assert_eq!(request.header(header), None, "{header} was sent");
    }
    let content_type = request.header("content-type").unwrap_or_default();
    let boundary = content_type
        .strip_prefix("multipart/form-data")
        .and_then(|rest| {
            rest.split(';')
                .find_map(|p| p.trim().strip_prefix("boundary="))
        })
        .map(|b| b.trim_matches('"'))
        .unwrap_or_else(|| panic!("not a multipart body: {content_type}"));
    let body = String::from_utf8_lossy(&request.body);
    let parts: Vec<&str> = body
        .split(&format!("--{boundary}"))
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "--")
        .collect();
    assert_eq!(parts.len(), 1, "expected one part: {body}");
    let (head, value) = parts[0]
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("a part without headers: {body}"));
    assert!(
        head.to_ascii_lowercase()
            .contains("content-disposition: form-data; name=\"list-unsubscribe\""),
        "the part is not named List-Unsubscribe: {head}"
    );
    assert_eq!(value, "One-Click");
    assert!(
        body.trim_end().ends_with(&format!("--{boundary}--")),
        "the body is not closed: {body}"
    );

    let started = Instant::now();
    match one_click.post(&url("/slow")).await {
        Err(FetchError::Unavailable(_)) => {}
        other => panic!("a slow server did not time out: {other:?}"),
    }
    assert!(
        started.elapsed() < TIMEOUT * 3,
        "the timeout was not kept: {:?}",
        started.elapsed()
    );

    let started = Instant::now();
    match one_click.post(&format!("https://{STALLED}/ok")).await {
        Err(FetchError::Unavailable(_)) => {}
        other => panic!("a resolver that never answers did not time out: {other:?}"),
    }
    assert!(
        started.elapsed() < TIMEOUT * 3,
        "the timeout did not cover resolution: {:?}",
        started.elapsed()
    );
}

/// What a [`DataDownload`] under [`data_download_cases`] uses instead of its production settings.
///
/// An implementation built from it pins exactly `pinned_hosts` in place of its own list, resolves
/// every name in `loopback_hosts` to `127.0.0.1` (the port comes from the URL), trusts `roots` and
/// gives up after `timeout`. Its redirect limit and every other rule stay as in production.
#[derive(Debug, Clone)]
pub struct DataDownloadHarness {
    /// The hosts that may be contacted.
    pub pinned_hosts: Vec<String>,
    /// Names that resolve to the test server on loopback, pinned or not.
    pub loopback_hosts: Vec<String>,
    /// Roots that sign the test server's certificate.
    pub roots: TlsRoots,
    /// The timeout to use in place of the implementation's own.
    pub timeout: Duration,
}

const DATA_HOST: &str = "data.example.com";
const ASSET_HOST: &str = "assets.example.com";
const OUTSIDE_HOST: &str = "outside.example.org";
const SUB_HOST: &str = "mirror.data.example.com";
const MAX: u64 = 64 * 1024;

/// Runs `DataDownload` implementations built by `make` against an HTTPS server in the process.
///
/// The server answers for four names on loopback, of which `data.example.com` and
/// `assets.example.com` are pinned. Checks a `200` with its `ETag`, `If-None-Match` sent only
/// with an etag and on every hop, `304` as [`Download::NotModified`] (and as unavailable without
/// an etag), refusal without contact of non-https URLs, user info, an unpinned host and a
/// subdomain of a pinned one, each redirect status followed to a pinned host, redirects to an
/// unpinned host or to `http` refused with the target never contacted, the redirect limit of
/// two, bodies over `max_bytes` with and without `Content-Length`, other statuses as
/// unavailable, the timeout over a silent server and a stalled body, and that no cookie,
/// `Authorization`, `Referer` or proxy header is ever sent. Panics on the first violation. Needs
/// no network beyond loopback; run it on a multi-threaded runtime.
pub async fn data_download_cases(make: &dyn Fn(DataDownloadHarness) -> Box<dyn DataDownload>) {
    let ca = TestCa::new(&[DATA_HOST, ASSET_HOST, OUTSIDE_HOST, SUB_HOST]);
    let server = server::Server::start(&ca, TIMEOUT * 4).await;
    let download = make(DataDownloadHarness {
        pinned_hosts: vec![DATA_HOST.to_owned(), ASSET_HOST.to_owned()],
        loopback_hosts: [DATA_HOST, ASSET_HOST, OUTSIDE_HOST, SUB_HOST]
            .map(str::to_owned)
            .to_vec(),
        roots: ca.roots(),
        timeout: TIMEOUT,
    });
    let port = server.port();
    let url = |host: &str, path: &str| format!("https://{host}:{port}/data/{path}");
    let fetched = |result: Result<Download, FetchError>, case: &str| match result {
        Ok(Download::Fetched { bytes, etag }) => (bytes, etag),
        other => panic!("{case}: expected the file, got {other:?}"),
    };
    let refused = |result: Result<Download, FetchError>, case: &str| match result {
        Err(FetchError::Refused(_)) => {}
        other => panic!("{case}: expected a refusal, got {other:?}"),
    };
    let unavailable = |result: Result<Download, FetchError>, case: &str| match result {
        Err(FetchError::Unavailable(_)) => {}
        other => panic!("{case}: expected unavailable, got {other:?}"),
    };
    let last_to = |host: &str| {
        server
            .requests_to(host)
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("nothing reached {host}"))
    };

    let (bytes, etag) = fetched(
        download.get(&url(DATA_HOST, "file"), None, MAX).await,
        "200",
    );
    assert_eq!(&bytes[..], server::DATA_BODY);
    assert_eq!(etag.as_deref(), Some(server::DATA_ETAG));
    let request = last_to(DATA_HOST);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/data/file", "not a direct request");
    assert_eq!(
        request.header("if-none-match"),
        None,
        "If-None-Match without an etag"
    );

    let (bytes, etag) = fetched(
        download
            .get(&url(DATA_HOST, "file"), Some("\"v0-old\""), MAX)
            .await,
        "a stale etag",
    );
    assert_eq!(&bytes[..], server::DATA_BODY);
    assert_eq!(etag.as_deref(), Some(server::DATA_ETAG));
    assert_eq!(
        last_to(DATA_HOST).header("if-none-match"),
        Some("\"v0-old\"")
    );

    match download
        .get(&url(DATA_HOST, "file"), Some(server::DATA_ETAG), MAX)
        .await
    {
        Ok(Download::NotModified) => {}
        other => panic!("a matching etag was not NotModified: {other:?}"),
    }
    assert_eq!(
        last_to(DATA_HOST).header("if-none-match"),
        Some(server::DATA_ETAG)
    );
    unavailable(
        download
            .get(&url(DATA_HOST, "unconditional-304"), None, MAX)
            .await,
        "a 304 without an etag",
    );
    refused(
        download
            .get(&url(DATA_HOST, "file"), Some("bad\r\netag"), MAX)
            .await,
        "an etag that is no header value",
    );

    let (_, etag) = fetched(
        download.get(&url(DATA_HOST, "untagged"), None, MAX).await,
        "no ETag",
    );
    assert_eq!(etag, None, "an etag that was never sent");
    let (_, etag) = fetched(
        download.get(&url(DATA_HOST, "odd-tag"), None, MAX).await,
        "a non-ASCII ETag",
    );
    assert_eq!(etag, None, "a non-ASCII etag was returned");

    for (case, target) in [
        ("http", format!("http://{DATA_HOST}:{port}/data/file")),
        ("not a url", "not a url".to_owned()),
        (
            "user info",
            format!("https://user:secret@{DATA_HOST}:{port}/data/file"),
        ),
        ("an unpinned host", url(OUTSIDE_HOST, "file")),
        ("a subdomain of a pinned host", url(SUB_HOST, "file")),
        (
            "an IP address",
            format!("https://127.0.0.1:{port}/data/file"),
        ),
    ] {
        let before = server.connections();
        refused(download.get(&target, None, MAX).await, case);
        assert_eq!(
            server.connections(),
            before,
            "{case}: a connection was made"
        );
    }
    let upper = format!("https://DATA.Example.COM:{port}/data/file");
    fetched(download.get(&upper, None, MAX).await, "an upper-case host");

    for code in [301, 302, 303, 307, 308] {
        let (bytes, etag) = fetched(
            download
                .get(
                    &url(DATA_HOST, &format!("redirect/{code}/{ASSET_HOST}/file")),
                    None,
                    MAX,
                )
                .await,
            &format!("a {code} to a pinned host"),
        );
        assert_eq!(&bytes[..], server::DATA_BODY);
        assert_eq!(etag.as_deref(), Some(server::DATA_ETAG));
        let request = last_to(ASSET_HOST);
        assert_eq!(request.path, "/data/file");
        assert_eq!(request.method, "GET");
    }
    match download
        .get(
            &url(DATA_HOST, &format!("redirect/302/{ASSET_HOST}/file")),
            Some(server::DATA_ETAG),
            MAX,
        )
        .await
    {
        Ok(Download::NotModified) => {}
        other => panic!("a matching etag behind a redirect was not NotModified: {other:?}"),
    }
    assert_eq!(
        last_to(ASSET_HOST).header("if-none-match"),
        Some(server::DATA_ETAG),
        "If-None-Match was not sent after the redirect"
    );

    let two = format!("redirect/302/{ASSET_HOST}/redirect/307/{DATA_HOST}/file");
    fetched(
        download.get(&url(DATA_HOST, &two), None, MAX).await,
        "two redirects",
    );
    let before = server.all_requests().len();
    let three = format!(
        "redirect/301/{ASSET_HOST}/redirect/302/{DATA_HOST}/redirect/308/{ASSET_HOST}/file"
    );
    refused(
        download.get(&url(DATA_HOST, &three), None, MAX).await,
        "three redirects",
    );
    assert_eq!(
        server.all_requests().len(),
        before + 3,
        "the target of the third redirect was requested"
    );

    for (case, path, host) in [
        (
            "a redirect to an unpinned host",
            format!("redirect/302/{OUTSIDE_HOST}/file"),
            OUTSIDE_HOST,
        ),
        (
            "a redirect to a subdomain",
            format!("redirect/301/{SUB_HOST}/file"),
            SUB_HOST,
        ),
        (
            "a redirect to http",
            format!("insecure/302/{ASSET_HOST}/file"),
            ASSET_HOST,
        ),
    ] {
        let before = server.connections();
        let reached = server.requests_to(host).len();
        refused(download.get(&url(DATA_HOST, &path), None, MAX).await, case);
        assert_eq!(
            server.connections(),
            before + 1,
            "{case}: the target was contacted"
        );
        assert_eq!(
            server.requests_to(host).len(),
            reached,
            "{case}: the target was requested"
        );
    }

    let exact = server::DATA_BODY.len() as u64;
    fetched(
        download.get(&url(DATA_HOST, "file"), None, exact).await,
        "a body of exactly max_bytes",
    );
    refused(
        download.get(&url(DATA_HOST, "file"), None, exact - 1).await,
        "a body one byte over max_bytes",
    );
    refused(
        download.get(&url(DATA_HOST, "large"), None, MAX).await,
        "a large body",
    );
    refused(
        download
            .get(&url(DATA_HOST, "large-unsized"), None, MAX)
            .await,
        "a large body without Content-Length",
    );

    for code in [300, 404, 500, 503] {
        unavailable(
            download
                .get(&url(DATA_HOST, &format!("status/{code}")), None, MAX)
                .await,
            &format!("status {code}"),
        );
    }

    for path in ["/slow".to_owned(), "/data/stalled-body".to_owned()] {
        let started = Instant::now();
        unavailable(
            download
                .get(&format!("https://{DATA_HOST}:{port}{path}"), None, MAX)
                .await,
            &format!("{path} within the timeout"),
        );
        assert!(
            started.elapsed() < TIMEOUT * 3,
            "{path}: the timeout was not kept: {:?}",
            started.elapsed()
        );
    }

    for request in server.all_requests() {
        for header in ["cookie", "authorization", "referer", "proxy-authorization"] {
            assert_eq!(
                request.header(header),
                None,
                "{header} was sent to {}",
                request.path
            );
        }
    }
}
