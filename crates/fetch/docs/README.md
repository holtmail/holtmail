# holtmail-fetch

The contract for outbound requests that message content can cause: fetching remote images linked from messages (`Fetcher`) and RFC 8058 one-click unsubscribe requests (`OneClick`). The reader's image proxy, the archiver and the unsubscribe route all go through it, so a single module decides which addresses the server may contact. Every URL in a message comes from its sender and is untrusted. It also holds `DataDownload`, through which the server fetches its own data files (the domain category tables) from a pinned list of hosts.

## Fetcher

```rust
#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn image(&self, url: &str) -> Result<RemoteImage, FetchError>;
}
```

`RemoteImage { content_type, bytes }`: a lowercase `image/*` type without parameters and the bytes as served.

`image(url)` guarantees:

- Only `http` and `https`.
- No connection to loopback, private, link-local, unique-local, carrier-grade NAT or other non-public addresses, whether the URL names them, a host name resolves to them or a redirect leads there.
- Only `image/*` content, up to the implementation's size limit, within its timeout.
- No cookies, no referrer.

`FetchError::Refused` means asking again gives the same answer: a bad scheme, a private address, not an image, too large. `FetchError::Unavailable` means the server could not be reached or answered with an error, and it may work later.

## OneClick

```rust
#[async_trait]
pub trait OneClick: Send + Sync {
    async fn post(&self, url: &str) -> Result<OneClickResponse, FetchError>;
}

pub struct OneClickResponse { pub outcome: OneClickOutcome, pub status: u16 }
pub enum OneClickOutcome { Accepted, Redirected, HttpError }
impl OneClickOutcome { pub fn from_status(status: u16) -> Self }
```

`post(url)` sends one `POST` and guarantees:

- Only `https` on port 443 (explicit or default), no user name or password, and a host that is a name or a public IP address; local names such as `localhost`, `*.local`, `*.internal` and names without a dot are refused.
- Never through a proxy.
- Every A and AAAA answer must be public, or the request is refused before any connection. An empty answer is refused too.
- The connection goes only to the checked addresses on port 443, and TLS verifies the certificate for the URL's host name.
- Redirects are not followed: a 3xx is `Redirected` and its target is never requested.
- No cookies, `Authorization` or `Referer`.
- The body is one `multipart/form-data` part named `List-Unsubscribe` with the value `One-Click`.
- The implementation's timeout (10 s by default) ends the request with `Unavailable`. At most its body limit (64 KiB by default) of the response is read and discarded; a longer body is not an error.
- At most one request per call, never retried.

How results map onto the stored unsubscribe outcomes:

| Result | Stored outcome |
| --- | --- |
| `Ok` with `OneClickOutcome::Accepted` (2xx) | `accepted` |
| `Ok` with `OneClickOutcome::Redirected` (3xx) | `redirected` |
| `Ok` with `OneClickOutcome::HttpError` (any other status) | `http_error` |
| `Err(FetchError::Refused)` (URL or address not allowed, nothing contacted) | `refused` |
| `Err(FetchError::Unavailable)` (name did not resolve, connect or TLS failed, timeout) | `network` |

## DataDownload

```rust
#[async_trait]
pub trait DataDownload: Send + Sync {
    async fn get(&self, url: &str, etag: Option<&str>, max_bytes: u64) -> Result<Download, FetchError>;
}

pub enum Download {
    NotModified,
    Fetched { bytes: Bytes, etag: Option<String> },
}
```

Its URLs come from the server's configuration, never from mail. `get(url, etag, max_bytes)` guarantees:

- Only `https`, without user name or password, on whatever port the URL names.
- The URL's host and every redirect target's host must be in the implementation's pinned list: exact and case-insensitive, never a subdomain of a listed host. Otherwise `Refused`, and that host is never contacted.
- At most the implementation's redirect limit (2 by default) of 301, 302, 303, 307 and 308 is followed; one more is `Refused` and its target is not requested. A redirect to `http`, to an unpinned host or with user info is refused the same way.
- Never through a proxy, from configuration or the environment.
- No cookies (a `Set-Cookie` on a redirect is ignored), no `Authorization`, no `Referer`.
- With an `etag`, every request of the call carries `If-None-Match: <etag>` verbatim, so the host that serves the file after a redirect sees it too; without one, none does. An `etag` that is not a valid header value is `Refused`.
- `304` is `NotModified` when an `etag` was sent, and `Unavailable` otherwise. A 2xx is `Fetched`. Any other status, a failed connection or TLS handshake and an unknown host are `Unavailable`.
- A body over `max_bytes`, announced by `Content-Length` or found while reading, is `Refused`; no more than `max_bytes` is held in memory.
- The timeout (60 s by default) covers the whole call, redirects and body included, and ends it with `Unavailable`.
- The returned `etag` is the final response's `ETag` verbatim when it is non-empty printable ASCII, otherwise `None`.
- At most one request per hop, never retried.

