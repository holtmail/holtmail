# holtmail-webhook-http

A [`WebhookSender`](../../webhook/docs/README.md) on reqwest and rustls.

## Config

`HttpWebhookSender::new(timeout)` (the server uses 10 seconds) and `with_roots(&TlsRoots)` for extra trusted certificates (`tls_ca_file` in the server config). The webhook address comes from the admin who added it, so private and loopback addresses are allowed, unlike remote images; no proxy is used.

## Limits

No redirects, no cookies, the answer's body is dropped unread. Errors carry no URL (`without_url`), so logs and the delivery list never show an address with its path.

## Tests

`cargo test -p holtmail-webhook-http`: the contract's conformance suite against an axum receiver on loopback, and a receiver that accepts the connection but never answers (`TimedOut` after 300 ms).
