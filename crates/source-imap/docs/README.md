# holtmail-source-imap

A [`Source`](../../source/docs/README.md) for IMAP servers, built on [`async-imap`](https://docs.rs/async-imap) with TLS through rustls and the Mozilla root store (`webpki-roots`).

## Settings

Stored as JSON in `Account.settings`; the password is sealed separately with `holtmail-secret`:

```json
{ "host": "imap.example.com", "port": 993, "security": "tls", "username": "ada@example.com" }
```

`security` is `tls` (default, port 993), `starttls` (port 143) or `plain` (local servers and tests only). `ImapSettings::from_json` fails with `SourceError::Config` when `host` or `username` is missing.

```rust
let source = ImapSource::new(ImapSettings::from_json(&account.settings)?, password);
```

## Behaviour

- **Read-only**: folders are opened with `EXAMINE`, messages fetched with `BODY.PEEK[]`, so nothing on the server changes and unread mail stays unread.
- **Folder paths** are decoded from modified UTF-7 (`&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-` → `Отправленные`) and use `/` between levels whatever the server's delimiter is (`INBOX.Sent` → `INBOX/Sent`). The session remembers the server's own name for each path.
- **Roles** come from special-use attributes (`\Sent`, `\Trash`, `\Junk`, `\Drafts`, `\Archive`, `\All`, `\Flagged`, Gmail's `\Important`), `INBOX` is always the inbox, and other folders fall back to `FolderRole::guess` on the name. `\Noselect` and `\NonExistent` folders are not selectable.
- **Fetching** asks for `UID FLAGS INTERNALDATE BODY.PEEK[]` with compressed UID sets (`1:3,7,9`). `\Recent` is dropped from flags; keywords are kept as they are.
- **Timeouts**: every command has two minutes; a server that does not answer is a `Connection` error.
- **Errors**: `NO`/`BAD` to `LOGIN` is `Auth`; I/O errors and lost connections are `Connection`; other refusals are `Protocol`.

## Tests

`src/tests.rs` runs a small IMAP server in the test process (plain TCP on a random port) that answers `LOGIN`, `LIST`, `EXAMINE`, `UID SEARCH` and `UID FETCH`, and fails the test if a fetch would mark mail as read.

## Not yet

`AUTHENTICATE XOAUTH2` for Gmail and Microsoft 365, `CONDSTORE` flag changes, Gmail `X-GM-LABELS`, `COMPRESS=DEFLATE`, `IDLE` for push.