## Writing an implementation

Enable the `testing` feature in `[dev-dependencies]`. It brings in `tokio` and `holtmail-core/testing` for the test server; a build without it does no I/O.

- **Fetcher:** call `holtmail_fetch::testing::conformance(&fetcher)`. It checks that every URL in `testing::REFUSED` is refused; it needs no network.
- **OneClick, refusals:** call `holtmail_fetch::testing::one_click_refusals(&one_click)` on an instance with default settings. Every URL in `testing::ONE_CLICK_REFUSED` (other schemes, userinfo, other ports, private, loopback, link-local, unique-local, 6to4 of private, Teredo and NAT64 literals, local names) must be `Refused` without resolving anything.
- **OneClick, against a server:** call `holtmail_fetch::testing::one_click_cases(&make)` from a multi-threaded tokio test, where `make: &dyn Fn(OneClickHarness) -> Box<dyn OneClick>` builds an instance for each case. `OneClickHarness { answers, unanswered, public, port, roots, timeout }` says what the instance uses instead of the real network: it resolves names only from `answers` (a host not listed fails to resolve), never finishes resolving the names in `unanswered`, treats the addresses in `public` as public on top of its own rules, connects to `port` instead of 443, trusts `roots` and uses `timeout` (one second). Every other rule stays as in production, so the hooks this needs must exist only in test builds. The suite runs an HTTPS server on loopback with a certificate for `news.example.com` and checks: mixed and all-private answers refused without a connection, an empty answer refused, an unknown host and a certificate for another name `Unavailable`, 2xx, 3xx and other statuses classified with the status kept, the redirect target never requested, the request is a `POST` with the multipart body and no `Cookie`, `Authorization`, `Referer` or `Proxy-Authorization`, a slow server and a resolver that never answers both give `Unavailable` within the timeout, and a 1 MiB body is still `Accepted`.

- **DataDownload:** call `holtmail_fetch::testing::data_download_cases(&make)` from a multi-threaded tokio test, where `make: &dyn Fn(DataDownloadHarness) -> Box<dyn DataDownload>` builds the instance. `DataDownloadHarness { pinned_hosts, loopback_hosts, roots, timeout }`: the instance pins exactly `pinned_hosts`, resolves every name in `loopback_hosts` to `127.0.0.1` (the port comes from the URL), trusts `roots` and uses `timeout` (one second); its redirect limit stays the production one. The suite serves HTTPS on loopback for `data.example.com` and `assets.example.com` (pinned) and `outside.example.org` and `mirror.data.example.com` (not pinned), and checks: a `200` with its body and `ETag`; `If-None-Match` absent without an etag and present with one, also after a redirect; a matching etag answered `304` as `NotModified`, directly and behind a redirect; a `304` without an etag `Unavailable`; an etag with a line break refused; a missing or non-ASCII `ETag` returned as `None`; `http`, non-URLs, user info, an unpinned host, a subdomain of a pinned host and an IP literal refused without a connection; an upper-case host accepted; 301, 302, 303, 307 and 308 to a pinned host followed; two redirects followed and a third refused with its target never requested; redirects to an unpinned host, a subdomain or `http` refused with the target never contacted; a body of exactly `max_bytes` accepted and one byte more, 1 MiB with and without `Content-Length` refused; 300, 404, 500 and 503 `Unavailable`; a silent server and a stalled body `Unavailable` within the timeout; no `Cookie`, `Authorization`, `Referer` or `Proxy-Authorization` on any request. `holtmail-fetch-http` proves the suite catches broken implementations: one that drops the etag, one that ignores `max_bytes`, one that follows three redirects, one that pins an extra host and one that ignores the timeout each fail it.

## Implementations

- [holtmail-fetch-http](../../fetch-http/docs/README.md) — HTTP with a resolver that only returns public addresses
