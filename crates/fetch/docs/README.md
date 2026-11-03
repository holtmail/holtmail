# holtmail-fetch

The contract for fetching remote images linked from messages. The reader's image proxy and the archiver both go through a `Fetcher`, so a single module decides which addresses the server may contact. Every URL in a message comes from its sender and is untrusted.

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

## Writing an implementation

Enable the `testing` feature in `[dev-dependencies]` and call `holtmail_fetch::testing::conformance(&fetcher)` from a test. The suite checks that every URL in `testing::REFUSED` is refused; it needs no network.

## Implementations

- [holtmail-fetch-http](../../fetch-http/docs/README.md) — HTTP with a resolver that only returns public addresses
