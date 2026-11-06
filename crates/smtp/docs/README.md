# holtmail-smtp

An SMTP receiver (RFC 5321) for mail servers that forward or journal into Holtmail. It takes mail in and never relays: every recipient is asked of a `Handler`, and accepted mail is handed to it with the envelope. A library, not a contract: there is one way to speak SMTP; what happens to the mail is the handler's.

## API

- `serve(listener, Arc<Options>, Arc<dyn Handler>, allowed)` serves until the runtime stops; clients for which `allowed(ip)` is `false` get `554` and are disconnected. At most `max_connections` sessions run at once.
- `Options { hostname, tls, require_tls, credentials, max_size, max_recipients, idle_timeout, max_connections }`; `Options::new(hostname)` gives 50 MiB, 100 recipients, five-minute waits, 32 connections, no TLS and no sign-in.
- `Handler::recipient(address) -> Recipient::{Accept, Reject(text)}` (`550 5.1.1`) and `Handler::deliver(&Envelope, Bytes) -> Result<(), Refusal::{Temporary, Permanent}>` (`250`, `451 4.3.0`, `554 5.6.0`). `Ok` means the mail is stored; the client then forgets it.
- `Envelope { peer, helo, mail_from, recipients, tls, authenticated }`.

## Guarantees

- Commands: `EHLO` (advertises `SIZE`, `8BITMIME`, `SMTPUTF8`, `ENHANCEDSTATUSCODES`, `STARTTLS` when offered and not on, `AUTH PLAIN LOGIN` when it may be used), `HELO`, `STARTTLS`, `AUTH PLAIN`/`LOGIN`, `MAIL` (with `SIZE=`), `RCPT`, `DATA`, `RSET`, `NOOP`, `VRFY` (`252`), `HELP`, `QUIT`. No `PIPELINING`, `CHUNKING` or `BDAT`.
- The data is handed on as received after dot-unstuffing, with no `Received:` line added. Only `<CRLF>.<CRLF>` ends it, and a dot is unstuffed only at the start of a line that follows a `CRLF`, so bare `LF`s cannot smuggle a second message in or change the bytes.
- `STARTTLS` refuses anything the client sent after it before the handshake (command injection) and forgets `EHLO` and sign-in afterwards. `AUTH` is offered only over TLS or to loopback clients; three failures close the connection; passwords are compared in constant time and never logged.
- `require_tls` refuses `MAIL` before `STARTTLS` (`530`); `credentials` refuse `MAIL` before `AUTH` (`530`). Messages larger than `max_size` are read to the end and refused (`552`), so the session goes on. Ten bad commands or a silent `idle_timeout` close the connection with `421`.
- Logs carry client addresses and counts, never envelopes or content.

## Tests

`cargo test -p holtmail-smtp`: a whole transaction byte for byte with a dot-stuffed line and bare-LF dots, out-of-order commands, `SIZE` and oversized data, temporary refusals, the error limit, refused clients and silence, and `STARTTLS` with a test CA, pipelining after it, `require_tls`, `AUTH PLAIN` and `LOGIN`.
