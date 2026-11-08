# holtmail-submit-smtp

A `holtmail_submit::Submitter` over SMTP submission (RFC 6409): implicit TLS on 465 or `STARTTLS` on 587, `AUTH PLAIN`, `LOGIN` or `XOAUTH2`, `SIZE`, `8BITMIME` and `SMTPUTF8`, with a result for every recipient. It hands the prebuilt bytes over unchanged with an explicit envelope; it never builds or rewrites a message.

## API

```rust
pub enum Security { Tls, Starttls, None }
pub enum Secret { Password(String), OAuthToken(String) }   // Debug hides the value
pub struct SmtpConfig { host, port, security, username, secret, helo, timeout, data_timeout }

SmtpConfig::new(host)                 // STARTTLS on 587, no sign-in, EHLO localhost, 60 s / 600 s
SmtpSubmitter::new(config)?           // trusts the web PKI roots
    .with_roots(&TlsRoots)?           // and extra roots from holtmail-core
```

`SmtpSubmitter::new` refuses (`Invalid`) `Security::None` towards anything but `localhost` and loopback addresses, a user name without a secret or the other way round, and an empty host or `EHLO` name.

| Field | Meaning |
| --- | --- |
| `host`, `port` | The server; `host` is also the name the certificate must carry |
| `security` | `Tls` from the first byte; `Starttls` upgrades before signing in and refuses a server that does not offer it; `None` only for localhost |
| `username`, `secret` | Sign-in; `Password` uses `AUTH PLAIN`, else `LOGIN`; `OAuthToken` uses `AUTH XOAUTH2` |
| `helo` | The `EHLO` name |
| `timeout` | Connecting, the TLS handshake, every command and its reply |
| `data_timeout` | Sending the message and waiting for the final reply |

## Guarantees

- Every `check` and `submit` opens its own connection and ends it with `QUIT`; nothing is shared between calls except the last seen `limits()`. No pipelining: each command waits for its reply, so every `RCPT` gets its own result.
- The bytes go out dot-stuffed and nothing else; the server undoes the stuffing and receives exactly the message. The message must pass `Envelope::validate` (CRLF line breaks, ending in CRLF).
- `MAIL FROM` carries `SIZE=<bytes>` when the server advertises `SIZE`, `BODY=8BITMIME` for 8-bit messages, and `SMTPUTF8` when an envelope address or the header section is not ASCII. A message over the advertised `SIZE` is `TooLarge` before `MAIL`; 8-bit mail to a server without `8BITMIME`, or internationalized mail without `SMTPUTF8`, is `Invalid` before `MAIL`.
- After `STARTTLS` the client refuses a server that sent anything before the handshake, and forgets the extensions it heard before.
- Logs: none. Errors carry server texts, which may name addresses; callers must not log them.

## Error mapping

| Where | Reply or failure | `SubmitError` |
| --- | --- | --- |
| Connect, TLS, greeting, `EHLO`, `STARTTLS` | refused connection, timeout, handshake failure, non-`2xx` greeting, no `STARTTLS` | `Unavailable` |
| `AUTH` | `5xx`, mechanism not offered | `Auth` |
| `AUTH` | `4xx` | `Temporary` |
| `MAIL` | `552` / `530` / other `4xx`, `5xx` | `TooLarge` / `Auth` / `Refused` |
| `RCPT` | `4xx`, `5xx` | per-recipient `Refusal`; `421` ends the session as `Refused`; all refused: `RSET` and `RecipientsRefused` |
| `DATA` | `552` / other `4xx`, `5xx` | `TooLarge` / `Refused` |
| Any command before the final dot | connection lost or timeout | `Temporary` |
| Writing the final dot, waiting for its reply | connection lost, timeout, malformed or `1xx`/`3xx` reply | `Uncertain` (no `QUIT` sent) |
| Final reply | `2xx` / `552` / `4xx`, `5xx` | `Ok` / `TooLarge` / `Refused` |

## Why not `mail-send`

The spike for Phase 56 looked at `mail-send` 0.7 (`Apache-2.0 OR MIT`). Its TLS goes through `rustls-platform-verifier`, so the extra roots from `holtmail-core` cannot be added, and its message send stops at the first refused `RCPT` and does not separate a drop after the final dot from one before it. The client here is a small state machine on `tokio-rustls` with the same TLS setup as `source-imap`, and adds no dependency.

## Not submitting to itself

`SmtpSubmitter::refusing_instance(token)` refuses a server whose greeting carries the word `holtmail-instance=<token>`, right after the greeting and before anything is sent, with `SubmitError::Invalid`, so the outbox fails the message once with a plain reason and the transport check shows it. Another token is another instance and is fine. `apps/server` sets it on every SMTP transport.

## Limits

- No `PIPELINING`, `CHUNKING`/`BDAT`, `DSN` parameters or `REQUIRETLS`; no connection reuse between messages.
- No `OAUTHBEARER` (RFC 7628) yet; Google and Microsoft accept `XOAUTH2`.
- The client does not add `CRLF` or convert bare `LF`: the caller builds proper bytes.

## Tests

`cargo test -p holtmail-submit-smtp`: the `holtmail-submit` conformance suite against `holtmail-smtp`'s `serve` with a test handler, plus a scripted raw TCP fake for the drop after the final dot; `STARTTLS` with a test CA, a missing or wrong CA and wrong passwords; implicit TLS; `AUTH LOGIN` when `PLAIN` is missing; `XOAUTH2` with a good and an expired token and when it is not offered; refused connections, a `421` greeting, a drop at `DATA` (`Temporary`), silence after the dot (`Uncertain`), `552` after the dot, `8BITMIME` and `SMTPUTF8` parameters and their absence, per-recipient codes and `limits`, and refused configurations.
