# holtmail-source-imap

A [`Source`](../../source/docs/README.md) for IMAP servers, built on [`async-imap`](https://docs.rs/async-imap) with TLS through rustls and the Mozilla root store (`webpki-roots`).

## Settings

Stored as JSON in `Account.settings`; the password or refresh token is sealed separately with `holtmail-secret`:

```json
{ "host": "imap.example.com", "port": 993, "security": "tls", "username": "ada@example.com" }
```

`security` is `tls` (default, port 993), `starttls` (port 143) or `plain` (local servers and tests only). `ImapSettings::from_json` fails with `SourceError::Config` when `host` or `username` is missing; `port()` is the port it will connect to.

```rust
let source = ImapSource::new(ImapSettings::from_json(&account.settings)?, Credentials::Password(password));
let gmail = ImapSource::new(settings, Credentials::AccessToken(access_token));
let private = ImapSource::new(settings, credentials).with_roots(&roots)?; // a server with a private CA
```

`with_roots(&TlsRoots)` trusts the given CA certificates for TLS and STARTTLS besides the Mozilla roots (see [`holtmail-core`](../../core/docs/README.md)); verification and the host name check stay on. A root that rustls cannot use is a `Config` error.

Unknown keys in the JSON (folder rules, the sync interval, the server's `oauth` block) are ignored.

## Behaviour

- **Read-only**: folders are opened with `EXAMINE`, messages fetched with `BODY.PEEK[]`, so nothing on the server changes and unread mail stays unread.
- **Folder paths** are decoded from modified UTF-7 (`&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-` → `Отправленные`) and use `/` between levels whatever the server's delimiter is (`INBOX.Sent` → `INBOX/Sent`). The session remembers the server's own name for each path.
- **Roles** come from special-use attributes (`\Sent`, `\Trash`, `\Junk`, `\Drafts`, `\Archive`, `\All`, `\Flagged`, Gmail's `\Important`), `INBOX` is always the inbox, and other folders fall back to `FolderRole::guess` on the name. `\Noselect` and `\NonExistent` folders are not selectable.
- **Capabilities** are read once after signing in. `X-GM-EXT-1` marks Gmail; `CONDSTORE` (or `QRESYNC`) marks mod-sequences, turned on with `ENABLE CONDSTORE` when the server has `ENABLE`, so `EXAMINE` reports `HIGHESTMODSEQ`.
- **Fetching** asks for `UID FLAGS INTERNALDATE BODY.PEEK[]` with compressed UID sets (`1:3,7,9`), plus `X-GM-MSGID X-GM-LABELS` on Gmail. `\Recent` is dropped from flags; keywords are kept as they are.
- **Flags** without content: `UID FETCH a:b (UID FLAGS)` (with `X-GM-LABELS` on Gmail), and `(CHANGEDSINCE <modseq>)` when asked for changes. Asking for changes in a folder that reported no `HIGHESTMODSEQ` is a `Protocol` error before anything is sent.
- **Gmail labels**: every selectable folder except the one with `\All` and the Trash and Spam folders (`\Trash`, `\Junk`) is reported with `label: true`, since Gmail shows labels as folders and All Mail holds every message except trash and spam. `X-GM-LABELS` values are unescaped (Gmail quotes system labels as `"\\Important"`) and decoded from modified UTF-7 (`Projects/&BBIEMAQ2BD0EPgQ1-` → `Projects/Важное`); system labels read as Gmail shows them: `\Inbox` → `Inbox`, `\Sent` → `Sent`, `\Important` → `Important`, `\Starred` → `Starred`, `\Draft` → `Drafts`, `\Trash` → `Trash`, `\Spam` → `Spam`, any other `\Name` → `Name`. Labels come sorted without duplicates. `async-imap` parses `X-GM-LABELS` given as atoms or quoted strings; a label sent as a literal would fail the fetch (Gmail is not known to do that).
- **Timeouts**: connecting, signing in and every command have two minutes each (`with_timeout(Duration)` sets another limit); a server that does not answer in time is a `Connection` error. A `plain` or `starttls` connection waits at most 15 seconds (or the shorter limit) for the server to speak first and then says the port may expect TLS from the start; a server that opens with TLS bytes gets the same answer at once, and a `tls` connection whose server answers in plain text says the port may be plain or STARTTLS.
- **Sign-in**: `Credentials::Password` uses `LOGIN`. `Credentials::AccessToken` uses `AUTHENTICATE XOAUTH2` with `user=<username>\x01auth=Bearer <token>\x01\x01`, base64-encoded, for Gmail and Microsoft 365. A server that refuses the token answers with a continuation carrying a base64 JSON error instead of `NO`; the source answers it with an empty line, as the protocol asks, and the server's `NO` that follows becomes `Auth`.
- **Errors**: `NO`/`BAD` to `LOGIN` or `AUTHENTICATE` is `Auth`; I/O errors and lost connections are `Connection`; other refusals are `Protocol`.

## Tests

`src/tests.rs` runs a small IMAP server in the test process (plain TCP on a random port) that answers `CAPABILITY`, `ENABLE CONDSTORE`, `LOGIN`, `AUTHENTICATE XOAUTH2` (accepting one token, and sending the error continuation for any other and checking it is answered with an empty line), `LIST`, `EXAMINE` (with `HIGHESTMODSEQ` once enabled), `UID SEARCH` and `UID FETCH` (with `X-GM-*` items and `CHANGEDSINCE`), and fails the test if a fetch would mark mail as read, if Gmail items go to another server or if `CHANGEDSINCE` comes before `ENABLE`. It runs as a plain server or as Gmail with labels in modified UTF-7 and quoted strings. A TLS variant with a certificate from `holtmail_core::tls::testing::TestCa` checks that the CA is trusted only through `with_roots`, that another CA and another host name are refused, and that an unusable root is a `Config` error. `src/labels.rs` has unit tests for label names.

## Not yet

`QRESYNC` (`VANISHED`), `COMPRESS=DEFLATE`, `IDLE` for push. Checked only against the in-process server; real Gmail is part of Phase 12.
