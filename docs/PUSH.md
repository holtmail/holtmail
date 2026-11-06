# Plan: push and fast change tracking

Built in WORKPLAN Phases 27–31 (2026-11-06); the module docs say how each piece behaves now, and this page keeps the reasoning. Where the build differs from the first plan, the plan below says so.

Before this work every account was polled: the scheduler looks every 30 seconds for accounts whose interval (15 minutes by default) has passed, syncs their mail one account after another, and syncs contacts and calendars only right after a mail sync. This plan makes new mail and changed contacts and events arrive within seconds where the server can say that something changed, and makes the polling that remains cheap. The steps are WORKPLAN Phases 27–31.

## What the servers offer

| Protocol | Push | Cheap "did anything change?" | Holtmail today |
| --- | --- | --- | --- |
| IMAP | `IDLE` (RFC 2177) per selected folder; `NOTIFY` (RFC 5465) for many folders on one connection | `CONDSTORE` `HIGHESTMODSEQ`, `UIDNEXT` | polling, `CHANGEDSINCE` for flags |
| JMAP mail | EventSource (RFC 8620 §7.3), WebSocket push (RFC 8887) | account `Email` and `Mailbox` state | polling, `Email/query` of every mailbox on every run |
| JMAP contacts, calendars | the same EventSource (`ContactCard`, `AddressBook`, `CalendarEvent`, `Calendar` types) | account state per type | collection tag = account state |
| CardDAV, CalDAV | WebDAV-Push (draft-bitfire-webdav-push, Web Push transport); Apple's push is closed | `getctag` / `sync-token` on the home set listing, `sync-collection` (RFC 6578) | `sync-collection` with ETag fallback, but only after a mail sync |
| Microsoft Graph | change notifications to a public webhook | delta links | delta links (mail), delta (contacts) |

`sync-collection` is already used for every DAV collection, and an unchanged collection costs nothing beyond the home set listing. What is missing for DAV is a schedule of its own and push where the server has it.

## Design

### A watch is a hint, the sync stays the truth

A watch never archives anything. It only says "this account (and maybe these folders or collections) may have changed", and the existing sync runs. So a missed or spurious notification can only make archiving later or do a wasted run. It can never lose or duplicate data. Polling stays on as the safety net for every account.

### Contracts

`holtmail-source` gains an optional watch:

```rust
#[async_trait]
pub trait Source: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError>;
    /// `Ok(None)` when the server cannot push; the default.
    async fn watch(&self, folders: &[String]) -> Result<Option<Box<dyn SourceWatch>>, SourceError> { Ok(None) }
}

#[async_trait]
pub trait SourceWatch: Send {
    /// Returns once something may have changed after `watch` returned. Spurious returns are allowed.
    /// A change is never swallowed while the watch is alive. An error ends the watch.
    async fn next(&mut self) -> Result<Changed, SourceError>;
    async fn close(self: Box<Self>);
}

pub struct Changed { pub folders: Option<Vec<String>> } // None: unknown which, look at all
```

`holtmail-collection` gains the same shape (`CollectionSource::watch(&self) -> Option<Box<dyn CollectionWatch>>`, `CollectionChanged { collections: Option<Vec<String>> }`). Both fakes get a `notify(..)` handle, and both conformance suites get cases: a change after `watch` wakes `next`, `close` ends it, and a source without push answers `None`.

How a source reaches the server again is not the watch's business. A watch that ends with an error is rebuilt by the server with fresh credentials, which also covers OAuth tokens that expire.

### IMAP (`source-imap`)

- `IDLE` only when `CAPABILITY` lists it; otherwise `None`. (Built: the renewal is every 10 minutes rather than 25, and the TCP keepalive was left out; a renewal on a dead connection fails and reconnects instead.)
- With `NOTIFY` (Dovecot has it): one connection, `NOTIFY SET (selected ...) (mailboxes <watched> (MessageNew MessageExpunge FlagChange))`, so `STATUS` answers name the folder. async-imap has no `NOTIFY`, so it needs its own command and response handling on the raw stream. That makes it a separate step after plain `IDLE`.
- Without `NOTIFY`: one connection per watched folder, `EXAMINE` + `IDLE`, woken by `EXISTS`, `EXPUNGE`, `FETCH` (flags) and `VANISHED`. `IDLE` is renewed every 25 minutes (RFC 2177 allows 29, and NATs often drop earlier), with TCP keepalive on the socket.
- Default watched folders: the Inbox role only. Per account, also a list of folders. At most 3 watch connections per account, since servers limit connections per user (Dovecot `mail_max_userip_connections` 10, Gmail 15), and the sync needs one of them.
- Gmail: a change in Inbox means syncing the folder with the `All` role, since labels shown as folders are not synced on their own.

