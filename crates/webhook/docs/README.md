# holtmail-webhook

Contract for delivering webhook requests: one `POST` to an address the instance's owner configured, with a body and headers the caller made. What to send, how it is signed and when to try again belong to the caller ([`holtmail-hooks`](../../hooks/docs/README.md) and the server); a sender only carries one request and reports the answer.

## API

- `WebhookRequest { url, headers, body }`.
- `WebhookSender::send(&request) -> Result<u16, WebhookError>`: one `POST` with `Content-Type: application/json`, the headers as given and the body byte for byte. Any answer is `Ok(status)`, `4xx` and `5xx` too; redirects are not followed (a `3xx` is returned); the answer's body is not kept; no cookies.
- `WebhookError::{Invalid, Unreachable, TimedOut}`: not an `http`/`https` URL (sending again gives the same answer), no connection or TLS failed, no answer within the sender's timeout.

## Writing an implementation

Depend on `holtmail-webhook` with `features = ["testing"]` under `[dev-dependencies]`, run a local HTTP server that implements `testing::Receiver` (`url(path, status)`, `redirect(path, target)`, `closed()`, `received()`), and call `testing::conformance(&sender, &receiver)`. It checks bodies and headers arrive unchanged with a JSON content type and no cookie, that 200, 204, 410 and 500 come back as statuses, that a redirect is not followed, that a closed port is `Unreachable`, and that non-HTTP addresses are `Invalid`.

## Implementations

- [`holtmail-webhook-http`](../../webhook-http/docs/README.md): reqwest and rustls.
