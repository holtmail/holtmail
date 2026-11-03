# holtmail-fetch-http

A [`Fetcher`](../../fetch/docs/README.md) over HTTP and HTTPS that only ever connects to public addresses. The image proxy and the remote image saver use it to download images that messages link to.

## Usage

```rust
let fetcher = HttpFetcher::new(Options::default())?;
let image = fetcher.image("https://example.com/logo.png").await?;
```

`Options { max_bytes, timeout, max_redirects }`, by default 10 MiB, 15 seconds for the whole request with redirects (5 seconds to connect) and 5 redirects. `is_public(ip)` tells whether an address is on the public internet.

## How it stays out of private networks

- The URL must be `http` or `https`, without credentials. Host names such as `localhost`, `*.local`, `*.internal` or names without a dot are refused.
- Addresses written in the URL, including decimal (`http://2130706433/`) and IPv4-mapped IPv6 forms, must be public.
- Host names are resolved before the request, and again by the client's own resolver, which drops every non-public address. A name that resolves only to private addresses, or switches to one between the two lookups, cannot be reached.
- Redirect targets go through the same checks, and so does the resolver.
- No proxy from the environment, no cookies, no `Referer`. The user agent is `Holtmail/<version>`.

Not public: unspecified, loopback, private (RFC 1918), carrier-grade NAT (`100.64/10`), link-local (`169.254/16`, `fe80::/10`, so cloud metadata endpoints too), documentation, benchmarking, multicast, broadcast and reserved ranges, unique-local IPv6 (`fc00::/7`), site-local and the NAT64 prefix.

## Limits

- The `Content-Type` must be `image/*`. Without one, PNG, JPEG, GIF and WebP are recognized by their first bytes; anything else is refused.
- A `Content-Length` above `max_bytes`, or a body that grows past it, is refused without reading further.
- Server errors, timeouts and unknown hosts are `Unavailable`; everything above is `Refused`. Error texts are short reasons, never the URL.

## Tests

The fetcher contract's conformance suite runs without network: every refused URL fails before any connection. Unit tests cover the address ranges and content sniffing.