### JMAP (`source-jmap`, `collection-jmap`)

- EventSource at the session's `eventSourceUrl` with `types=Email,Mailbox` for mail and `types=ContactCard,AddressBook,CalendarEvent,Calendar` for PIM, `closeafter=no`, `ping=60`. A missing ping for two intervals ends the watch.
- The two crates are separate implementations and must not depend on each other, so an account with both open has two event streams. That is accepted.
- Mail sync gets the shortcut that Graph has: `state()` is the account's `Email` state after a listing, and `unchanged_since` compares it with `Email/get` with no ids. An unchanged account then costs one request per folder instead of an `Email/query` of every mailbox.
- Credentials follow the existing rule: they go only to the session address and to the `apiUrl` and `eventSourceUrl` the authenticated session names, over https.

### CardDAV and CalDAV (`collection-dav`)

1. **A schedule of its own** (no push needed; built as `interval_minutes` in `settings.dav`, `contacts_interval_minutes` in the API): contacts and calendars get their own interval per account (`pim_interval_minutes`, default the mail interval, minimum 1). They are synced by the scheduler on their own, not only after mail. An unchanged account costs one `PROPFIND` per home set, because pim-sync already skips collections whose `getctag`/`sync-token` did not move.
2. **WebDAV-Push** where a collection lists `push-transports` with `web-push`, and only when `public_url` is set and https. Holtmail creates a P-256 key pair and an auth secret per subscription. It registers `<public_url>/api/push/dav/<random id>` as the push resource with `push-register`, and receives the server's Web Push messages there. It decrypts them (RFC 8291, `aes128gcm`), maps the topic to a collection, and starts that account's PIM sync. Subscriptions are renewed before they expire and unregistered when contacts are turned off or the account is deleted. Built against the draft as published on 29 October 2026 (namespace `https://bitfire.at/webdav-push`, `push-register`, `web-push-subscription`, `content-update` triggers, `Location` and `Expires` on the answer, message encryption required). The cryptography uses `aws-lc-rs`, already in the binary through rustls, instead of new `p256`, `hkdf` and `aes-gcm` crates. Subscription keys live only in memory: a restart registers anew and the old addresses answer `410`. Whether it works against Nextcloud's DAV Push app is not checked yet (VERIFY.md).
3. Without push and without a public address, the account polls at its own interval, which is cheap.

### JMAP accounts with contacts or calendars over DAV

Today the protocol is one choice for both kinds: an account that offers JMAP contacts uses JMAP for both, and gets no calendars when the session does not advertise `urn:ietf:params:jmap:calendars`. That is common, because JMAP Calendars is still a draft. The plan:

- `settings.dav` gets a protocol per kind: `contacts: jmap | dav`, `calendars: jmap | dav`, each `dav` with its address. Old settings with a single `protocol` are read as both kinds with that protocol.
- New library crate `holtmail-collection-merge`. It depends only on the contract, like pim-sync. It presents two `CollectionSource`s as one: contacts from one, calendars from the other. It routes `changes`, `fetch` and `list` by which source listed the collection id. An unknown id lists both again, then `Gone`. Watches are merged the same way. Stored collection ids do not change, so no migration is needed.
- Turning contacts and calendars on for a JMAP account checks `offers_contacts` and a new `offers_calendars`. For each kind JMAP lacks, it finds the DAV address the way IMAP accounts do (provider preset, then the mail domain's well-known address, then the JMAP host's well-known address) and signs in to it.
- Credentials: the JMAP password is tried for DAV only on the JMAP host or a known provider preset. Fastmail and others want a separate app password for DAV, and an API key used as a JMAP bearer token usually does not work for DAV. So the dialog can take a DAV password of its own, stored sealed like the account's secret. When no password works, contacts or calendars stay on JMAP only, and the account says which kind is missing and why.
- Existing JMAP accounts with calendars missing: a one-time check at start finds whether CalDAV answers with the stored credentials on the same host. If it does, the account shows "Calendars are available over CalDAV" with a button to turn them on. Nothing is changed by itself.

