# holtmail-serve-submit

SMTP submission for the user's own mail clients. A client signs in with a device's app password and what it submits goes through `Submission`, so it meets the same locks, identities and limits as a send from the web app, and leaves through a configured transport. A protocol front end, not a contract implementation: it receives `Submission` and `DeviceAuth`, holds no writer, transport or store handle, and only `apps/server` builds it, behind `[serve.submit]`. Design in [SERVE.md](../../../docs/SERVE.md#smtp-submission).

## API

```rust
SubmitServer::new(submission: Arc<dyn Submission>, devices: Arc<dyn DeviceAuth>, settings: Settings) -> Arc<Self>
submit_server.serve(listener, tls: PortTls, proxy_protocol, trusted_proxies, limiter).await
pub struct Settings { hostname, instance_token, undo_window, sign_in_timeout, idle_timeout }
pub enum PortTls { Implicit(TlsCertificates), Starttls(Option<TlsCertificates>) }
pub const MAX_SIZE: usize; // 50 MiB
```

`Settings::new(hostname)` gives no undo window, 60 seconds before sign-in and 30 minutes after. `Starttls(None)` is only for listeners on loopback addresses. Listeners of one server share one `ConnectionLimiter`.

## What it guarantees

1. **Sign-in:** `AUTH PLAIN` or `LOGIN` with an app password, verified as `DeviceProtocol::Smtp`, so the device needs `smtp`, `send:mail` and a readable mail archive. Only over TLS, or from a client on this machine. On a `STARTTLS` port `MAIL` is refused before TLS, except from loopback. A signed-in connection moves to its user's place in the connection limits; over the limit it gets `421`. Revoking the device ends the session with `421`.
2. **`MAIL FROM`:** `Submission::sender`. The null reverse path is refused.
3. **`DATA`:** the message goes to `Submission::queue` as the identity of the return path, with these changes only: `Date` and `Message-ID` (`<uuidv7@identity-domain>`) when missing or empty, `Bcc` and `Resent-Bcc` removed with their folded lines. Every other byte stays, and no `Received:` is added. Envelope recipients are kept, so a Bcc-only recipient still gets the message.
4. **Retries:** the idempotency key is a hash of the device, the lowercased recipients and the client's `Message-ID`, or the bytes when it has none. A retry after a lost `250` is one message, and one message sent in several transactions, such as one for the Bcc recipients, still reaches them all.
5. **The undo window** is `Settings::undo_window`.
6. **The loop guard:** the greeting carries `holtmail-instance=<token>`, which `submit-smtp` refuses.

## Replies

| Refused for | Reply |
| --- | --- |
| Not signed in | `530 5.7.0` |
| Wrong return path, null return path, a closed lock (`Forbidden`) | `553 5.7.1` |
| Used-up limits (`RateLimited`) | `451 4.7.1` |
| Header `From` is not exactly the identity, or the field is repeated | `554 5.7.1` |
| A retry of a message that was cancelled in the meantime | `554 5.7.1` |
| Leak guard | `550 5.7.1`, naming nobody |
| Larger than the limit | `552` |
| The message cannot be sent as given (`Invalid`) | `554 5.6.0` |
| Anything else | `451 4.3.0` |

## Limits and quirks

- `SIZE` and `SMTPUTF8` come from the transports the user may use. Before sign-in nothing is known about the user: `EHLO` says 50 MiB and no `SMTPUTF8`. A client that does not repeat `EHLO` after `AUTH` sees those; the real limits are enforced at `MAIL` and `DATA`.
- `DSN`, `CHUNKING` and `BINARYMIME` are not offered.
- Failures after `250` reach the user as a report in the archive, made by the outbox ([holtmail-outbox](../../outbox/docs/README.md#reports-for-mail-clients)).
- Logs carry device and outbox ids and counts, never addresses, subjects or secrets.

## Tests

`cargo test -p holtmail-serve-submit` drives real loopback sockets against `FakeSubmission` with SMTP dialogues (Bcc removal, RFC 6409 additions, a retry being one message, split transactions, wrong `MAIL FROM`, null return path, wrong `From`, `SIZE`, `SMTPUTF8`, used-up limits, the leak guard, revocation, the per-user connection limit, `STARTTLS` required of a remote client through PROXY v2) and `submit-smtp` as the client over plain loopback, `STARTTLS` and implicit TLS, including the loop guard. `tests/integration/tests/serve_submit.rs` runs the real binary against GreenMail with `submit-smtp` and curl.
