# holtmail-source-jmap

Reads mail from JMAP servers (RFC 8620 for the core, RFC 8621 for mail), such as Fastmail, Stalwart or Cyrus. It implements the [`holtmail-source`](../../source/docs/README.md) contract: mailboxes become folders, each email's original RFC 5322 bytes are downloaded through the session's `downloadUrl`, and keywords become flags. The source never changes anything on the server: it only uses `Mailbox/get`, `Email/query`, `Email/get` and blob downloads. Writing is a separate `JmapWriter`, which implements [`MailboxWriter`](../../source/docs/README.md#mailboxwriter) with `Email/set`, blob uploads and `Email/import`; see [Writing](#writing).

## Public API

| Item | Guarantees |
| --- | --- |
| `JmapSettings { url, username, auth }` | Stored in the account's settings JSON; the password or token is sealed separately. `from_json` fails with `Config` |
| `JmapAuth::{Password, Token}` | `password` (default): HTTP Basic with `username`. `token`: a bearer token, such as a Fastmail API token; the user name is not sent |
| `session_url(input)` | The session address for what someone typed: `https://` is added when the scheme is missing; an address without a path gets `/.well-known/jmap`. Only `https`, or `http` to a loopback address; no credentials in the address. `Config` otherwise |
| `JmapSource::with_roots(roots)` | Also trusts the CA certificates in `TlsRoots` besides the system's roots; verification stays on. `Config` for an unusable certificate |
| `JmapSource::new(settings, credentials)` | `Credentials::Password` signs in with Basic as `username` (`Config` without one), `Credentials::AccessToken` with a bearer token. `Config` for an unusable address |
| `JmapSource::refusing_instance(token)` | Refuses, with `Config` and before anything is listed or watched, a server whose session carries `token` under the `urn:holtmail:params:jmap:instance` capability (`{"token": ...}`): that server is this Holtmail instance, which cannot be its own upstream. Another Holtmail instance is read like any server |
| `JmapSource::with_push_ping(ping)` | Asks the event source to ping every `ping` (at least a second) instead of every minute |
| `JmapSource::writer()` | A `JmapWriter` for the same account, signing in the same way and trusting the same roots |
| `JmapWriter` | `MailboxWriter` for the account. Capabilities: the four system flags and keywords, moves and appends report UIDs; no `unchanged_since`, no labels, no marking deleted without expunging. Details in [Writing](#writing) |

## Config

```json
{ "url": "https://api.fastmail.com/jmap/session", "username": "", "auth": "token" }
```

`folders.exclude`, `sync_interval_minutes` and `min_age_days` in the same JSON work as for IMAP accounts.

## How the contract maps onto JMAP

- **Session.** A session address under `/.well-known/` is first followed through its redirects (at most five, never from https to http) **without credentials**; the session is then asked with credentials at the address it led to, which may be on another host (Fastmail sends `fastmail.com` to `api.fastmail.com`). The session request itself does not follow a redirect: credentials never go to a host the typed or well-known address did not lead to, and a session address that redirects is refused as a protocol error saying so. `connect` checks that the session offers `urn:ietf:params:jmap:mail` and uses that capability's primary account, `apiUrl` and `downloadUrl`, also when they are on another host than the session (Fastmail downloads from `www.fastmailusercontent.com`); relative addresses are resolved against the session's address. API calls and downloads follow up to five redirects, never away from https, and reqwest drops the credentials on a redirect to another origin.
- **Folders.** Every mailbox is a folder; paths join names from the top with `/`. JMAP roles (`inbox`, `sent`, `drafts`, `trash`, `junk`, `archive`, `all`, `flagged`, `important`) map to the same `FolderRole`; other mailboxes have none. Every mailbox is selectable and none is a label.
- **Several mailboxes per email.** An email in two mailboxes is a message in two folders, like an IMAP copy: one blob, two locations. Its bytes are downloaded once per folder it is first seen in; ingest stores them once.
- **UIDs.** JMAP has none, so each email gets `receivedAt` in whole seconds shifted left by 22 bits, with the low 22 bits from a 64-bit FNV-1a hash of its id. That UID is the same in every mailbox and session (`receivedAt` never changes), below `2^62`, and ascends with the received time, so minimum-age syncs stop in the right place. It does not ascend with the time a message entered a mailbox (a message moved in later can be older), so `select` reports `uids_ascend: false` and the sync looks for every UID it has not archived yet. Two emails received in the same second with the same 22 hash bits would share a UID; the one with the larger id is skipped and the clash counted in a warning (ids only, never content).
- **`UIDVALIDITY`.** 32 bits of the hash of the mailbox id: a mailbox deleted and created again with the same name renumbers, as on IMAP.
- **Listing.** `uids` pages `Email/query` (`inMailbox`, sorted by `receivedAt` ascending, `calculateTotal`) and reads `receivedAt` with `Email/get` through a back-reference in the same request when `maxCallsInRequest` allows two calls. `fetch` and `flags` use `Email/get` with at most `maxObjectsInGet` ids per call (capped at 500) and at most `maxCallsInRequest` calls per request (capped at 16). Blobs are downloaded four at a time.
- **Flags.** Keywords set to `true` become flags: `$seen`, `$flagged`, `$answered` and `$draft` take IMAP's names (`\Seen`, ...); others stay as JMAP writes them (`$forwarded`). Folders have no mod-sequences, so the sync re-reads flags once a day; `flags` with `changed_since` is a `Protocol` error.
- **Vanished mail.** An email deleted between listing and fetching (not found by `Email/get`, or a `404` download) is left out, as the contract asks.
- **Unchanged accounts.** A session asks once, before it lists anything, for the account's `Email` state (`Email/get` with no ids). `state()` gives it for a listed folder, and `unchanged_since(state)` is `Some` while it has not moved. The state moves with every email added, removed, moved or re-flagged in any mailbox, so a sync of an account where nothing happened costs one call and lists no mailbox; any change lists every mailbox again.
- **Push.** `watch` opens the session's `eventSourceUrl` (RFC 8620 §7.3) with `types=Email,Mailbox`, `closeafter=no` and `ping=60`, filling the URL template or adding a query to a plain address, and reports every folder (`Changed::all()`) for each `StateChange` that names the session's mail account with `Email` or `Mailbox`; other accounts, types and malformed events are skipped. No answer at all for two and a half pings ends the watch with `Connection`, and so does a closed stream. `None` when the session names no event source. The event source gets the same credentials, only over https.

## Writing

`JmapWriter` keeps the `MailboxWriter` contract with these JMAP specifics. Every call opens its own session (one session request plus `Mailbox/get`), so writers can be used from several tasks at once.

- **Folders.** `folder` and `target` are paths as `folders()` reports them; a path no mailbox has is `Missing`, checked before anything changes.
- **Finding messages.** UIDs carry their email's received second, so a call does not list the whole mailbox: it groups the UIDs' seconds into windows (seconds less than an hour apart share one), asks `Email/query` with `inMailbox` and `after`/`before` for each window (`after` one second before the window, since Stalwart treats `after` as strictly later), and reads `id`, `receivedAt` and `mailboxIds` with `Email/get`. An email counts only when it is still in the folder and its derived UID was asked for; UIDs nothing matches are ignored. Where two emails share a UID, the one with the smaller id is used, as listing keeps it.
- **Flags.** `\Seen`, `\Answered`, `\Flagged` and `\Draft` become `$seen`, `$answered`, `$flagged` and `$draft`; other keywords are sent lowercased. One `Email/set` patches `keywords/<name>` to `true` or `null` per email, so a flag in both lists is removed and setting again changes nothing. A keyword JMAP cannot spell (empty, outside printable ASCII, or with one of `( ) { ] % * " \`, such as `\Deleted`) is `Unsupported` before anything is asked. `unchanged_since` is `Unsupported`: JMAP has only an account-wide state, no mod-sequence per message. Flag patches are not guarded by `ifInState`: they touch only the named keywords, and an email that left the folder meanwhile keeps the change, as on IMAP after a move.
- **Moves.** One patch per email: `mailboxIds/<folder>` to `null` and `mailboxIds/<target>` to `true`, so an email already in the target is only removed from the folder. The UID stays the same, so `Moved` reports each UID twice. A move to the same folder changes nothing and reports the UIDs found.
- **Deletes.** Only with `expunge`; without it is `Unsupported` (JMAP has no `\Deleted`). An email also in other mailboxes is only taken out of this one (`mailboxIds/<folder>` to `null`), like an IMAP expunge in one folder; an email only in this folder is destroyed, which on JMAP is permanent (no server-side Trash). To move to Trash, use `move_to`.
- **`ifInState`.** Moves and deletes are sent with `ifInState` set to the `Email` state the emails were found at, so a delete never destroys an email that was moved into a second mailbox meanwhile. When the server answers `stateMismatch`, the call finds the emails and plans the changes again, up to five times, then fails with `Connection` (retried later). Each later `Email/set` chunk is guarded by the previous chunk's `newState`.
- **Appends.** The bytes are uploaded unchanged to the session's `uploadUrl` as `message/rfc822`, then `Email/import` puts the blob in the folder with the flags as keywords and `date` as `receivedAt` (whole seconds). The new UID is derived from the created id and the `receivedAt` the server then reports, the same UID sync will find. A session without `uploadUrl` is a `Protocol` error.
- **Labels.** Every JMAP mailbox is a folder here (none is a label), so `set_labels` is `Unsupported`.
- **Not atomic.** A JMAP server applies each email's patch on its own: when it refuses one email (other than `notFound`, which counts as already gone), the call fails with `Protocol` naming only the error type, and emails earlier in the call may already be changed. Every patch is idempotent, so retrying the whole call is safe. Calls hold at most `maxObjectsInSet` emails (capped at 500).

## Errors

`401` and `403` are `Auth`; `404` on the session address is `Config`; a missing mail capability or mail account is `Config`; `429`, `5xx`, timeouts and broken connections are `Connection`; other refusals, method-level `error` answers and malformed answers are `Protocol`. Error texts name the host, never the address with ids, the user name or the secret.

## Limits and not done

- JSON answers larger than 64 MiB and message downloads larger than 256 MiB are refused as a protocol error.
- `Email/changes` and `Mailbox/changes` are not used: a sync after any change lists each mailbox's ids, as IMAP's `UID SEARCH ALL` does, and reads flags of archived mail once a day.
- A mailbox name containing `/` reads as a deeper path.
- `Mailbox/get` asks for every mailbox at once; a server that answers `requestTooLarge` for that is not handled.
- The writer has not yet run against a real JMAP server; the `mailbox-write` integration suite is to come.
- Tested against the in-process fake JMAP server in `src/tests.rs` (writes: `Email/set` with keyword and mailbox patches, destroys, `ifInState` and `stateMismatch`, uploads and `Email/import`, `Email/query` with `after`/`before`, all run through the `holtmail_source::testing::writer_conformance` suite with generous and with one-object, one-call limits; session with a redirect from `/.well-known/jmap`, a well-known redirect to another host asked without credentials, a session address that redirects refused with nothing sent to its target, Basic and bearer sign-in, `Mailbox/get`, paged `Email/query`, `Email/get` with back-references and a state that follows the emails, downloads, an event source with pings and `StateChange`s, and a session without one), a TLS server in the process whose test CA is trusted only through `with_roots`, and Stalwart 0.16 through the real binary in [`tests/integration/tests/jmap.rs`](../../../tests/integration/tests/jmap.rs) (password and API-key sign-in, first and incremental syncs, an email in two mailboxes, moves, deletions, keywords, refused credentials, well-known discovery and redirects) and [`watch.rs`](../../../tests/integration/tests/watch.rs) (new mail through the event source). Not yet against Fastmail or Cyrus.