### Server (`apps/server`)

- New `watch.rs`: one task per account with watching on. It builds the source with fresh credentials, opens the mail watch and the PIM watch, and turns events into sync requests after a 2-second debounce.
- **Targeted runs.** `SyncOptions::only: Option<Vec<String>>` syncs just the named folders (the `All` folder for Gmail). A contacts request syncs the whole account; collections whose tag did not move cost nothing. An event that arrives while the account is already syncing marks it "again after this run", so the change is not lost.
- **The scheduler stops being sequential.** Due accounts and watch requests go through one queue with at most 4 accounts syncing at once (`max_parallel_syncs`), still one run per account. The 30-second check stays for intervals.
- **Reconnects.** Backoff from 5 seconds to 5 minutes. A sync runs after every (re)connect, for changes during the gap. `Auth` stops the watch and marks the account "sign in again" as a failed sync does. OAuth accounts reconnect before their access token expires. Pausing, disabling, deleting or editing the connection of an account stops or restarts its watch at the next settings check (30 seconds), or at once for changes made through the API. On `SIGTERM`: `DONE` and `LOGOUT`.
- **Config.** `[watch] enabled = true` (`HOLTMAIL_WATCH`) turns watching off server-wide, and `[watch] parallel_syncs = 4` sets the queue. The three connections per account are a constant of `source-imap`, not a setting. Per account: `watch: off | inbox | folders` with a folder list, and the contacts interval in `settings.dav.interval_minutes`. Contacts and calendars push follows the account's `watch` mode.
- **Logs** carry account and folder ids, counts and timings, never folder names, subjects or addresses.

### API, web and CLI

- Account DTO: `watch` (mode, folders) and `watch_state`: `live`, `polling` (the server cannot push), `reconnecting` with the reason, or `off`. Contacts and calendars carry `pim_interval_minutes`, and their protocol and push state per kind. `PATCH` changes them. OpenAPI and the client are regenerated.
- Web: a "Live" mark in the account row with a tooltip for its state, a "Watch for new mail" choice in the Check for new mail submenu (off, Inbox, chosen folders), the contacts and calendars interval, a per-kind protocol with a separate DAV password in the contacts and calendars dialog, and the "Calendars are available over CalDAV" offer.
- CLI: `holtmail account edit --watch off|inbox|<folder>,... --pim-interval <minutes>`.

### Not in this plan

- Microsoft Graph change notifications: they need a public https endpoint with Graph's validation handshake, and subscriptions that last at most about 3 days. Graph stays on delta polling, which is already cheap. It goes to Later.
- JMAP WebSocket push (RFC 8887): EventSource is enough.
- Gmail Pub/Sub push: it needs a Google Cloud project.

## Tests

- **Unit, against in-process fakes.** The IMAP fake answers `IDLE` with `+ idling`, sends `EXISTS` / `EXPUNGE` / `FETCH` on cue and checks `DONE`. Renewal after 25 minutes is tested with paused tokio time, and `NOTIFY` with `STATUS` answers. The JMAP fake serves an SSE stream with `StateChange` and pings, and the tests check that a missing ping ends the watch. The DAV fake lists `push-transports`, accepts `push-register` and POSTs an encrypted message (RFC 8291 test vectors as well). Merge source routing goes through the collection conformance suite run on a merged pair. The server watch manager is tested for debounce, "again after this run", backoff and a stop on `Auth`.
- **Integration (`tests/integration`, new `watch.rs`, `watch_pim.rs`).**
  - Mail appended to Dovecot, GreenMail and Stalwart by another IMAP connection is archived within 5 seconds with a one-day interval. A server-side kill of the IDLE connection is followed by a reconnect and the mail is still archived.
  - JMAP mail and contacts on Stalwart through EventSource.
  - A JMAP account whose session (served by the test, as in `jmap_pim.rs`) has no calendars gets calendars from Stalwart's CalDAV.
  - A one-minute PIM interval on Radicale picks up a changed card without a mail sync.
  - WebDAV-Push against Nextcloud with the DAV Push app in the `nextcloud` profile. The app has to be baked into the image, since tests reach no network beyond local containers.
- **VERIFY.md.** IDLE on real Gmail, Outlook and Yandex for an hour or more (NAT timeouts, token expiry), Fastmail JMAP push and its CardDAV/CalDAV with an app password, WebDAV-Push from a real Nextcloud to a public Holtmail.
