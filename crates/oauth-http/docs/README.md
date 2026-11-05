# holtmail-oauth-http

The [`TokenEndpoint`](../../oauth/docs/README.md#tokenendpoint) of `holtmail-oauth` over HTTPS, on reqwest with rustls.

```rust
let endpoint = HttpTokenEndpoint::new(Duration::from_secs(30))?;
let tokens = client.refresh(&endpoint, &refresh_token).await?;
```

- Only `https` URLs are contacted; plain `http` only for loopback addresses (`127.0.0.0/8`, `::1`, `localhost`), so tests can run an endpoint in the process. Anything else fails with `OAuthError::Connection` before any connection.
- Redirects are not followed; the timeout covers the whole request.
- The form is encoded as `application/x-www-form-urlencoded`; the answer is returned with its status, whatever it is, up to 64 KiB.
- Errors name only the host ("could not be reached", "did not answer in time"); they never repeat the form or the answer.

The outbound image proxy does not apply: token requests go directly to the provider.

## Tests

`holtmail_oauth::testing::conformance` plus a one-shot HTTP server on a loopback port that checks the request line, content type and form encoding, and a code exchange through `holtmail_oauth::Client`. No test leaves the machine.
