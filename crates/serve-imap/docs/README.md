# holtmail-serve-imap

IMAP for the user's own mail clients. A client signs in with a device's app password and sees the one archive the device serves (its `mail_archive`) as a mail account: the tree, served UIDs and `MODSEQ`, flags, bodies and search come from `MailView`, and every flag change, move, copy, delete and append goes through `MailActions`, so it meets the same five locks as the web app and reaches upstream servers only as held mail changes. A protocol front end, not a contract implementation: it receives `MailView`, `MailActions`, `DeviceAuth` and a `ChangeSignal`, holds no store, writer or transport, and only `apps/server` builds it, behind `[serve.imap]`. Nothing it does deletes a message from the archive. Design in [SERVE.md](../../../docs/SERVE.md#imap).

Built on `imap-next` 0.3.4 (the server state machine: framing, literals, `AUTHENTICATE`, `IDLE`) with `imap-codec` 2.0.0-alpha.10 and `imap-types` 2.0.0-alpha.8, all `MIT OR Apache-2.0`, pinned exactly in the workspace and kept inside this crate; no `imap-types` type is public. `imap-next`'s own stream is tied to `TcpStream`, so the crate drives the sans-I/O state machine over the `serve-net` connection itself. Earlier alphas encoded `FETCH (MODSEQ n)` without its parentheses and parsed `CONDSTORE` as `UNSELECT`; the pinned versions do not.

## API

```rust
ImapServer::new(view: Arc<dyn MailView>, actions: Arc<dyn MailActions>, devices: Arc<dyn DeviceAuth>,
                signal: Arc<dyn ChangeSignal>, settings: Settings) -> Arc<Self>
imap_server.serve(listener, tls: PortTls, proxy_protocol, trusted_proxies, limiter).await
impl ConnectionHandler for ImapServer        // one serve-net connection to its end
pub trait ChangeSignal { fn listen(&self, archive, user) -> Box<dyn ChangeListener>; }
pub trait ChangeListener { async fn changed(&mut self) -> Option<u64>; }
pub struct Settings { instance_token, sign_in_timeout, idle_timeout, idle_limit, keepalive, poll,
                      max_literal, response_bytes, fetch_batch }
pub enum PortTls { Implicit(TlsCertificates), Starttls(Option<TlsCertificates>) }
pub const INSTANCE_ID_FIELD: &str = "holtmail-instance";
pub const MAX_LITERAL: u32; // 50 MiB
pub const MAX_LINE: u32;    // 64 KiB
```

- `Settings::default()`: no instance token, 60 s before sign-in, 30 min between commands, `IDLE` up to 31 min with `* OK Still here` every 4 min, a look for changes every minute while idling, 50 MiB literals, 64 MiB of message data in memory for all sessions together, `FETCH` batches of 250.
- `ChangeSignal::listen` gives a session its wake-ups for one archive and user (the `ChangeHub` in `apps/server`). `ChangeListener::changed` waits for the next change and is safe to cancel; `None` means no more wake-ups, and the session falls back to `poll`.
- `Starttls(None)` is only for listeners on loopback addresses. Listeners of one server share one `ConnectionLimiter`.

## What it guarantees

1. **Sign-in:** `LOGIN`, `AUTHENTICATE PLAIN` (with `SASL-IR`) and `AUTHENTICATE LOGIN` with an app password, verified as `DeviceProtocol::Imap`, so the device needs `imap`, `read` and a readable mail archive. A `PLAIN` authorization identity other than empty or the login is refused. Only over TLS or from a client on this machine; otherwise the capability list says `LOGINDISABLED` and sign-in answers `NO [PRIVACYREQUIRED]`. A signed-in connection moves to its user's place in the connection limits (`NO [LIMIT]` over it). Wrong secrets are `NO [AUTHENTICATIONFAILED]`, a blocked client or an unavailable check `NO [UNAVAILABLE]`. Before sign-in a command may hold at most 8 KiB (`LOGIN` and `AUTHENTICATE` never need more); a client past it, for example with a large literal, gets `BYE` and the connection closes. `sign_in_timeout` is one deadline for the whole time before sign-in, not a wait per read.
2. **`STARTTLS`:** before TLS, input reaches the state machine one line at a time, and whatever the client sent after `STARTTLS` in the clear is dropped, so no command can be injected across the upgrade.
3. **Revocation:** revoking a device, or ending its sessions, closes every connection of it with `* BYE`, idle or not, within the feed's second.
4. **Capabilities** after sign-in: `IMAP4rev1 IDLE UIDPLUS MOVE UNSELECT ENABLE CONDSTORE QRESYNC LITERAL- SASL-IR ID NAMESPACE SPECIAL-USE CHILDREN APPENDLIMIT=<n>`, where `n` is the smaller of `MailActions::append_limit` and `max_literal`. `\Recent` is never set (`RECENT 0`; `NEW` and `RECENT` match nothing).
5. **`ID`** answers `name "Holtmail"` and `holtmail-instance` with the instance token, never a version. `source-imap` refuses an account whose `ID` carries its own instance's token.
6. **Names:** the view's paths joined by `/`, in modified UTF-7 both ways. `INBOX` matches in any letter case; other names exactly, then ignoring ASCII case. `LIST` adds `\HasChildren`/`\HasNoChildren`, `\Noselect` for tree levels without mail, and the `SPECIAL-USE` attribute of the role. `LSUB` lists the viewer's subscriptions, defaults included. `CREATE`, `DELETE` and `RENAME` are `NO [CANNOT]`: mailboxes come from the accounts.
7. **`SELECT`/`EXAMINE`/`STATUS`** read the served tables only, never a blob. `PERMANENTFLAGS` comes from the mailbox rights: `\Seen` with `set_seen`, `\Answered \Flagged \Draft \*` with `set_keywords`, `\Deleted` with `remove`, nothing when read-only. A mailbox where nothing may be changed, and every `EXAMINE`, is `[READ-ONLY]`. A view still being built is `NO [UNAVAILABLE]`; clients retry. A `SELECT` or `EXAMINE` while a mailbox is selected first sends `* OK [CLOSED]`, whether or not the client enabled `QRESYNC`: Dovecot's `imapc` waits for it from every server that offers `QRESYNC`.
8. **Catching up:** before or after each command in the selected state the session reads `mailbox_changes` from the `HIGHESTMODSEQ` it last told the client: new members become `EXISTS`, changed flags an unsolicited `FETCH (UID FLAGS [MODSEQ])` (not for changes the session itself just reported), members that left `EXPUNGE` or, with `QRESYNC`, `VANISHED`. Departures are held back during `FETCH`, `STORE` and `SEARCH` (RFC 3501 7.4.1) and told at the next command that allows them, so sequence numbers never drift.
9. **`FETCH`:** `FLAGS`, `ENVELOPE`, `INTERNALDATE` (`received_at`), `RFC822.SIZE` (the CRLF size), `UID`, `MODSEQ` from the store; `BODYSTRUCTURE`, `BODY` and every section (`HEADER`, `HEADER.FIELDS[.NOT]`, `TEXT`, `MIME`, part numbers, partials) cut from the raw bytes by the view's MIME structure. Bodies are served with every bare LF made CRLF; sizes in `BODYSTRUCTURE` count that form. A section that does not exist is an empty string. `BINARY` is not offered.
10. **Implicit `\Seen`:** `BODY[]`, `RFC822` and `RFC822.TEXT` without `.PEEK` set `\Seen` (through `MailActions::set_flags`, so local flags apply) only when the device's `implicit_seen` is on and the mailbox is read-write. Otherwise they behave as `.PEEK`.
11. **`CONDSTORE`/`QRESYNC`:** `ENABLE CONDSTORE QRESYNC`; `HIGHESTMODSEQ` on every select; `FETCH (CHANGEDSINCE)`, `UID FETCH (CHANGEDSINCE n VANISHED)`, `SELECT (QRESYNC (v m [uids]))` with `VANISHED (EARLIER)` and the changed flags, `SEARCH MODSEQ` with the highest `MODSEQ` found, `STORE (UNCHANGEDSINCE m)` refusing changed members with `OK [MODIFIED set]`. Read-only devices get all of it.
12. **`SEARCH`:** flags, dates, sizes, sequence and UID sets and `MODSEQ` are filtered by the view; text keys (`TEXT`, `BODY`, `SUBJECT`, `FROM`, `TO`, `CC`, `BCC`, `HEADER`) match words and word beginnings, never a piece from inside a word. Charsets other than UTF-8 and US-ASCII are `NO [BADCHARSET]`; a search too wide for the index is `NO [LIMIT]`.
13. **`STORE`** answers with the flags each message shows afterwards, held state included; `.SILENT` with `CONDSTORE` still sends `UID` and `MODSEQ`. A flag the locks refuse somewhere (`\Deleted` where nothing may be deleted, any flag where local flags are off) makes the command `NO [CANNOT]`; the others are applied. A held change the server later refuses comes back as an unsolicited `FETCH` with a new `MODSEQ`.
14. **`COPY`/`MOVE`:** `MailActions::copy_to`/`move_to`; `COPYUID` for every message that got a UID in the target. `MOVE` sends its `COPYUID` untagged, then the departures. Nothing accepted is `NO [CANNOT]`; some accepted is `OK` with a text that says some were not, and only those leave the mailbox. An unknown target is `NO [TRYCREATE]`.
15. **`EXPUNGE`/`UID EXPUNGE`/`CLOSE`:** members showing `\Deleted` go to `MailActions::remove` (a held delete to the trash, or expunge; a local place dropped; a draft deleted). A refusal is `NO [CANNOT]`. `CLOSE` does the same silently on a read-write mailbox.
16. **`APPEND`:** `MailActions::append`, answered `APPENDUID` at once. Drafts become draft versions (a new UID per save), Sent copies are linked or kept as local places by the view; the same bytes again are the same UID. Non-synchronising literals above 4096 bytes are `BAD` (`LITERAL-`), a message above the device's limit `NO [TOOBIG]`, a mailbox that takes nothing `NO [CANNOT]`.
17. **`IDLE`** waits on the change signal (and `poll`), sends what changed at once, keeps the connection alive every `keepalive`, and ends the connection after `idle_limit`.
18. **Memory:** messages whose bytes a `FETCH` needs take their size from one byte budget shared by all sessions (`response_bytes`) until their response is written; a larger message waits for the whole budget. Large `FETCH`es go out in batches of `fetch_batch`.

## Limits and quirks

- Commands are limited to `MAX_LINE` beyond their literals; a literal above `max_literal` is refused before it is sent (synchronising) or read and dropped (non-synchronising) with `BAD`. The device's own append limit, when smaller, is checked once the message arrived (`NO [TOOBIG]`): `imap-next` cannot change its limit per session.
- `ENVELOPE` dates are the `Date` header's time written in UTC (the store keeps the time, not the text); non-ASCII subjects and names go out as one RFC 2047 encoded word. `Sender` and `Reply-To` default to `From`.
- A literal cannot carry NUL, so NUL bytes in a message are sent as 0x80, as other servers do.
- `ENABLE` is accepted in the selected state too. `SORT`, `THREAD`, `COMPRESS`, `QUOTA`, `LIST-EXTENDED` and `UTF8=ACCEPT` are not offered.
- Logs carry device ids and counts, never mailbox names, search keys, addresses, content or secrets.

## Tests

`cargo test -p holtmail-serve-imap` runs scripted sessions over in-memory streams against `FakeMailView` (the mail view's fake) and an in-test `DeviceAuth`: sign-in and `PRIVACYREQUIRED`, the 8 KiB command cap and the single deadline before sign-in, the `STARTTLS` injection guard, `ID`, the tree in modified UTF-7, `STATUS`/`EXAMINE`, sections and partials, CRLF for bare-LF mail, implicit `\Seen`, search, `STORE` with `MODSEQ` and `[MODIFIED]`, expunge into the trash, read-only devices with local flags, `COPYUID`/`APPENDUID`, `LITERAL-` and `TOOBIG`, `IDLE` wake-ups and keepalives, revocation (also during `IDLE`), held-back expunges, `QRESYNC` resync, and convergence after a refused held change. Invented transcripts in the shape of Thunderbird, Apple Mail, Outlook and K-9 (with two connections idling on different folders) run end to end. Loopback tests run `source-imap` (reading, its `MailboxWriter`, and its loop guard) against a real listener on 127.0.0.1.
