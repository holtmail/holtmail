# holtmail-fetch-http

A [`Fetcher`](../../fetch/docs/README.md) over HTTP and HTTPS and a [`OneClick`](../../fetch/docs/README.md#oneclick) over HTTPS that only ever connect to public addresses, and a [`DataDownload`](../../fetch/docs/README.md#datadownload) over HTTPS that only ever connects to pinned hosts. The image proxy and the remote image saver use `HttpFetcher` to download images that messages link to; the unsubscribe route uses `HttpOneClick` to send one-click requests; the category data updater uses `HttpDataDownload` to fetch signed data files from GitHub releases.

## Usage

```rust
let fetcher = HttpFetcher::new(Options::default())?;
let image = fetcher.image("https://example.com/logo.png").await?;
```

`Options { max_bytes, timeout, max_redirects, proxy, roots }`, by default 10 MiB, 15 seconds for the whole request with redirects (5 seconds to connect), 5 redirects, no proxy and no extra roots. `roots` are CA certificates trusted for https besides the system's roots (Holtmail's `tls_ca_file`); verification stays on, and an unusable certificate fails `new`. `is_public(ip)` tells whether an address is on the public internet. `proxy_address(url)` checks a proxy URL and returns `ProxyAddress { scheme, host, port }`, which prints as `scheme://host:port` without credentials.

## Outbound proxy

`proxy` sends every request through `http://`, `https://`, `socks5://` or `socks5h://`, with an optional `user:password@`, so senders see the proxy's address. The checks below still run before anything is sent. With `socks5h` host names are resolved by the proxy (as Tor needs) and never looked up locally, so names are only checked against the local-name list and the proxy decides where it can connect. With the other schemes names are resolved and checked locally first, but the proxy resolves them again when connecting.

## How it stays out of private networks

- The URL must be `http` or `https`, without credentials. Host names such as `localhost`, `*.local`, `*.internal` or names without a dot are refused.
- Addresses written in the URL, including decimal (`http://2130706433/`) and IPv4-mapped IPv6 forms, must be public.
- Host names are resolved before the request, and again by the client's own resolver, which drops every non-public address. A name that resolves only to private addresses, or switches to one between the two lookups, cannot be reached.
- Redirect targets go through the same checks, and so does the resolver.
- No proxy from the environment, no cookies, no `Referer`. The user agent is `Holtmail/<version>`.

Not public: unspecified, loopback, private (RFC 1918), carrier-grade NAT (`100.64/10`), link-local (`169.254/16`, `fe80::/10`, so cloud metadata endpoints too), documentation, benchmarking, multicast, broadcast and reserved ranges, unique-local IPv6 (`fc00::/7`), site-local, the NAT64 prefix (`64:ff9b::/32`) and Teredo (`2001::/32`, whose embedded addresses cannot be checked). IPv4-mapped addresses (`::ffff:a.b.c.d`) and 6to4 addresses (`2002:AABB:CCDD::/48`, the IPv4 address in the second and third groups) count as the IPv4 address they carry, so `2002:a00:1::1` (10.0.0.1) is refused and `2002:5db8:d822::1` (93.184.216.34) is public. These rules apply to images and one-click alike.

## One-click unsubscribe

```rust
let one_click = HttpOneClick::new(OneClickOptions::default())?;
let response = one_click.post("https://news.example.com/unsubscribe/abc").await?;
```

`OneClickOptions { timeout, max_body, roots }`, by default 10 seconds for the whole call, name resolution and the response body included, 64 KiB of the response read and then discarded, and no extra roots. An unusable root fails `new`.

Each request:

1. The URL must be `https`, on port 443 (explicit or default), without user name or password, with a host that is not a local name (as above); an IP literal must be public.
2. The host is resolved with the system resolver. Every A and AAAA answer must be public; one private answer, or none at all, refuses the request before anything is contacted.
3. A client is built for this request only: no proxy, `https` only, no redirects, no `Referer`, no cookie store, the timeout, the extra roots, user agent `Holtmail/<version>`, and the host pinned with `resolve_to_addrs` to exactly the checked addresses on port 443. Any other name the client tried to resolve would fail. SNI and certificate verification use the URL's host name.
4. One `POST` with `Content-Type: multipart/form-data; boundary=…` and one part, `List-Unsubscribe` = `One-Click`.
5. 2xx is `Accepted`, 3xx `Redirected` (not followed), anything else `HttpError`, each with the status. Up to `max_body` bytes of the body are read and dropped; read errors after the status are ignored.

