# holtmail-source-imap

A [`Source`](../../source/docs/README.md) and a [`MailboxWriter`](../../source/docs/README.md#mailboxwriter) for IMAP servers, built on [`async-imap`](https://docs.rs/async-imap) with TLS through rustls and the Mozilla root store (`webpki-roots`).

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

- **Read-only**: the `Source` never changes the server (writing is the separate [`ImapWriter`](#writing)): folders are opened with `EXAMINE`, messages fetched with `BODY.PEEK[]`, so nothing on the server changes and unread mail stays unread.
- **Folder paths** are decoded from modified UTF-7 (`&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-` → `Отправленные`) and use `/` between levels whatever the server's delimiter is (`INBOX.Sent` → `INBOX/Sent`). The session remembers the server's own name for each path.
- **Roles** come from special-use attributes (`\Sent`, `\Trash`, `\Junk`, `\Drafts`, `\Archive`, `\All`, `\Flagged`, Gmail's `\Important`), `INBOX` is always the inbox, and other folders fall back to `FolderRole::guess` on the name. `\Noselect` and `\NonExistent` folders are not selectable.
- **Capabilities** are read once after signing in. `X-GM-EXT-1` marks Gmail; `CONDSTORE` (or `QRESYNC`) marks mod-sequences, turned on with `ENABLE CONDSTORE` when the server has `ENABLE`, so `EXAMINE` reports `HIGHESTMODSEQ`.
- **Fetching** asks for `UID FLAGS INTERNALDATE BODY.PEEK[]` with compressed UID sets (`1:3,7,9`), plus `X-GM-MSGID X-GM-LABELS` on Gmail. `\Recent` is dropped from flags; keywords are kept as they are.
- **Flags** without content: `UID FETCH a:b (UID FLAGS)` (with `X-GM-LABELS` on Gmail), and `(CHANGEDSINCE <modseq>)` when asked for changes. Asking for changes in a folder that reported no `HIGHESTMODSEQ` is a `Protocol` error before anything is sent.
- **Gmail labels**: every selectable folder except the one with `\All` and the Trash and Spam folders (`\Trash`, `\Junk`) is reported with `label: true`, since Gmail shows labels as folders and All Mail holds every message except trash and spam. `X-GM-LABELS` values are unescaped (Gmail quotes system labels as `"\\Important"`) and decoded from modified UTF-7 (`Projects/&BBIEMAQ2BD0EPgQ1-` → `Projects/Важное`); system labels read as Gmail shows them: `\Inbox` → `Inbox`, `\Sent` → `Sent`, `\Important` → `Important`, `\Starred` → `Starred`, `\Draft` → `Drafts`, `\Trash` → `Trash`, `\Spam` → `Spam`, any other `\Name` → `Name`. Labels come sorted without duplicates. `async-imap` parses `X-GM-LABELS` given as atoms or quoted strings; a label sent as a literal would fail the fetch (Gmail is not known to do that).
- **Timeouts**: connecting, signing in and every command have two minutes each (`with_timeout(Duration)` sets another limit); a server that does not answer in time is a `Connection` error. A `plain` or `starttls` connection waits at most 15 seconds (or the shorter limit) for the server to speak first and then says the port may expect TLS from the start; a server that opens with TLS bytes gets the same answer at once, and a `tls` connection whose server answers in plain text says the port may be plain or STARTTLS.
- **Sign-in**: `Credentials::Password` uses `LOGIN`. `Credentials::AccessToken` uses `AUTHENTICATE XOAUTH2` with `user=<username>\x01auth=Bearer <token>\x01\x01`, base64-encoded, for Gmail and Microsoft 365. A server that refuses the token answers with a continuation carrying a base64 JSON error instead of `NO`; the source answers it with an empty line, as the protocol asks, and the server's `NO` that follows becomes `Auth`.
- **Loop guard**: `refusing_instance(token)` makes every connect ask `ID NIL` after signing in, when the server offers `ID`, and refuse with a `Config` error (not transient) a server whose answer carries `holtmail-instance` (`INSTANCE_ID_FIELD`) equal to `token`: an account pointing at this Holtmail itself. A server without `ID`, one that refuses it, or another Holtmail instance connects as usual. The composition root is to pass the same token `serve-imap` sends in its `ID` answer.
- **Errors**: `NO`/`BAD` to `LOGIN` or `AUTHENTICATE` is `Auth`; I/O errors and lost connections are `Connection`; other refusals are `Protocol`.

## Watching

`watch(folders)` needs `IDLE` in the capabilities; without it the probe connection signs out and the answer is `None`. Folders that do not exist or cannot be selected are left out, and with none left (or none named) `INBOX` is watched.

- **`NOTIFY`** (RFC 5465): with more than one folder and `NOTIFY` offered, one connection sends `NOTIFY SET (mailboxes (<names>) (MessageNew MessageExpunge FlagChange))` and idles; the `STATUS` answers name the folder. A server that refuses the command (`NO` or `BAD`) gets one connection per folder instead.
- **One connection per folder** otherwise, each with `EXAMINE` and `IDLE`, for the first `MAX_WATCH_CONNECTIONS` (3) folders: servers limit connections per user (Dovecot 10, Gmail 15) and syncs need theirs. A connection that cannot sign in stops adding more, and the watch keeps the ones it has.
- **What counts**: `EXISTS`, `EXPUNGE`, `FETCH` (flag changes) and `VANISHED` for the selected folder, `STATUS` for a watched one; `RECENT` and `OK Still here` do not. Answers that arrive while `IDLE` ends are read too.
- **Renewal**: each `IDLE` ends with `DONE` and starts again after ten minutes without news (`with_idle_renew(Duration)` sets another time). RFC 2177 allows 29 minutes; NAT gateways often forget a silent connection sooner, and a renewal on a dead connection fails and ends the watch.
- **Ending**: a lost connection, a timeout or a refused command ends the watch with `Connection` or `Protocol`; `close` sends `DONE` and `LOGOUT` on every connection; dropping the watch closes them without.

## Writing

`ImapSource::writer().await` returns an `ImapWriter`, the account's `MailboxWriter`. It signs in once to read the capabilities, which stay fixed for the writer's lifetime:

| `MailboxWriteCaps` | From |
| --- | --- |
| `flags` | All four system flags |
| `keywords` | Always; a folder whose `PERMANENTFLAGS` has neither `\*` nor the keyword makes `set_flags` fail with `Unsupported` before anything changes |
| `conditional` | `CONDSTORE` or `QRESYNC` (turned on with `ENABLE CONDSTORE`) |
| `labels` | `X-GM-EXT-1` (Gmail) |
| `move_uids`, `append_uid` | `UIDPLUS` |
| `mark_deleted` | Always |

- **Connections**: every call signs in on a connection of its own and opens its folder with `SELECT` (the read path keeps `EXAMINE`). A connection a call leaves in good order (success, `Missing`, `Unsupported`) is kept and reused by the next call within 60 seconds; an older one is dropped, and a connection error never leaves one behind. Calls from several tasks at once each get their own connection. Folder paths map to server names through a cached `LIST`, read again once when a path is unknown; a path that is still unknown or `\Noselect`, a `SELECT` the server refuses with `NO`, and an `APPEND` answered with `[TRYCREATE]` are `Missing`.
- **Which UIDs exist**: `set_flags`, `move_to` and `delete` first ask `UID SEARCH UID <set>`, so unknown UIDs are left out of every result. UIDs above 2³²−1 cannot exist in IMAP and are ignored.
- **Flags**: `UID STORE <set> +FLAGS (...)`, then `-FLAGS (...)` for the removed ones (a flag in both lists is only removed). With `unchanged_since`, the first `STORE` carries `(UNCHANGEDSINCE <modseq>)` and the UIDs in its `[MODIFIED <set>]` answer go to `FlagResult.modified` and are left out of the second `STORE`, so the condition is checked once, by the first command. `imap-proto` does not parse `MODIFIED`, so the writer reads it from the response text. A keyword must be an IMAP atom and must not start with `\`; `\Deleted` and other backslash flags are refused with `Unsupported` (deleting is `delete`, which needs its own capability).
- **Labels**: `UID STORE <set> +X-GM-LABELS (...)` / `-X-GM-LABELS (...)` in the folder with the `\All` role. Readable names are the mirror of the read path: `Inbox`, `Sent`, `Important`, `Starred`, `Drafts`, `Trash` and `Spam` go as `\Inbox` ... `\Draft` ...; any other name is encoded in modified UTF-7 and quoted.
- **Moving**: `UID MOVE` when the server has `MOVE`, with the new UIDs from `COPYUID` (sent untagged before the expunges, or in the tagged answer) when it also has `UIDPLUS`. Without `MOVE`: `UID COPY` of every message asked for (also one already marked `\Deleted`, so it is never expunged without a copy), `UID STORE +FLAGS.SILENT (\Deleted)` on them, then the expunge rule below. Without `UIDPLUS` the writer checks the expunge rule before copying, so a move it could not finish copies nothing. A connection lost between the copy and the expunge (or another client marking a message in that window) leaves the message in both folders, and a retry copies it again: a duplicate, never a loss.
- **Expunging** (`delete` with `expunge`, and a move without `MOVE`): with `UIDPLUS`, `UID EXPUNGE <set>` removes exactly the messages asked for. Without it, plain `EXPUNGE` would remove every message marked `\Deleted` in the folder, including ones another client marked, so the writer first asks `UID SEARCH DELETED` and sends `EXPUNGE` only when that finds nothing but its own messages. Otherwise it expunges nothing, fails with a transient `Protocol` error that says so (a `delete` leaves its messages marked `\Deleted`; a move fails before copying); a retry succeeds once the other marked messages are gone. A message another client marks between the search and the `EXPUNGE` would still be removed; the window is one round trip.
- **Deleting** without `expunge`: `UID STORE +FLAGS.SILENT (\Deleted)`.
- **Appending**: `APPEND "<folder>" (<flags>) "<dd-Mon-yyyy hh:mm:ss +0000>" {<size>}` with a synchronizing literal; the bytes go out unchanged. A server that refuses before the continuation is answered without sending the bytes. The UID comes from `APPENDUID` with `UIDPLUS`, else `None`.
- **Errors**: a `NO` or `BAD` to a write command is `Protocol` with the command's verb and the server's text (no folder names, flags or labels from the command); I/O errors and timeouts are `Connection`. Logs carry only counts.
- **Labels that act as folders**: `set_labels` answers `Unsupported` before connecting for `Inbox`, `Sent`, `Drafts`, `Trash` and `Spam` (any case) and any name starting with `\`, since adding `\Trash` trashes a message and removing `\Inbox` archives it; those are moves and deletes, with their own locks.
- **Gmail quirks**: Gmail folders other than All Mail, Trash and Spam are labels, so moving out of a label folder removes the label, `delete` with `expunge` in a label folder only removes that label (the message stays in All Mail), and deleting for good means moving to `[Gmail]/Trash`. Whether Gmail expunges `\Deleted` messages at once depends on the account's "auto-expunge" setting.

## Tests

`src/tests.rs` runs a small IMAP server in the test process (plain TCP on a random port) that answers `CAPABILITY`, `ENABLE CONDSTORE`, `LOGIN`, `AUTHENTICATE XOAUTH2` (accepting one token, and sending the error continuation for any other and checking it is answered with an empty line), `LIST`, `EXAMINE` (with `HIGHESTMODSEQ` once enabled), `UID SEARCH` and `UID FETCH` (with `X-GM-*` items and `CHANGEDSINCE`), and fails the test if a fetch would mark mail as read, if Gmail items go to another server or if `CHANGEDSINCE` comes before `ENABLE`. It runs as a plain server or as Gmail with labels in modified UTF-7 and quoted strings. A TLS variant with a certificate from `holtmail_core::tls::testing::TestCa` checks that the CA is trusted only through `with_roots`, that another CA and another host name are refused, and that an unusable root is a `Config` error. `src/labels.rs` has unit tests for label names.

`src/tests_watch.rs` runs a second server in the process that speaks `IDLE` and, when asked, `NOTIFY`, announces deliveries to idling connections (`EXISTS` for the selected folder, `STATUS` for notified ones, queued while a connection is between two `IDLE`s) and checks that only `DONE` ends `IDLE`. It runs the watch conformance suite, and checks a server without `IDLE`, `NOTIFY` for several folders on one connection (an unwatched folder never wakes it), one connection per folder up to three without `NOTIFY`, renewal, a connection the server drops, `DONE` and `LOGOUT` on `close`, and a refused sign-in.

`src/tests_writer.rs` runs a third, writable server in the process with shared state: `LIST`, `SELECT` (with `PERMANENTFLAGS` and `HIGHESTMODSEQ`), `UID SEARCH` (`UID`, `DELETED`, `ALL`), `UID STORE` of flags and `X-GM-LABELS` with `UNCHANGEDSINCE` and `MODIFIED`, `UID COPY`, `UID MOVE` (with an untagged `COPYUID`), `UID EXPUNGE`, `EXPUNGE` and `APPEND` with a literal (with `APPENDUID`, `[TRYCREATE]` for an unknown folder), each extension only when its capability is advertised; it fails the test when a command arrives that the advertised capabilities do not allow. It implements `MailboxProbe`, and the writer conformance suite runs against it three times: with `CONDSTORE`, `MOVE` and `UIDPLUS`; with none of them; and as Gmail with labels. Further tests check Gmail label spelling and that labels acting as folders are refused, that without `UIDPLUS` another client's `\Deleted` message is never expunged, a move that cannot expunge copies nothing, a message already marked `\Deleted` is copied before it is expunged, the `APPEND` date and flags, refused keywords, connection reuse and `Missing`.

## Not yet

`QRESYNC` (`VANISHED` in syncs), `COMPRESS=DEFLATE`; for writes, `LITERAL+` and `BINARY` in appends and the `MAILBOX-ID` of RFC 8474 to follow renamed folders. The writer is checked only against the in-process server; real servers (Stalwart, Gmail) are for the `mailbox-write` integration suite and [VERIFY](../../../docs/VERIFY.md). The source is checked only against the in-process server; real Gmail is part of Phase 12.
