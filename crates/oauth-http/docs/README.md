# holtmail-oauth-http

The [`TokenEndpoint`](../../oauth/docs/README.md#tokenendpoint) of `holtmail-oauth` over HTTPS, on reqwest with rustls.

```rust
let endpoint = HttpTokenEndpoint::new(Duration::from_secs(30))?.with_roots(&roots)?;
let tokens = client.refresh(&endpoint, &refresh_token).await?;
```

- Only `https` URLs are contacted; plain `http` only for loopback addresses (`127.0.0.0/8`, `::1`, `localhost`), so tests can run an endpoint in the process. Anything else fails with `OAuthError::Connection` before any connection.
- `with_roots(&TlsRoots)` trusts the given CA certificates (Holtmail's `tls_ca_file`) besides the system's roots, for a token endpoint with a private CA; verification stays on. An unusable certificate is a `Connection` error.
- Redirects are not followed; the timeout covers the whole request.
- The form is encoded as `application/x-www-form-urlencoded`; the answer is returned with its status, whatever it is, up to 64 KiB.
- Errors name only the host ("could not be reached", "did not answer in time"); they never repeat the form or the answer.

The outbound image proxy does not apply: token requests go directly to the provider.

## Tests

`holtmail_oauth::testing::conformance` plus a one-shot HTTP server on a loopback port that checks the request line, content type and form encoding, a code exchange through `holtmail_oauth::Client`, and a TLS endpoint in the process whose test CA is trusted only through `with_roots`. No test leaves the machine.
