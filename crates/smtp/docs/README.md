# holtmail-smtp

An SMTP server (RFC 5321) for mail that comes to Holtmail: inbound from mail servers that forward or journal into it, and submission from mail clients. It takes mail in and never relays: the reverse path and every recipient are ruled on by a handler, and accepted mail is handed to it with the envelope. A library, not a contract: there is one way to speak SMTP; who signs in and what happens to the mail are the caller's.

## API

The full API is in `holtmail_smtp::server`. The crate root is a small adapter over it for receiving.

### `server`

- `serve(listener, Arc<Options>, Arc<dyn Handler>, allowed)` serves until the runtime stops, at most `max_connections` sessions at once; clients for which `allowed(ip)` is `false` get `554` and are disconnected (silently under implicit TLS).
- `handle(Connection, &Options, &dyn Handler)` serves one connection a listener already accepted. `Connection { stream, peer, tls_by_proxy, until_signed_in }` takes any stream (`Box<dyn ClientStream>`), so a listener with its own connection type can hand it in, and carries the real client address, whether TLS is already in place (done by that listener or by a trusted proxy), and a guard dropped at sign-in, so a listener can count connections that have not signed in. `Principal` is dropped when the connection ends, so a value inside it can count connections per user.
- `Options { hostname, tls, implicit_tls, require_tls, auth, instance_token, max_size, max_recipients, idle_timeout, sign_in_timeout, max_connections }`; `Options::new(hostname)` gives 50 MiB, 100 recipients, five-minute waits, 32 connections, no TLS and no sign-in.
- `SmtpAuth::verify(login, secret, peer) -> Result<SignedIn, AuthError>`. `AuthError::Invalid` is `535` (three close the connection), `TooMany` is `421` and closes, `Unavailable` is `454` and does not count.
- `SignedIn { principal, ended }`. `ended` is an optional future; when it finishes, the session gets `421 4.7.0` and closes. This is how a revoked credential ends a live session.
- `Principal` is opaque here: `Principal::new(value)`, `principal.get::<T>()`.
- `Handler`, each method given the `Session { peer, helo, tls, principal }`:
  - `limits(session) -> Option<Limits { max_size, smtputf8 }>`, asked at each `EHLO` and `MAIL`, by default `None` (`Options::max_size`, `SMTPUTF8` offered). `Options::max_size` always caps it. Note that clients commonly send `EHLO` only before `AUTH`; `MAIL` and `DATA` enforce the limits of the session as it is then.
  - `mail_from(session, address) -> Result<(), Refused>`; `address` is empty for `<>`. Asked after every session check.
  - `recipient(session, address) -> Result<(), Refused>`.
  - `deliver(session, &Envelope, Bytes) -> Result<(), Refused>`. `Ok` is `250` and means the mail is safely held. At most once per transaction; never for a message that was too large.
- `Refused::new(code, enhanced, text)` is sent as `<code> <enhanced> <text>`. A code outside 400 to 599 becomes `451 4.3.0`, so a refusal cannot read as a success.
- `Envelope { peer, helo, mail_from, recipients, tls, principal }`.

### Crate root (the receiver)

`serve(listener, Arc<Options>, Arc<dyn Handler>, allowed)`, with `Options { hostname, tls, require_tls, credentials, max_size, max_recipients, idle_timeout, max_connections }`, `Handler::recipient(address) -> Recipient::{Accept, Reject(text)}` (`550 5.1.1`), `Handler::deliver(&Envelope, Bytes) -> Result<(), Refusal::{Temporary, Permanent}>` (`250`, `451 4.3.0`, `554 5.6.0`) and `Envelope { peer, helo, mail_from, recipients, tls, authenticated }`. Every reverse path is taken, and `credentials` is one fixed sign-in, compared in constant time. Otherwise it behaves as `server` does.

## Guarantees

- Commands: `EHLO` (advertises `SIZE`, `PIPELINING`, `8BITMIME`, `SMTPUTF8` when offered, `ENHANCEDSTATUSCODES`, `STARTTLS` when offered and not on, `AUTH PLAIN LOGIN` when it may be used), `HELO`, `STARTTLS`, `AUTH PLAIN`/`LOGIN`, `MAIL` (with `SIZE=`, `BODY=7BIT|8BITMIME`, `SMTPUTF8`; other parameters are ignored), `RCPT`, `DATA`, `RSET`, `NOOP`, `VRFY` (`252`), `HELP`, `QUIT`. No `CHUNKING`, `BDAT` or `DSN`.
- Pipelining (RFC 2920): replies come in command order and are held only while another whole command is already waiting, so a client never waits for a reply the server is holding. A `DATA` with no accepted recipient gets `554`. A client that does not read its replies is closed after `idle_timeout`.
- The greeting is `220 <hostname> Holtmail ESMTP`, followed by ` holtmail-instance=<token>` when `instance_token` is set, so a transport can recognise that it would be submitting to its own instance.
- Implicit TLS (`implicit_tls`, port 465) starts with the handshake and sends nothing in the clear; without a certificate every connection is closed.
- The data is handed on as received after dot-unstuffing, with no `Received:` line added. Only `<CRLF>.<CRLF>` ends it, and a dot is unstuffed only at the start of a line that follows a `CRLF`, so bare `LF`s cannot smuggle a second message in or change the bytes.
- `STARTTLS` refuses anything the client sent after it before the handshake (command injection) and forgets `EHLO` and sign-in afterwards. `AUTH` is offered only over TLS or to loopback clients. `PLAIN` with an authorization identity other than empty or the login is refused without asking `SmtpAuth`. A client's `*` cancels a sign-in with `501`, without counting as a failure. Secrets are never logged.
- `require_tls` refuses `MAIL` before TLS (`530`); `auth` refuses `MAIL` before `AUTH` (`530`). Messages larger than the limit are read to the end and refused (`552`), so the session goes on. Without `SMTPUTF8`, addresses that are not ASCII get `553 5.6.7`. Ten bad commands or silence for `idle_timeout` (`sign_in_timeout` before sign-in, when set) close the connection with `421`.
- Enhanced status codes (RFC 2034) are on every reply except the greeting, `EHLO`, `HELO`, `354` and `334`.
- Logs carry client addresses and counts, never envelopes or content.

## Tests

`cargo test -p holtmail-smtp`. `src/tests.rs` drives the receiver: a whole transaction byte for byte with a dot-stuffed line and bare-LF dots, out-of-order commands, `SIZE` and oversized data, temporary refusals, the error limit, refused clients and silence, and `STARTTLS` with a test CA, pipelining after it, `require_tls`, `AUTH PLAIN` and `LOGIN`. `src/server/tests.rs` drives `server`: the principal at every step, `mail_from` refusals, implicit TLS with and without a certificate, `STARTTLS` forgetting a sign-in, pipelined batches, a partial command behind a held reply, the instance token, per-client limits, `PLAIN` authorization identities, cancelled sign-ins, each `AuthError`, an ended sign-in, `sign_in_timeout`, and a connection from a proxy.