Failures to resolve, connect, finish TLS or answer in time are `Unavailable`; everything refused in steps 1 and 2 is `Refused`. Error texts are short reasons, never the URL or host.

**No proxy, by design.** The image fetcher's `proxy` setting does not apply here. A `socks5h` or `http` proxy resolves the name itself and connects wherever that answer points, so the server could not hold the request to public addresses; for the one request that a user explicitly asks for, the check matters more than hiding the server's address.

### The `test-networks` feature

With the cargo feature `test-networks`, `OneClickOptions` gains `allowed_test_networks: Vec<(IpAddr, u8)>` (empty by default) and the crate exports `parse_cidr(&str) -> Result<(IpAddr, u8), String>`, which reads `11.88.77.0/24`, `fd00:1::/64` or a bare address (a single-address network). Addresses inside those networks count as public for one-click requests only, never for `HttpFetcher`. Without the feature the field, the function and the check do not exist, so a release build cannot be configured to reach private addresses. The server forwards its `insights-test-data` feature to this one for the integration stack, whose test servers sit on private addresses.

## Data downloads

```rust
let download = HttpDataDownload::new(DataDownloadOptions { roots, ..DataDownloadOptions::default() })?;
let result = download.get("https://github.com/example/holtmail/releases/download/categories-latest/manifest.json", Some(&etag), 1 << 20).await?;
```

`DataDownloadOptions { roots, pinned_hosts, timeout, max_redirects }`, by default no extra roots, `GITHUB_RELEASE_HOSTS` (`github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`), 60 seconds for the whole call and 2 redirects. An unusable root fails `new`.

- One client for the downloader: no proxy (`no_proxy()`, so `HTTPS_PROXY` and friends are ignored, and there is no proxy option), `https` only, no cookie store, no `Referer`, rustls with the built-in roots plus `roots`, user agent `Holtmail/<version>`.
- The URL is checked before anything is sent: `https`, no user info, a host in `pinned_hosts` (exact, case-insensitive). The same check runs in the redirect policy on every hop, with the hop count, so an unpinned target is never contacted.
- `If-None-Match` goes on the first request and reqwest keeps it on redirects, so the asset host can answer `304`.
- `Content-Length` over `max_bytes` is refused before the body is read; otherwise chunks are added up and the call is refused as soon as they pass the cap.
- The client timeout and an outer timeout both cover the whole call. Error texts are short reasons and statuses, never the URL, the host or the body.

**GitHub releases.** A release asset URL on `github.com` answers `302` to a short-lived signed URL on `objects.githubusercontent.com` or `release-assets.githubusercontent.com`; that response carries the asset's `ETag` and answers `If-None-Match` with `304`. That is why the default allows one more hop than GitHub uses, and why the etag goes on every hop rather than only the first.

**Configurable hosts.** `pinned_hosts` (and so the base URL) is a plain option here, with no test-only feature in this crate. Only the server decides whether its config may change them, under its `insights-test-data` feature for the integration stack; a release server always uses the default.

## Limits

- The `Content-Type` must be `image/*`. Without one, PNG, JPEG, GIF and WebP are recognized by their first bytes; anything else is refused.
- A `Content-Length` above `max_bytes`, or a body that grows past it, is refused without reading further.
- Server errors, timeouts and unknown hosts are `Unavailable`; everything above is `Refused`. Error texts are short reasons, never the URL.

## Tests

`HttpDataDownload` runs `data_download_cases`; the harness's loopback names go through reqwest's `resolve` overrides in a test-only constructor. Five deliberately broken variants (etag dropped, size cap ignored, three redirects allowed, an unpinned host pinned, a 30 s timeout) each fail the suite under `#[should_panic]`, and unit tests cover the default hosts and the URL check.

`HttpOneClick` runs `one_click_refusals` and `one_click_cases` from `holtmail_fetch::testing`. The hooks the harness needs (fixed resolver answers, names that never resolve, extra public addresses, the server's port instead of 443) exist only under `#[cfg(test)]`. With `--features test-networks`, a test reaches a loopback server inside an allowed network and is refused for an address outside it, and for the same server when the list is empty or does not cover it.

The fetcher contract's conformance suite runs without network: every refused URL fails before any connection. Unit tests cover the address ranges and content sniffing. One test fetches through a SOCKS5 proxy in the process (`socks5h`, so the name is never resolved locally) from a TLS server whose test CA is trusted only through `roots`; it also proves the proxy is used at all.
