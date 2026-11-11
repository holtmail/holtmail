# Plan: serving

Planned 2026-11-08, reviewed the same day. The steps are WORKPLAN Phases 65–75; all are built. Deployment is in [DEPLOY.md](DEPLOY.md).

Holtmail learns to be an account. A user adds it to Apple Mail, Thunderbird, Outlook, K-9, aerc, Apple Calendar, DAVx5 or any other standard client, and uses it like a normal mail, calendar and contacts account:

- **IMAP** and **JMAP** over the archive, with flags, moves, deletes and appends that reach the upstream servers through the existing writeback;
- **SMTP submission**, so clients send through Holtmail's identities, routes and outbox;
- **CalDAV** and **CardDAV** over the served contacts and calendars, with held and write-through edits;
- **calendar-auto-schedule** (RFC 6638), so invitations and replies made in a calendar app are delivered by Holtmail.

Sieve export (old Phase 65) is dropped; rules stay Holtmail flows ([WRITE.md, Rules](WRITE.md#rules)).

This plan builds on [WRITE.md](WRITE.md) (the five locks, `mail_changes`, `MailboxWriter`, the outbox, routes and identities) and [CLIENTS.md](CLIENTS.md) (devices, the `changes` log, held edits, `write_mode`, `item_changes`, `pim-writeback`, invitations). It takes over the device table of CLIENTS Phase 44, the app passwords and `.mobileconfig` of CLIENTS Phase 50, the `dav-serve` bullet of CLIENTS Phase 51 and the auto-schedule half of WRITE Phase 62.

## Principles

- **The archive stays the truth.** A served mailbox, calendar or address book is a view over the archive: stored messages and blobs, locations, flags, open `mail_changes`, versions and open `item_changes`. Serving adds no second copy of any message. Everything a protocol shows can be rebuilt from the archive, except the served UIDs, subscriptions and drafts, which are protocol and user state (see [UIDs](#uids-modseq-and-states)).
- **Serving never bypasses the five locks.** Every write from a client is the same write the web app makes: a `mail_changes` row, an `item_changes` row or an outbox row, checked by the same code (`holtmail-access`) against the instance switch, the account capability, the archive grant, the device scope and, for sending, the identity grant. A protocol front end cannot write any other way: it holds no writer, transport or store handle that writes.
- **Read-only by default.** A new device credential has the `read` scope only. Every protocol listener is off until the owner configures it. A client that may not write sees that up front: `PERMANENTFLAGS`, `myRights`, DAV privileges and `calendar-auto-schedule` advertise only what the locks allow, so clients grey things out instead of failing.
- **The archive never loses anything because of a client.** `EXPUNGE`, `Email/set destroy` and DAV `DELETE` remove a message or item from a place, never from the archive. Deleting from the archive stays an admin and API action with its own review.
- **Raw bytes go out unchanged.** `BODY[]`, JMAP blob downloads and DAV `GET` of an unedited item return the archived bytes byte for byte; their hash is the blob address. One exception: IMAP requires CRLF line ends, so a message archived with bare LF (mbox imports) is served over IMAP with CRLF, and `RFC822.SIZE` counts that served form. JMAP and DAV still return the archived bytes.
- **Holtmail is still not an MTA.** Mail submitted over SMTP or JMAP goes into the outbox and leaves through a configured transport, never directly to recipient MX hosts.
- **Speak the standard, document the deviations.** Where the archive cannot honour a protocol rule exactly (word search instead of substring, thread ids that merge, a draft that changes id when sent), the server picks the behaviour clients handle best and the module docs say so.
- **No loops.** Holtmail never serves itself as its own upstream, never submits to itself as a transport, and never answers a scheduling message with another automatic one (see [Loops](#loops)).

### Not in this plan

POP3, Exchange ActiveSync, MAPI/EWS, ManageSieve, `ACL` and shared mailboxes between users over IMAP (archives with grants already cover sharing), and delivery to recipient MX hosts. CalDAV delegation (`calendar-proxy`), `MKCALENDAR` and time zones by reference (RFC 7809) are not offered.

## Devices and sign-in

There are no device credentials yet (WRITE.md: "tokens and devices do not exist yet"). Phase 65 builds them once for every protocol and for the native clients of CLIENTS.md; CLIENTS Phase 44 (enrolment, pairing, sliding sessions) adds to the same table instead of making its own.

### Device credentials

A **device** is one client installation that a user set up: "Anna's iPhone", "Thunderbird at work". Each has:

| Field | Meaning |
| --- | --- |
| `user` | The owner. A device never acts as anyone else |
| `name` | Chosen by the user, shown in lists and in audit entries |
| `mail_archive` | The one archive IMAP and SMTP serve to this device (see [IMAP](#imap)); required when `imap` or `smtp` is on |
| `archives` | The archives JMAP and DAV may see. Default: every archive the user can read. A `.mobileconfig` device therefore has one mail archive and every calendar and address book |
| `protocols` | Any of `imap`, `smtp`, `jmap`, `dav`, `api` |
| `scopes` | `read`, `write:mail`, `send:mail`, `write:pim`, `notify` (WRITE.md scopes; `rules` is never given to a device) |
| `implicit_seen` | Whether fetching a whole body marks a message read (see [IMAP](#imap)); off by default |
| `created`, `last_used_day`, `last_protocol` | For the device list; days, not timestamps, so usage is not a tracking log |
| `revoked_at` | Revocation is immediate and permanent |

- **Secret:** an app password `hm_` plus 32 base32 characters (160 random bits), shown once. It is stored as a BLAKE3 hash, the same scheme as session tokens in `holtmail-auth` (`token_hash`): a random secret of that size needs no slow hash, and clients reconnect constantly. Comparison is by hash lookup, so it takes the same time for any wrong secret.
- **Only app passwords.** The user's web password, a passkey or an OIDC sign-in never authenticates a protocol. A leaked app password is one device, not the account.
- **Login name:** the user's email address. A secret only works with its own user. SASL `PLAIN` with an authorization identity other than empty or the login name is refused.
- **Creating a device, or raising its scopes or protocols, needs a recent sign-in** (password or passkey within 5 minutes; the API answers `428` otherwise), and the user gets a notice in the web app: a line in the mail and settings layouts for seven days, dismissed per browser, from the device's `created_at` and `raised_at`. A wider archive list and another mail archive count as raises too. A stolen browser session therefore cannot quietly mint a lasting credential with `send:mail`.
- **Scopes replace CLIENTS's `dav:read` and `dav:write`:** `read` with the `dav` protocol is `dav:read`, `write:pim` with `dav` is `dav:write`.
- **Rights are the lowest of all locks.** A scope never adds a right: a device with `write:mail` on an archive where the user is a `reader` still cannot change flags.
- **Revocation closes live sessions.** IMAP connections, IDLE, JMAP EventSource streams and SMTP sessions of a revoked device end within a second, through a revocation feed from `DeviceAuth`. Taking a right away from a device ends its live sessions the same way; the client signs in again under the new rights. HTTP protocols re-verify on every request; a verified secret may be cached for at most 10 seconds and the cache entry is dropped by the same feed.
- **Rate limits:** the existing login limiter (keyed by client address, an IPv6 address counting by its /64, and login name) counts failures from every protocol together; a second limit counts failures per client address across all login names, so one address cannot spray many accounts. The limiter leaves `holtmail-auth`'s private module for a public type `device-local` uses. Behind the HTTP proxy the client address comes from `X-Forwarded-For` only when the peer is in `trusted_proxies`; today's login route sees only the proxy's address, and Phase 65 fixes that for the web sign-in too.
- **Who may make devices:** `[serve] members = true` (default) lets every member make devices for their own archives; `false` limits new devices and raises to the owner and admins; devices members already have keep working until revoked.
- **Audit:** creation, scope change and revocation are audit entries. Every write a device makes carries its device id: `item_changes` already has a `device` column; `mail_changes` and `outbox` gain one in Phase 65.
- **Never logged:** secrets, mailbox names, search keys, message or item content, addresses. Logs carry device ids, protocol, command counts and timings.
- **Later:** `OAUTHBEARER` and `XOAUTH2` with device tokens from the CLIENTS Phase 44 enrolment.

### Authentication per protocol

| Protocol | Mechanisms | Only over |
| --- | --- | --- |
| IMAP | `AUTH=PLAIN` (with `SASL-IR`), `LOGIN` | TLS, or loopback. Before TLS: `LOGINDISABLED` |
| SMTP submission | `AUTH PLAIN`, `AUTH LOGIN` | TLS, or loopback |
| JMAP | HTTP Basic, `Authorization: Bearer <login>:<app password>` (as built) | https per `public_url`, or loopback |
| CalDAV, CardDAV | HTTP Basic | https per `public_url`, or loopback |

- "Loopback" is the real client address: after PROXY v2 it is the address the proxy reports, never the proxy's own. A connection counts as TLS when Holtmail terminated TLS itself, or when it came from a trusted proxy whose PROXY v2 header carries the SSL TLV with a verified client connection.
- The HTTP protocols run on the existing listener behind the operator's proxy, as the web app does. `WWW-Authenticate` is sent only with a 401. A cookie session never authenticates JMAP or DAV: they are for clients, not for the browser, and accepting cookies there would reopen CSRF.

## Listeners and TLS

```toml
[serve]
tls_cert = "/etc/holtmail/fullchain.pem"   # shared by IMAP and submission
tls_key = "/etc/holtmail/privkey.pem"
proxy_protocol = false                     # PROXY v2 from trusted_proxies only
trusted_proxies = ["10.0.0.0/8"]           # also trusted for X-Forwarded-For on HTTP
members = true
local_flags = true

[serve.imap]
listen_tls = ["0.0.0.0:993"]
listen_starttls = []                       # e.g. ["0.0.0.0:143"]

[serve.submit]
listen_tls = ["0.0.0.0:465"]
listen_starttls = ["0.0.0.0:587"]
undo_seconds = 0
messages_per_hour = 200                    # per user, across devices
recipients_per_day = 2000

[serve.jmap]
enabled = true                             # mounts /jmap and /.well-known/jmap

[serve.dav]
enabled = true                             # mounts /dav and /.well-known/caldav, carddav
edits = true                               # false: every collection read_only (CLIENTS's [dav] edits)
scheduling = true
```

- Every listener is off when its section is missing. JMAP and DAV need `public_url`, because their discovery documents carry absolute URLs.
- TLS 1.2 and 1.3 only, with the same rustls setup as the inbound `holtmail-smtp`. The certificate is reloaded when the files change; open connections keep the old one. Holtmail does not run ACME itself: the operator's certbot or Caddy writes the files.
- Implicit TLS (993, 465) is the recommended setup (RFC 8314); STARTTLS ports exist for clients that need them, and refuse sign-in before `STARTTLS`.
- PROXY protocol v2 is accepted only from `trusted_proxies`, so Caddy L4 or HAProxy can pass TCP through and the limiter still sees client addresses. A connection from anyone else that starts with a PROXY header is closed.
- The inbound `[smtp]` listener (journal and forwarding into the archive) stays separate from submission.

**Limits** (configurable, these are the defaults):

| Limit | Value |
| --- | --- |
| Connections in total / per user / unauthenticated per client address (IMAP and submission each) | 200 / 20 / 10 |
| Command line | 64 KiB |
| Literal, APPEND, submitted message, JMAP upload | the smallest transport `max_size` for submission, otherwise 50 MiB (`APPENDLIMIT`, `SIZE`, `maxSizeUpload`) |
| Wait before sign-in / after | 60 s / 30 min. IDLE may last 31 min (clients re-issue at 29); during IDLE the server sends `* OK Still here` every 4 min so mobile NATs keep the connection |
| JMAP | `maxCallsInRequest` 32, `maxObjectsInGet` and `InSet` 500, `maxSizeRequest` 10 MB, `maxConcurrentRequests` 4 |
| DAV resource | 2 MiB iCalendar, 5 MiB vCard (`max-resource-size`) |
| Search results | 50 000 ids, then the result is cut and the response says so where the protocol can |
| Expensive work per user (index searches, `BODYSTRUCTURE` parses, view builds) | 2 at once; more wait (as built: searches and structure parses through `mailview-archive`; large view builds run in the background job instead) |
| Submission per user | `messages_per_hour`, `recipients_per_day`, on top of every transport's own limits |

## What is served

### The mail view

IMAP and JMAP show the same tree, computed by one `MailView` (see [Modules](#modules)). The view belongs to one archive.

**An archive with one account** shows that account's folders at the top, with their roles. **An archive with several accounts** shows:

| Mailbox | Contents | Writes |
| --- | --- | --- |
| `INBOX`, `Sent`, `Drafts`, `Junk`, `Trash`, `Archive` | Unified by role: every place with that role in every account of the archive. They carry the role (`SPECIAL-USE`, JMAP `role`) | Per place, routed to the place's account |
| `Accounts/<account>/<path>` | One source folder: its live places, with held `mail_changes` overlaid | By that account's capabilities. No role, so a client sees one Inbox |
| `All Mail` | Every message in the archive, once | Flags only; never expunges. `SPECIAL-USE \All` |
| `Only in Holtmail` | Messages with no live place left upstream: archive-only mail | Flags as local flags only |

- **Places, not copies.** A message that sits in two accounts' inboxes appears once in the unified `INBOX`. Identical bytes twice in one folder are one message, so one UID; a delete of it applies to every place in that view.
- **Flags across places.** A message shows a flag when any of its places in the view, or the user's local flag, has it. A `STORE` writes every writable place (and the local flag where a place cannot be written); the answer carries the flags that result, so a client never believes a flag is gone that one place still has.
- **Gmail** label folders appear as folders; a move between them is `set_labels`, as the Phase 8 sync already models. An upstream folder with `\All` (Gmail's All Mail) is not served on its own: Holtmail's `All Mail` covers it.
- **Moves into a unified role mailbox** go to that role's folder in the same account as each place. A message whose account has no folder for the role cannot go there (`NO [CANNOT]`, JMAP `forbidden`).
- **Drafts** is per user and special for both protocols (see [Drafts and Sent copies](#drafts-and-sent-copies)).
- **Local places.** A few things a user must see have no upstream folder: DSNs for outbox failures and a client's Sent copy that cannot be appended upstream. They are per-user rows `local_places(user, archive, role, message)` shown in the role mailbox for that user only, never written upstream.
- **People, From mail and attachments** are not mail and are not in the tree. Saved searches as mailboxes wait for saved searches ([WORKPLAN, Serving](WORKPLAN.md#serving-planned-2026-11-08), Later).

**Names.** The hierarchy delimiter is `/`. Mailboxes are keyed by folder id, not by name, so an account rename is a rename to the client and keeps every UID. As built, folders are known by account and path, so a folder renamed upstream arrives from sync as a new folder: a new mailbox with new UIDs (keeping UIDs across upstream renames needs folder identity from IMAP `OBJECTID` and JMAP or Graph folder ids; WORKPLAN, Serving, Later). A `/` inside an upstream name segment is shown as `∕` (U+2215); a name that collides with a Holtmail mailbox gets ` (<account>)` appended. Names go out in modified UTF-7, or as UTF-8 after `ENABLE UTF8=ACCEPT` once `imap-types` supports it.

**Subscriptions** are stored per user and archive (`served_subscriptions`). By default role mailboxes and non-role folders are subscribed; `All Mail`, `Only in Holtmail` and the role folders under `Accounts/` are not, so clients that sync or download only subscribed folders (Thunderbird offline, K-9) do not fetch the same message three times.

**Effective flags** are the location's synced flags, then open `mail_changes` on top (the held state shows at once), then [local flags](#local-flags). The web app and every protocol read the same overlay from the store, so a flag set in Apple Mail shows in the web app and the other way round.

**As built (Phase 67).** `mailview-archive` keeps the layout above with these decisions, written in its docs and the contract's:

- With one account, mailboxes are keyed by folder id and no `role:*` mailbox exists; a second account changes the keys, and clients resync once. The one-account role folder reads the `Role` view, so the user's local places show in it.
- Tree levels without a folder are `node:<path>` placeholders that cannot be selected and allow nothing.
- `COPY` is always a held `append` of the archived bytes; a delete goes to the account's trash (`mail.delete`) and in the trash, without one, or for JMAP destroy it is expunged (`mail.expunge`).
- A held move that a client moves again or deletes is replaced (the held change is failed as `replaced` and dismissed) and the new change is recorded from the location, so the view always shows the newest decision.
- Drafts serve raw drafts only; rendering compose drafts with `holtmail-compose` did not land in Phase 69 and is moved to Serving, Later (WORKPLAN, Phase 69 log). The web app opens a raw draft like "Edit as new".
- Search text matches words and word beginnings, as the index does.
- Mailbox changes report tree, rights and subscriptions, mapped through the layout: a changed folder also marks the unified role mailbox and tree levels it is part of, a role mailbox left without folders is destroyed, and a new or deleted account (every path moves) answers `CannotCalculateChanges`; `counts_changed` tells JMAP to refetch counts. A client draft the web app saves over or deletes is reported destroyed.
- A held, sending or applied move shows in its target with the source's flags until sync records the target; a cross-account move or a copy appends with the place's synced flags (never local flags).
- A held move a client moves again is replaced in one store step (`replace_held_mail_changes`) only while it is still held; once the writeback worker claimed it the place is busy and the action is refused.
- A Sent copy no account takes becomes a local place only when the viewer's own locks for appending are open (switch, grant, `write:mail`) and every Sent folder is closed only by its account (capability or consent); it is audited with the device id. A read-only device is refused and nothing is archived.
- Search text goes to the index narrowed to the mailbox's dates; when even that matches more than `MAX_MATCHES`, the search fails with `MailViewError::TooMany` (IMAP `NO [LIMIT]`) instead of answering incompletely.
- `apps/server` prunes the change log every six hours to 90 days (`ChangeStore::prune_changes`, the mark in `pruned_through`) and then the served tables (`prune_served`); a JMAP state below the mark answers `CannotCalculateChanges`.
- Outbox-failure DSNs for mail from a device are local places in the device user's `INBOX`.
- `ChangeHub` in `apps/server` wakes listeners at most every 250 ms; `build_ahead` builds the views of every archive a live IMAP or JMAP device reads.

### Local flags

An archive is read-only towards most servers by default. A user who reads Holtmail in Apple Mail still expects read and starred state to stick. So when a flag change cannot go upstream (the account lacks `mail.flags`, writes are off, or the message is only in Holtmail), and the device has `read`:

- the flag is kept as a **local flag** per user and message, served by every protocol, never sent upstream. A local flag records the user's setting either way (set or cleared), so marking a read message unread works where the upstream `\Seen` cannot be removed;
- `PERMANENTFLAGS` and JMAP `myRights.maySetSeen` and `maySetKeywords` say yes; `\Deleted` is never a local flag, so it is in `PERMANENTFLAGS` only where the place may be deleted;
- moves and deletes have no local form and are refused up front.

When a lock later opens for the account, new changes go upstream; existing local flags stay local until the user drops them in Settings → Devices (_In mail apps_ lists them per archive, newest 200, with Clear per message and Clear all; as built, there is no "send them now": a user who wants them upstream sets the flag again from a client once the lock is open). A local flag never goes upstream on its own. `[serve] local_flags = false` turns this off, and protocols then refuse flag changes they cannot write.

A local flag change raises the served `MODSEQ` of the message for everyone reading the shared view; other users refetch flags and see their own, unchanged. The change row carries no user content, so nothing about the change leaks beyond that it happened.

### UIDs, MODSEQ and states

Upstream UIDs cannot be reused: they are per account, change with upstream `UIDVALIDITY`, do not exist for Graph, and unified views mix accounts. Holtmail assigns its own.

**One change feed.** The `changes` table (migration 0032: `seq`, `archive_id`, `collection_id`, `item_id`, `href`, `item_change_id`, `at`; today written by item changes) gains `kind`, `message_id`, `folder_id` and `user_id` (set for per-user kinds: local flags, drafts, local places, subscriptions), and as built also `thread_id` (the thread a merged message left), `outbox_id` and `draft_id`, so JMAP `/changes` can name the object without a lookup. Subscriptions write `mailbox` rows with the user; `submission` rows carry no user. Kinds: `item` (today's rows, and as built also a new upstream version, removal or return of an item, so `sync-collection` sees what pim-sync brought in), `message` (ingest, deletion from the archive), `place` (location added or removed, `mail_changes` state), `flags` (synced flags, label changes, local flags), `thread` (merges), `mailbox` (as built: a new folder, a changed role, and every folder of a deleted account; the store has no single-folder rename or removal), `submission` (outbox state), `draft`. Every writer writes its row in the same transaction as the change: ingest, `record_location`, `mark_removed`, `set_flags`, `set_labels`, thread merges, folder sync, `mail_changes` and outbox transitions. A writer writes a row only when something changed: a flag sync that reads the same flags writes nothing, so a full resync of an unchanged folder adds no rows.

One global `seq` orders everything; RFC 7162 asks only for monotonic values per mailbox, and JMAP and DAV states are opaque. **The store guarantees that `seq` order is visibility order**: once `changes_since(s)` has returned a row, no row with a lower `seq` becomes visible later. SQLite gets this from its single writer; a future `store-postgres` must hand out `seq` under a commit-serialising lock (or a counter row updated in the same transaction), because a plain sequence lets a slower transaction commit a lower number after a reader moved past it, and that change would never reach a client. A store conformance case runs concurrent writers and checks it.

| Protocol state | Is |
| --- | --- |
| IMAP `MODSEQ` of a message in a mailbox | The `seq` at which the mailbox view last saw it change |
| IMAP `HIGHESTMODSEQ` | The highest such `seq` in the mailbox |
| JMAP `state` per type (`Email`, `Mailbox`, `Thread`, `EmailSubmission`) | The archive's highest `seq` of the kinds behind that type, counting per-user rows of this user only |
| DAV sync-token, `getctag` | `urn:holtmail:sync:<collection>:<seq>` and that `seq` |

**Served mailboxes and UIDs** (new store part, `ServeStore`):

- `served_mailboxes(id, archive, user, key, uidvalidity, uidnext, highest_modseq, synced_seq, lowest_modseq, built)`: `key` names the view (`role:inbox`, `folder:<id>`, `all`, `kept`, `drafts`). `drafts` is per user (`user` set); the others are shared by everyone who reads the archive, with local flags and local places applied per user on output.
- `served_uids(mailbox, uid, message_id, modseq, expunged_modseq)`, unique on `(mailbox, message_id)` among live rows (as built: a member that leaves keeps its tombstone and comes back under a new UID, which a plain unique key could not hold). In the drafts view the member is a draft version, not a message. Local places get their UIDs in their role view and are shown to their user only.

Guarantees:

- **UIDs are keyed by message, not by upstream UID.** A held move gets its target UID at once (`COPYUID`); when sync later sees the place under its new upstream UID, it is the same message and keeps its served UID. An upstream `UIDVALIDITY` reset changes nothing for clients.
- **UIDs only grow and are never reused.** A view catches up when a session selects it, asks `STATUS`, or wakes from IDLE: it reads `changes_since(synced_seq)`, appends new members in `archived_at` order, raises the `modseq` of members that changed and tombstones members that left. Nothing is computed for views no one looks at.
- **One catch-up per view at a time.** Catch-up is single-flight per view in the process and a compare-and-set on `synced_seq` and `uidnext` in the store, so two sessions selecting the same view never hand out a UID twice.
- **Big views are built ahead.** The first build of a view assigns a UID to every member (one million rows for a large `All Mail`). When the first device with `imap` or `jmap` is made for an archive, a background job builds its views and Settings → Devices shows the progress (`GET /api/devices/serving`: per archive, built mailboxes and `assigned` of `total` over every mailbox, a built one counting in full so the bar only grows); a `SELECT` of a view still building answers `NO [UNAVAILABLE]` and clients retry. Later catch-ups are incremental.
- **`UIDVALIDITY`** is random when the view is created and changes only when the served tables are lost. They live in the database and go into every backup; a restore from blobs alone bumps it, and clients resync once.
- **Pruning.** The `changes` log keeps 90 days (CLIENTS). A view whose `synced_seq` is older rescans its membership: UIDs stay, every member gets a new `modseq`, clients refetch flags. Tombstones older than `lowest_modseq` are dropped; a QRESYNC client older than that gets the full UID set (`VANISHED (EARLIER)` covers the known range).

### IMAP

Library crate `serve-imap`, on `imap-next` 0.3 with `imap-codec`/`imap-types` pinned exactly and kept inside the crate (all `MIT OR Apache-2.0`; see [Libraries](#libraries-and-licenses)).

- **Capabilities:** `IMAP4rev1 IDLE UIDPLUS MOVE UNSELECT ENABLE CONDSTORE QRESYNC LITERAL- APPENDLIMIT=<n> SASL-IR ID NAMESPACE SPECIAL-USE CHILDREN AUTH=PLAIN`, plus `STARTTLS` and `LOGINDISABLED` before TLS. `LITERAL-` rather than `LITERAL+`: non-synchronising literals stop at 4 KiB, so a literal over the server's limit is refused before the client uploads it. As built, `imap-next` has one literal limit for every session (50 MiB), so a literal over it gets `BAD` before upload, while a message over the device's smaller append limit is refused with `NO [TOOBIG]` after it arrived. Before sign-in a command may hold at most 8 KiB and sign-in has one 60-second deadline for the whole exchange, so an unauthenticated client cannot make the server buffer a large literal. `IMAP4rev2` waits for an `ESEARCH` response in `imap-types`. `\Recent` is never set. `COMPRESS=DEFLATE` comes later.
- **One archive per device.** The IMAP namespace is the device's `mail_archive`; a user with two archives adds two accounts, which also matches their identities.
- **`ID`** answers `name "Holtmail"` and an opaque `holtmail-instance` value (a hash of the instance id), used by the [loop guard](#loops); no version.

| Command | What it does |
| --- | --- |
| `LIST`, `LSUB`, `STATUS`, `SELECT`, `EXAMINE`, `SUBSCRIBE`, `UNSUBSCRIBE` | From the served tables and store counts; never reads a blob. `All Mail` with 100 000 messages selects without touching blobs |
| `FETCH` `ENVELOPE`, `FLAGS`, `INTERNALDATE`, `RFC822.SIZE`, `UID`, `MODSEQ` | From the store. `INTERNALDATE` is `received_at` |
| `FETCH` `BODY[]`, `BODY[]<o.n>`, `BINARY` | The archived bytes (CRLF-normalised as in [Principles](#principles)) |
| `FETCH` `BODYSTRUCTURE`, `BODY[n]`, `BODY[HEADER.FIELDS]` | Parsed once and cached as derived data by blob (as built: `mailview-archive`'s MIME walker, which keeps the byte ranges sections need, with an in-memory cache, not `mail-parser`) |
| `FETCH ... (CHANGEDSINCE)`, `SELECT (QRESYNC ...)`, `VANISHED` | From the served `modseq` and tombstones. Read-only devices get them too: they are how clients resync flags cheaply |
| `STORE` | One `mail_changes` flags row per writable place, or a local flag. `UNCHANGEDSINCE` against the served `MODSEQ`, failing with `[MODIFIED]` |
| `COPY`, `MOVE` | Same account: a `move` change (to the Trash role: `delete` semantics, Phase 60). Other account: `append` of the archived bytes, plus `delete` for `MOVE`. Into `All Mail` or `Only in Holtmail`: `NO [CANNOT]` |
| `EXPUNGE`, `UID EXPUNGE` | `\Deleted` messages become `delete` changes (Trash, or expunge with `mail.expunge`). They leave the view; `All Mail` keeps them |
| `APPEND` | To a real folder: needs `mail.append`; archives the bytes first through the `Archiver`, then queues an `append` change and answers `APPENDUID` at once. The same bytes are the same blob, so a repeat returns the existing UID. Drafts and Sent are special (below) |
| `SEARCH`, `UID SEARCH` | Text keys (`TEXT`, `BODY`, `SUBJECT`, `FROM`, `TO`, `CC`, `HEADER`) go to the index through a new `SearchIndex::matching`; flags, dates, size, UID sets and `MODSEQ` are filtered in the store. The index matches words and word beginnings (and stems), never a piece from inside a word: documented. `matching` returns `Matches { ids, total }`, the newest at most `MAX_MATCHES` (100 000) ids and the full count |
| `IDLE` | Waits on the change hub; sends `EXISTS`, `EXPUNGE` or `VANISHED`, and `FETCH (FLAGS MODSEQ)` |

- **Implicit `\Seen`.** `BODY[]` without `.PEEK` sets `\Seen` only when the device's `implicit_seen` is on and the place is writable for flags (or local flags apply). Otherwise it behaves as `BODY.PEEK[]`, the archive's default. Off by default, because backup and migration tools fetch `BODY[]` for every message, and Apple Mail and Thunderbird set `\Seen` with an explicit `STORE` anyway. (The web app's "mark as read when opened" is a per-browser choice and does not apply here.)
- **Answers show the held state.** A `STORE` reply carries the flags the client asked for. If writeback later loses to the server (`superseded_by_server`) the flag changes back with a new `MODSEQ`, and CONDSTORE clients converge. A refused move puts the message back in its old view with a new UID, never a reused one.
- **IDLE at scale.** Sessions wait on the change hub, not on the store. As built, an idling session also looks for changes every 60 seconds (`serve_imap::Settings::poll`) as a backstop for a missed wake-up. The change hub reads each new batch of `changes` once and wakes only the sessions of archives (and, for per-user rows, users) it touches, at most once per 250 ms per session; a woken session then catches up its one view. Each idle connection costs a task and its TLS buffers, about 64 KiB; the connection limits above bound the total.
- **Large responses:** `imap-next` encodes a whole response in memory, so big `BODY[]` responses run under a byte semaphore.
- **As built (Phases 68 and 69):** `imap-next` 0.3.4 with `imap-codec` 2.0.0-alpha.10 and `imap-types` 2.0.0-alpha.8 (earlier alphas wrote `FETCH (MODSEQ n)` wrong); `AUTH=LOGIN` too, for Outlook; `BINARY` not offered; `ID` carries the instance token kept in the `instance_token` setting. Every implicit close by `SELECT`/`EXAMINE` sends `* OK [CLOSED]`, enabled `QRESYNC` or not: Dovecot's `imapc` (and so `doveadm backup`) drops the connection without it. `[serve.imap]` takes `listen_tls`, `listen_starttls` and `hostname`; `IDLE` subscribes each session to `ChangeHub` for its archive and user. Proved by the `serve_imap` integration suite.
- **Client quirks covered by fixtures:** Outlook's `FETCH 1:* (UID FLAGS)` without CONDSTORE (one store query), Apple Mail's need for `SPECIAL-USE` and `NAMESPACE` and its error loop when a Sent `APPEND` fails (so Sent always accepts, below), iOS dropping IDLE in the background, mutt expecting `\Recent`, Thunderbird with CONDSTORE always on and its offline download of every subscribed folder, K-9 idling on several folders at once.

### JMAP

Library crate `serve-jmap`: an axum router over the same `MailView` and `Submission` contracts, mounted by `apps/server` at `/jmap` and `/.well-known/jmap` (307), so it shares the listener, proxy, compression and limits.

- **Capabilities:** `urn:ietf:params:jmap:core`, `:mail`, `:submission`. Later `:websocket` (RFC 8887) and `:contacts` (RFC 9610). Calendars over JMAP wait for clients; CalDAV is the path for PIM.
- **One JMAP account per archive** the device covers; `isReadOnly` from the grant and scope.
- **Not offered:** `VacationResponse`, `Email/copy`, `Identity/set` (identities are made in the web app; `mayDelete: false`), `Email/queryChanges` in v1 (`cannotCalculateChanges`).

| JMAP | Holtmail |
| --- | --- |
| `Mailbox` | The mail view's tree; `myRights` from the locks, scope and local flags |
| `Email` id, `Thread` id | `MessageId`, `ThreadId` with a type prefix; a draft is `D<draft>.<version>` |
| Blob id | `B` + the BLAKE3 hash of the raw bytes; parts `B<hash>.<partId>`, cut out with `mail-parser` at download |
| `Email/get` | Stored fields for list properties; `bodyStructure`, `bodyValues`, `header:*` parsed from the blob; `keywords` are effective flags |
| `Email/query` | `inMailbox` sorted by `receivedAt` from the store; text conditions through `holtmail-query` to the index; other sorts `unsupportedSort`; `collapseThreads`, anchors and `calculateTotal` supported |
| `SearchSnippet/get` | Index highlighting |
| `Email/set` keywords | `$seen`, `$flagged`, `$answered`, `$draft` as system flags, others as keywords; `mail_changes` or local flags |
| `Email/set` `mailboxIds` | Remove and add in one account: `move`. Remove only: delete by the Phase 60 rules. Add only, or add in another account: `append` of the archived bytes |
| `Email/set` destroy | Expunges every place (`mail.expunge`, which needs `mail.delete`), never a move to the trash; the email moves to `Only in Holtmail`. Destroying an email already only there is `forbidden` |
| `Email/import`, `Email/set` create | Into Drafts (below), or `append` into a real folder after archiving (`mail.append`) |
| Upload | Into a blob store of its own (`<data_dir>/jmap-uploads`, emptied at start); an upload is usable by its uploader for 24 hours and then deleted unless another live upload holds the same bytes. Importing or sending copies the bytes into the archive. Only devices with `write:mail` or `send:mail` upload, within 200 MiB per user at a time |
| `Identity/get` | The identities the user may send as in this archive |
| `EmailSubmission/set` create | `Submission::queue` (below), `idempotency_key` = device + creation id (as built: a hash of device, email id and creation id, because clients reuse creation ids such as `k1` across requests). The queued bytes get CRLF line ends. Without an `envelope`, recipients come from `To`, `Cc` and `Bcc`; the `Bcc` header is removed from the bytes that are queued |
| `undoStatus` | `pending` during the undo window; setting `canceled` calls `undo` |
| `deliveryStatus` | Per-recipient outbox results |
| `onSuccessUpdateEmail`, `onSuccessDestroyEmail` | Run after the queue accepts. The draft leaves Drafts and the sent copy arrives through the outbox `Archiver` as a new Email: clients see the draft destroyed and a new Email created in Sent, not the draft id moving (documented) |
| `/changes`, `ifInState` | The change feed; a `seq` older than the prune gives `cannotCalculateChanges` |
| Push | `GET /jmap/eventsource` (`ping`, `closeafter=state`, `X-Accel-Buffering: no`). `PushSubscription` comes later ([WORKPLAN, Serving](WORKPLAN.md#serving-planned-2026-11-08)): it needs a push sender, and its URL is chosen by the client, so it goes through an egress guard (https, public addresses only) and sends the RFC 8620 verification push first |

- **Method dispatch is our own.** Stalwart's server-side `jmap-proto` is AGPL and not on crates.io. `PatchObject` and result references are a small module of our own; `jmap-tools` 0.1 (`Apache-2.0 OR MIT`, already in the lock through `calcard`) gets a short spike first.
- **Thread merges.** Holtmail merges threads into the oldest one, while RFC 8621 says `threadId` never changes. The server reports the old thread destroyed and its emails updated; this is documented.
- **CORS** on `/jmap` for web clients such as Twake Mail, with credentials only from the `Authorization` header.
- **As built (Phases 70 and 71):** `[serve.jmap]` (`enabled`, default true) merges the routes into the main listener and needs `public_url`. Bearer tokens carry `<login>:<app password>`, since `DeviceAuth` verifies a secret with its login; a login-free token waits for device tokens. The instance token is the session capability `urn:holtmail:params:jmap:instance` (`{"token": ...}`). Ids are rewritten into `[A-Za-z0-9_-]` (`role-inbox`, `folder-<id>`, `M<message>`, `D<draft>_<version>`, `B<hash>`, part blobs `P<part>_<email>`). The `EmailSubmission` log and the 24-hour upload registry are kept in memory, so after a restart old states answer `cannotCalculateChanges` and uploads are gone. Browsers are kept out of cached credentials: a request with `Sec-Fetch-Mode` or `Origin` gets a `Bearer` challenge instead of `Basic`, so a browser never prompts for or keeps the app password; a cross-site navigation or `no-cors` request is refused; `/jmap/api` takes only `application/json`, which a browser preflights; a WebSocket handshake from another origin than the host is refused. `:websocket` (RFC 8887) is served as well. Details in [serve-jmap](../crates/serve-jmap/docs/README.md).

### SMTP submission

Library crate `serve-submit` over a generalized `holtmail-smtp` (it stays one way to speak SMTP as a server, now for submission as well as inbound).

**Changes to `holtmail-smtp`:** `Options.credentials` (today one fixed `Credentials`) becomes `Arc<dyn SmtpAuth>` returning an opaque principal; implicit TLS (`Options.implicit_tls`, port 465); `Handler::mail_from(&Session, addr)` and the session passed to `recipient` and `deliver`; `Envelope.principal` instead of `authenticated`; `PIPELINING`, `8BITMIME`, `ENHANCEDSTATUSCODES`. The inbound receiver keeps working through a small adapter.

**Checks, in order:**

1. `AUTH` with a device that has `smtp` and `send:mail`. Port 587 requires `STARTTLS` first.
2. `MAIL FROM`: an active identity of the user in the device's `mail_archive` whose address, or plus address, is the return path; a `sender` grant; `[write] send` and `mail.send` for the identity's transport; the user's submission limits. A null reverse path (`<>`) is refused: every send needs an identity. Otherwise `553 5.7.1`, except a used-up limit, which is `451 4.7.1` so the client tries again later.
3. After `DATA`: the header `From` must be one address and the same identity (`554 5.7.1`). RFC 6409 §8 fix-ups only: `Date`, and `Message-ID` `<uuidv7@identity-domain>` if missing. No `Received:` line with the client's address is added. Any `Bcc:` header is removed; envelope recipients are kept and stored in the outbox record, as for every send.
4. **Leak guard** (WRITE.md, Replies through relay mailboxes): when the message answers a thread that arrived through an alias and a recipient would see the real mailbox, it is refused with `550 5.7.1` and a text that says why without naming addresses; the web app shows the same send with its one-click fix. Holtmail never rewrites a client's recipients.
5. `Outbox::queue` with the route built for that identity, the user and device, `idempotency_key = hash(device, envelope recipients, Message-ID)`, or the bytes when the message has no `Message-ID` (a client that retries after a lost `250` does not send twice, and a client that sends one message in several transactions, say one for its Bcc recipients, still reaches them all) and `send_at = None`. The outbox checks `allowed_from`, size and recipient limits again. `OutboxError::Invalid` is `554`, a store error `451`.

- **Undo:** `undo_seconds`, default 0, because the client already had its own.
- **`SIZE`** advertises the smallest `max_size` among the identity transports the user may use, default 25 MiB. Bigger messages are read to the end and refused with `552`. `SMTPUTF8` is offered only when every transport the user may use has it, as the last transport check found (an SMTP server's `EHLO`; JMAP and Graph count as not having it). Before sign-in nothing is known about the user: `EHLO` says 50 MiB and no `SMTPUTF8`, so a client that does not repeat `EHLO` after `AUTH` sees those, and the real limits are enforced at `MAIL` and `DATA`. `DSN` (RFC 3461), `CHUNKING` and `BINARYMIME` are not offered.
- **Later failures** of the outbox (`failed`, or `sent` with a refused recipient, for mail from a device) become a notice, the outbox row's `note`, plus an RFC 3464 DSN ingested into the user's archive, as a local place in the user's `INBOX` (built in Phase 67), so the mail client shows the bounce in its inbox.
- **Rules on `message.sending`** (Phase 63) apply to submitted mail as to web mail, through the same route, once Phase 63 lands. They may block, add headers or a Bcc to self; they never change the identity of a message a client composed.

### Drafts and Sent copies

- **Drafts churn.** Thunderbird and Apple Mail `APPEND` every autosave and expunge the previous one; JMAP clients re-create a draft on every save. A draft written to `Drafts` over either protocol goes into Holtmail's draft store (Phase 56, with versions and retention) and is never archived. The draft store gains a `raw` draft kind (the client's bytes as a blob instead of the compose payload); the web app opens it like "Edit as new". The `Drafts` view is per user and serves those drafts, web drafts rendered by `holtmail-compose` (deterministically, so one draft version keeps its UID), and the upstream Drafts folders. Each saved version is a new UID, as IMAP messages are immutable. Deleting one there deletes the draft. A draft enters the archive only when sent (the outbox archives the sent bytes) or when the identity's account has `mail.append` and the user chose to keep drafts upstream.
- **Sent copies.** A client that sent through Holtmail usually `APPEND`s its own copy to `Sent`. Its bytes often differ (Bcc kept, headers folded). The copy is matched by `Message-ID` against the same user's outbox rows of the last 24 hours (not failed or cancelled), answered with the UID of the outbox's own archived message and not appended upstream, because the identity's sent-copy policy already handled that. As built, the client's bytes are dropped rather than archived and linked with `link_outbox_copy` (that field stays for a provider's rewritten copy): archiving them would put a second copy of one message in Sent. When the outbox has not archived its message yet, the view archives the outbox's own bytes, which is the same message the outbox records later, and when no folder of that Sent mailbox holds it, it becomes the user's local place in `Sent`. Bytes that match nothing are archived and appended like any other `APPEND`; where the account lacks `mail.append` they are archived as a local place in `Sent` instead of refused, because Apple Mail retries a failed Sent `APPEND` forever.

### CalDAV and CardDAV

Library crate `serve-dav` (the `dav-serve` of CLIENTS): our own thin layer on axum, `quick-xml` and `calcard`. `dav-server` is built around files and lacks ACL, REPORTs, sync and scheduling; Stalwart's DAV crates are AGPL and only a reference.

**Layout:**

- One principal per user: `/dav/principals/<user>/`; homes `/dav/calendars/<user>/` and `/dav/addressbooks/<user>/`.
- `PROPFIND` on `/` and on `/dav/` answers `current-user-principal` (some clients ignore the well-known redirect and probe the root); every other method on `/` stays the web app's.
- Every collection the device's archives hold that the user can read, named by collection id, with its display name: source collections, "From mail" (only invitations whose UID no other served calendar has) and the merged "People" book, listed first because iOS shows one address book per account.
- `supported-calendar-component-set` is what the source collection holds (`VEVENT`, `VTODO`); VTIMEZONEs are served inside resources.
- Each resource's href is `items.dav_name`, kept forever (CLIENTS, What is served).
- **vCard versions.** Source books are served as stored. A client that asks for vCard 3.0 in `address-data` or `Accept` gets a `calcard` conversion, with its own ETag. "People" is generated as vCard 3.0, which every client reads, including iOS and macOS Contacts.

**Methods and reports:**

- `OPTIONS`, `PROPFIND` (Depth 0 and 1; `infinity` is 403 `propfind-finite-depth`), `PROPPATCH`, `GET`, `HEAD`, `PUT`, `DELETE`, `REPORT`.
- Reports: `sync-collection`, `calendar-multiget`, `addressbook-multiget`, `calendar-query` (time range through `holtmail-recur`, plus component and UID filters), `addressbook-query` (text match), `free-busy-query`, and `principal-property-search` that finds only the user's own principal. People are not principals: Apple would then treat contacts as users of this server and try to deliver to them internally. Apple completes invitee names from Contacts, where People already is.
- `PROPPATCH` of `displayname`, `calendar-color` and `calendar-order` is stored as per-user display properties and never sent upstream; Apple shows an error when it is refused.
- `MKCALENDAR` and `MKCOL` are 403: collections come from accounts.

**Sync:** sync-tokens and `getctag` from the change feed; an unknown or pruned token is 403 `valid-sync-token`; a cut result is 507 on the request URI.

**Writes** follow CLIENTS.md exactly: the advertised privilege is `write_mode` × archive role × device scope; `PUT` runs `pim_edit::validate` and `classify` and then `record_change` with the client's `If-Match`; the ETag and 412 table of [CLIENTS, What is served](CLIENTS.md#what-is-served) applies; an item removed upstream stays served; the only 5xx is 503 with `Retry-After`. `write_through` collections reach upstream through `pim-writeback`, `held` ones stay held. A collection moved from `held` to `read_only` still accepts writes as held changes for 24 hours (CLIENTS, Mode changes).

**Held creates are served.** WRITE.md's Phase 61 decision left held creates unserved because a create has no item until writeback makes one. A DAV `PUT` with `If-None-Match: *` must answer 201 and be listed at once, so Phase 73 adds a local item keyed by the href's `dav_name`, served from the change's bytes; writeback later fills in its upstream id, and the href never changes. The web app's held creates show through the same item. As built, no local item row is needed: the store already serves an href with an open create (`ChangeTarget::Href`) and `open_changes` lists it, and writeback copies the href into the new item's `dav_name`; the web app's held creates are served to DAV clients the same way, while its own lists still wait for writeback (WRITE.md, Phase 61 decision).

**As built (Phases 72–73):** the routes are merged into the main listener by `apps/server` after the web app's fallback, and `root_propfind` wraps the whole router; `[serve.dav]` takes `enabled` and `edits`, and `scheduling` arrives with Phase 74. Hrefs holding `/` (upstream paths) are served as `~` plus base64url, because some clients decode `%2F`. `getctag` and sync tokens use the archive's newest `item` row, so a change in another collection of the archive moves them too (the client then gets an empty `sync-collection`). The change log's `item` rows now also come from upstream versions, removals and returns, not only from edits. The 24-hour grace after `held` → `read_only` is read from the collection's own `write_mode_changed_at` and `previous_write_mode` (migration 0041), not from the audit log. An invitation deleted for good with its mail is reported 404 by the next `sync-collection` (its change-log row keeps the href), and "From mail" re-checks invitations whose `UID` another calendar of the archive gained or lost. Whole answers to the Apple, DAVx5 and Thunderbird request shapes are kept as golden files.

**Live updates:** WebDAV-Push server side once CLIENTS Phase 47 has a push sender (`notify-webpush`); DAVx5 and Apple poll until then.

### calendar-auto-schedule

RFC 6638 obliges the server to deliver invitations and replies. Holtmail advertises it only when it can. It needs WRITE Phase 62 complete (RSVP and organizer routes in the web app), not only its built foundation.

**Advertised** (`DAV: calendar-auto-schedule` in `OPTIONS`) for a principal only when all hold:

- `[serve.dav] scheduling = true`;
- the device has `write:pim` and `send:mail`;
- the user has an active identity, with a `sender` grant, for at least one address in `calendar-user-address-set` (identity addresses plus the alias addresses they cover, as `mailto:`);
- every writable calendar has a reply route and an organizer route (`pim_edit::schedule`).

Otherwise Holtmail keeps the held-reply behaviour of CLIENTS Phase 51 and says nothing. As built, the last condition is not checked per request: whether a calendar's server schedules is a live probe that fails whenever that server is unreachable, and dropping scheduling for one request after a client saw the capability would record a change nobody tells the attendees about. A scheduled write to a calendar whose host cannot be told now answers 503 instead.

**Principal properties:** `schedule-inbox-URL`, `schedule-outbox-URL`, `calendar-user-address-set`, `schedule-default-calendar-URL`; `schedule-calendar-transp` on calendars.

**Schedule inbox:** one resource per incoming iTIP message addressed to an own address. `DELETE` there only hides it; the archived message stays.

**Schedule outbox:** `POST` free-busy for the user's own addresses from served events; any other address answers `REQUEST-STATUS:3.7`.

**Schedule-Tag:** a new column `items.schedule_tag`.

- It changes on a scheduling-relevant change by the organizer and on any client `PUT`.
- It does not change when the server merges a REPLY's `PARTSTAT` into the organizer's copy, so Apple never gets a false 412.
- A wrong `If-Schedule-Tag-Match` is 412. An attendee `PUT` with a matching tag is reduced to what RFC 6638 §3.2.2.1 allows, which the `reply` class already models. An organizer `PUT` with a matching tag keeps every other attendee's `PARTSTAT` from the served copy (RFC 6638 §3.2.10), so answers merged since the client last read the copy survive.
- A new upstream version (an edit made on the calendar server or in another client) unpins the tag, unless its bytes are exactly the merged reply Holtmail wrote back.
- A `PUT` whose stored bytes the scheduler changes (it writes `SCHEDULE-STATUS`) returns the `Schedule-Tag` and no ETag, so the client refetches (CLIENTS's ETag rule).

**Outgoing scheduling,** for a `PUT` or `DELETE`, delivered just before the change is recorded (the organizer's copy to record is the one with `SEQUENCE` raised and `SCHEDULE-STATUS` written). Its messages are held in the outbox and recorded with the change; `confirm` lets them into the undo window once the change exists, and `withdraw` takes them back when recording fails, so neither happens without the other whatever the undo window, and a stop in between is resolved at the next start (held messages of recorded changes are released, the others stay held):

| Who | Route |
| --- | --- |
| Organizer, `REQUEST` or `CANCEL` | `organizer_route`: write-through on a scheduling upstream (it sends), `Deferred` while the change is held there, otherwise `pim_edit::organize` → `compose::imip` → `Outbox::queue` with the routed identity |
| Attendee, `REPLY` | `reply_route`: `respond` on RFC 6638 CalDAV, Graph or JMAP, otherwise an iMIP REPLY through the outbox. An invitation that came through an alias is answered as the alias (`route::attendee_address`), and one that came through an alias service is sent to its reverse alias or `Reply-To` (`Routed::to`), never straight to the organizer |

- `SCHEDULE-AGENT=CLIENT` or `NONE` on an attendee: nothing is sent for that attendee.
- Client-sent `SCHEDULE-STATUS` and `SCHEDULE-FORCE-SEND` are stripped. The server writes `SCHEDULE-STATUS` (RFC 6638 §3.2.9) on each attendee: `1.0` pending (held or deferred), `1.1` sent (queued in the outbox, or written through to a scheduling upstream), `1.2` delivered (only when an upstream scheduler reports it), `3.7` no usable identity or route, `5.1` the outbox failed or the message bounced. As built, `5.1` covers only a submission refused when it is made; a message that fails or bounces later keeps `1.1` and shows in the outbox, because writing a status back would be one more change of the organizer's copy for every bounce. An iTIP REPLY never changes `SCHEDULE-STATUS`; it changes `PARTSTAT`.

**Incoming iTIP from mail.** An ingest hook runs on live arrivals (never backfill or import) that carry `text/calendar` with `METHOD:REQUEST`, `CANCEL` or `REPLY` addressed to an own address, and that are not in a Sent folder, not an outbox copy and not `From` an own address:

1. The message becomes a schedule-inbox resource.
2. `REQUEST` and `CANCEL` update the attendee copy in "From mail", unless a served upstream calendar already holds the UID (Google adds invitations itself), or the `ORGANIZER` is an own address of the same user (the organizer's copy already exists).
3. `REPLY` merges `PARTSTAT` into the organizer's copy as a change with origin `itip`.
4. **Anti-spoofing:** a REPLY is applied only when `From` is the attendee and the message's stored auth facts ([INSIGHTS.md §4.4](INSIGHTS.md), from the topmost `Authentication-Results` of the account's own server) show DKIM or DMARC pass for its domain. As built: only a field whose authserv-id the operator trusts for the account the mail came through counts (INSIGHTS §4.3; with no trusted id nothing passes), the mail must have exactly one `From` address, and DMARC counts only for the attendee's own domain (`header.from`). Otherwise it stays in the inbox, unapplied, with a note; the web app offers "Apply anyway" to the organizer.

**No double sending.** With auto-schedule on, a client may still mail its own iMIP through submission (Thunderbird with `calendar.caldav.sched.enabled` off). `serve-submit` accepts with `250` but drops an iMIP message whose UID, `SEQUENCE` and method the scheduler delivered in the last 10 minutes, and records the drop. As built, the match also takes the `RECURRENCE-ID` and, for a REPLY, the answer, and every envelope recipient must have been sent that message: a decline after a web accept, a per-instance answer, a message to a `SCHEDULE-AGENT=CLIENT` attendee or a forward to someone new is a message of its own and goes out. The web app's RSVP buttons (Phase 62) use the same `Scheduler`, so web, DAV and mail cannot answer twice.

**As built (Phase 74):** `holtmail-schedule` is the contract and `schedule-itip` its implementation over `pim-edit`, `compose` and `Submission`; one instance is shared by the API, `serve-dav` and `serve-submit`. The scheduler never writes to the store: it returns the bytes to record and its decisions, and the caller records them. `items.schedule_tag` (migration 0042) holds a tag only while a merged reply pins it; otherwise the `Schedule-Tag` is the ETag, and any change with an origin other than `itip` unpins it. `calendar-user-address-set` is the active identities' addresses and the aliases they cover; the "sender grant" condition is `holtmail-access`'s `may_send_somehow` in one of the device's archives, and "a reply route and an organizer route" means the server's writers can say how every writable calendar's host schedules. `OPTIONS` advertises only to a caller with valid credentials. The schedule inbox is a store table of entries (message, calendar part, note), hidden per user by `DELETE`; "From mail" keeps following the derived invitations and its `UID` rule, and the hook records the decision as the note. An incoming `REPLY` is merged only into copies in `write_through` calendars whose server does not schedule (as Phase 62 built it); the DKIM check counts the attendee's domain or a parent of it, from the topmost `Authentication-Results`, and "Apply anyway" (`POST /api/messages/{id}/itip/apply`, the web card's button) skips only that check. The duplicate check compares message content without `DTSTAMP` instead of the tuple with `DTSTAMP` (each message gets a fresh one), and what was delivered recently lives in memory, so `just_sent` and the submission drop reset at a restart; a message undone in the outbox no longer counts, and an outbox key that names a cancelled message is followed by a new one. Free-busy answers from every calendar the device sees. Review fixes: messages are held until `confirm` (above), the advertisement no longer depends on a live host probe, a matching tag keeps merged answers, the pin is set in the same transaction as the merge and dropped by foreign upstream versions, the reply check reads only trusted `Authentication-Results`, "Apply anyway" honours the instance switch, and an answer's `References` are at most 20 usable ids.

## Loops

| Loop | Guard |
| --- | --- |
| Holtmail added as its own upstream (an IMAP or JMAP account pointing at this instance) | `source-imap` reads `ID` and `source-jmap` the session's `holtmail-instance` value, and refuse an account on the same instance (`SourceError` with a plain reason). A second instance is allowed: that is the loopback test |
| A transport that submits to Holtmail's own submission | `submit-smtp` refuses when the server's greeting carries this instance's token; the outbox fails the row with a plain reason instead of retrying |
| Scheduling ping-pong | A server-applied REPLY never sends anything (RFC 6638 allows the organizer's server to tell other attendees; Holtmail does not). Incoming REQUEST and CANCEL never send a REPLY by themselves; only a user's `PARTSTAT` change does. iTIP from an own address is never applied as incoming |
| An invitation to the user's own alias | The REQUEST mailed to the alias comes back by mail; its `ORGANIZER` is own, so it is not applied (above) |
| Storms from a misbehaving client | Outgoing iTIP is deduplicated per (UID, recipient, method, `SEQUENCE`, content without `DTSTAMP`) and counts against the submission limits; a client rewriting the same event in a loop sends at most one message per change that matters (`noop` and `personal` classes send nothing) |
| Bounces of bounces | DSNs are never sent by Holtmail; outbox failures become notices and local-place DSNs only, archived as not live so they never count as a bounce of the row they describe |

## Change notification

One feed drives every push channel:

```
 store commit (changes row) ──► ChangeHub ──┬──► IMAP IDLE
   (ChangeWatch, poll fallback)             ├──► JMAP EventSource (later PushSubscription)
                                            ├──► DAV sync-token (later WebDAV-Push)
                                            └──► /api/live (CLIENTS Phase 45)
```

- The store contract gains `watch_changes() -> Box<dyn ChangeWatch>` with `async fn next(&mut self) -> u64` (the highest committed `seq`), the same shape as `SourceWatch`, so no tokio type crosses the contract. It wakes after each commit that wrote a `changes` row. A 30-second poll of `latest_seq` backs it up, so a missed wake-up delays at most one tick and a future multi-process store still works.
- `ChangeHub` (in `apps/server`) reads `changes_since` once per wake-up and fans out by archive, and by user for per-user rows. Listeners never get content from the hub, only "something after `seq` changed in your archive"; each session then reads its own view, filtered by its own grants.
- A backfill or import writes `changes` rows like anything else, so clients see the mail; notifications to people (`notify`) still go only for live arrivals.

## Modules

| Module | Kind | Job |
| --- | --- | --- |
| `holtmail-device` | Contract | `DeviceAuth::verify(login, secret, peer, protocol) -> Principal { user, device, mail_archive, archives, scopes }`, the revocation feed, `DeviceAuthError::{Invalid, TooMany, Revoked}`; fake and conformance suite |
| `device-local` | Implementation | Over the store's `DeviceStore` and `holtmail-auth` hashing and limiter |
| `holtmail-access` | Library over the store contract | The one decision for every write: switch × capability × grant × scope × sign-in consent (today `may_write`, `writable` and `may_send` in `holtmail-api`, with `holtmail_mail_writeback::write_consent_missing`). The API, the mail view, submission, DAV and the scheduler call it |
| `holtmail-submission` | Contract | `Submission` (queue with identity checks, undo, status) and its error enum; fake and conformance suite. Its own crate so SMTP submission does not wait for the mail view |
| `submission-outbox` | Implementation | Over `Store`, `BlobStore`, `holtmail-route`, `holtmail-access` and `holtmail-outbox`. The API's send handler moves onto it |
| `holtmail-mailview` | Contract | `MailView` (tree, snapshot, served UIDs, envelope, raw, structure, effective flags, search, `changes_since`) and `MailActions` (flags, move, copy, delete, append, drafts, subscriptions); error enum; fake and conformance suite |
| `mailview-archive` | Implementation | Over `Store`, `BlobStore`, `SearchIndex` and `holtmail-access`. As built, the API's Phase 60 mail-change endpoints keep their own planner: they answer with the recorded change rows, skip reasons, labels and an account filter, which `MailActions` does not model; both decide through `holtmail-access` and `mail-writeback`'s `capability_for`, and the endpoints move onto `MailActions` when the web app reads the mail view |
| `serve-imap` | Protocol library | IMAP server over `MailView`, `MailActions` and `DeviceAuth` |
| `serve-jmap` | Protocol library | JMAP router over `MailView`, `MailActions`, `Submission` and `DeviceAuth` |
| `serve-submit` | Protocol library | Submission handler over `holtmail-smtp`, `Submission` and `DeviceAuth` |
| `serve-dav` | Protocol library | CalDAV and CardDAV router over the store, blobs, `holtmail-pim`, `holtmail-pim-edit`, `holtmail-access`, `DeviceAuth` and `Scheduler` |
| `holtmail-schedule` | Contract | `Scheduler`: prepare a client's write, deliver organizer and attendee iTIP held before the change is recorded, confirm or withdraw it after, decide what incoming iTIP does (the caller records it), answer "was this just sent"; fake and conformance suite. It never writes to the store |
| `schedule-itip` | Implementation | Over Phase 62's routes and iTIP builders in `pim-edit`, `compose` and `Submission` |
| `holtmail-smtp` | Library (changed) | Generalized for submission: principal, implicit TLS, `mail_from`, `PIPELINING` |
| `holtmail-proxy-protocol` | Library | Parses PROXY v2 headers (address, SSL TLV); no I/O. Our own, about 200 lines, fuzzed; `ppp` (Apache-2.0) is the fallback |

- `serve-*` crates are protocol front ends, not implementations of a contract; there is deliberately no `holtmail-serve` crate. They receive contracts and never construct implementations. Only `apps/server` wires them, behind `[serve]`. AGENTS.md gains one line saying so.
- No `imap-types`, `axum`, `quick-xml` or `calcard` type crosses a contract boundary.

## Libraries and licenses

Holtmail is `MIT OR Apache-2.0`; everything linked must be compatible with both.

| Library | License | Use |
| --- | --- | --- |
| `imap-next` 0.3, `imap-codec`, `imap-types` 2.0 alphas (pinned exactly) | `MIT OR Apache-2.0` | IMAP server state machine and codec |
| `calcard` 0.3, `jmap-tools` 0.1 | `Apache-2.0 OR MIT` | Already in the workspace |
| `mail-parser`, `mail-builder` | `Apache-2.0 OR MIT` | Already in the workspace |
| `quick-xml`, `axum`, `tokio-rustls` | MIT, MIT, `MIT OR Apache-2.0` | Already in the workspace |
| `jmap-client` (dev-dependency of `tests/integration`) | `Apache-2.0 OR MIT` | Third-party JMAP client in tests |
| Rejected: Stalwart `jmap-proto`, `dav-proto`, `groupware`, `imap` server crates | AGPL-3.0 | Reference only, never linked or copied |
| Test containers: Dovecot (`doveadm`, `imaptest` if an image exists), Apple CalDAVTester, vdirsyncer, Radicale, GreenMail, Stalwart | various (MIT, Apache-2.0, LGPL, AGPL) | Run as separate programs in `mise run it`; nothing is linked |

## Setting up a client

- **Settings → Devices** in the web app: add a device, pick protocols, the mail archive, other archives and scopes (with the consequences in plain words), show the app password once with the server names and ports (the full JMAP and DAV addresses for the HTTPS protocols), list devices with their last use and any protocol this server does not offer, revoke. Changes a device made name it where the web app shows held, failed and conflicting changes.
- **Per platform:** an Apple `.mobileconfig` with IMAP, SMTP, CalDAV and CardDAV ("People") accounts and a fresh app password, downloaded once over a short-lived link because it holds the secret (unsigned profiles show "Not Verified" on iOS; documented); DAVx5 with the address filled in.
- **Autoconfig and SRV only help when the operator controls the mail domain.** Thunderbird looks for `autoconfig.<domain>` and `<domain>/.well-known/autoconfig/...` of the user's address, and RFC 6186 and RFC 6764 SRV records and `_jmap._tcp` live under the same domain. Holtmail serves `/.well-known/autoconfig/mail/config-v1.1.xml` and the deployment docs give the records; everyone else enters the host by hand from Settings → Devices.
- **What clients cannot do:** iOS Mail has no push for IMAP accounts that are not iCloud, Gmail or Exchange; it fetches every 15 minutes or by hand, so Web Push from the PWA stays the way to be notified. The new Outlook for Windows and Gmail's "other accounts" reach IMAP through Microsoft's and Google's servers: Holtmail must be reachable from the internet and the app password is stored with them. The setup flow says both.
- **"Use Holtmail or the upstream account, not both"** on the same device, as CLIENTS says for DAV: two accounts showing the same mail or events give duplicates and double notifications.
- **As built (Phase 75):** the profile link is made for an existing device (`POST /api/devices/{id}/profile`, recent sign-in) and the fresh app password is minted when the link is used, replacing the device's secret (`DeviceStore::replace_device_secret`) and ending its live sessions, so no secret is ever held by the server in clear; links live 10 minutes in memory. Opening the link (`GET`, also `HEAD`) only shows a confirmation page; its button (`POST`) issues the profile, so link previews, mail scanners and chat unfurlers that fetch a pasted link neither use it up nor receive the app password. A store failure before the secret is replaced puts the link back. As an app on the iOS home screen the web dialog offers only _Copy link_. Mail goes into a profile when the device has IMAP and SMTP, calendars and contacts when it has DAV. Autoconfig answers only for the domains in `[serve] mail_domains` (also at `/mail/config-v1.1.xml` for `Host: autoconfig.<domain>`), `holtmail dns` prints the records, and the DAVx5 link carries the login but never the password and is offered only over https. The setup notes are on Settings → Devices.

## Tests

- **Conformance suites** for `holtmail-device`, `holtmail-submission`, `holtmail-mailview` and `holtmail-schedule`, run by every implementation; store conformance cases for the new `changes` writers (a row only when something changed; visibility in `seq` order under concurrent writers), `DeviceStore` and `ServeStore` (UID stability across a held move, an upstream `UIDVALIDITY` reset and an account rename or role change (an upstream folder rename is a new mailbox as built); never reused; tombstones; concurrent catch-up hands out no UID twice).
- **Protocol unit tests** against fakes: scripted IMAP sessions and recorded transcripts of Thunderbird, Apple Mail, Outlook and K-9 as fixtures (rebuilt with invented mail); JMAP requests through `tower::ServiceExt::oneshot`; golden XML for the Apple, DAVx5 and Thunderbird DAV request shapes; SMTP dialogues with the existing in-process client; fuzzing of the PROXY v2 parser and the IMAP command path.
- **Loopback tests:** Holtmail's own clients against Holtmail's own servers, in process: `source-imap` and its `MailboxWriter` against `serve-imap`, `source-jmap` and `submit-jmap` against `serve-jmap`, `collection-dav` and `CollectionWriter` against `serve-dav`, `submit-smtp` against `serve-submit`. Two independent sides of each protocol in one repository. The same setup with one instance on both sides checks the loop guards.
- **Integration suites** (`mise run it`), each with a third-party client in a container:

| Suite | Clients | Checks |
| --- | --- | --- |
| `devices` | curl | App password per protocol, the web password refused, wrong secret limited, revoke ends a live IMAP IDLE and a JMAP stream |
| `serve_submit` | `submit-smtp`, the host's `curl --ssl-reqd` | Send through Holtmail into GreenMail; a Bcc-only recipient arrives without a Bcc header; wrong `From` `553`; oversize `552`; a retry gives one outbox row; the sent copy is archived; a transport pointing at Holtmail itself fails without a loop |
| `serve_imap` | `doveadm backup -R imapc:` (Dovecot 2.4.5), curl `imap://`, `source-imap` of a second Holtmail | Full mirror with QRESYNC; a held move on the Dovecot upstream keeps its served UID after sync confirms it; `STORE \Seen` arrives on Dovecot; IDLE wakes on a new delivery; a client `APPEND` to Sent links to the outbox row; the backup does not mark mail read |
| `serve_jmap` | `jmap-client`, curl | Well-known redirect, 401, CORS; inbox matches Dovecot; download bytes match the BLAKE3 hash; `$seen` reaches Dovecot and Stalwart; `ifInState` mismatch; `EmailSubmission` with undo into GreenMail; an EventSource `StateChange` after an SMTP delivery |
| `serve_dav` | vdirsyncer, curl, `collection-dav` | Discovery, sync with Radicale as the upstream, a held create listed at once, a held edit, a write-through edit, an upstream conflict, 412 only on Holtmail's own ETag, `valid-sync-token` after pruning |
| `serve_schedule` | curl, Stalwart | An organizer `PUT` with a Stalwart attendee: the REQUEST lands in Stalwart's IMAP and the attendee gets `SCHEDULE-STATUS:1.1`; the attendee's REPLY comes back by mail and sets `PARTSTAT` without changing the `Schedule-Tag`; an incoming REQUEST shows in the schedule inbox and in "From mail"; a forged REPLY without DKIM stays unapplied; a client iMIP after server scheduling is dropped; an invitation to an own alias is not applied back |

- **Spikes:** a Dovecot `imaptest` container image for compliance and stress (not yet checked that one exists); Apple CalDAVTester (Apache-2.0) in a container for the scheduling and sync scripts.
- **VERIFY:** Apple Mail, iOS Mail, Thunderbird, Outlook (new and classic), K-9/Thunderbird for Android, aerc, mutt, Twake Mail, Ltt.rs over JMAP; Apple Calendar and Contacts (vCard versions, single address book), DAVx5, Thunderbird, Evolution, GNOME Calendar; Apple Calendar organizing a meeting with Gmail and Outlook attendees; the `.mobileconfig` on iOS and macOS.

## Hard parts and their answers

| Problem | Answer |
| --- | --- |
| Upstream UIDs are unusable for clients | Served UIDs keyed by message, per view, never reused, kept across account renames and role changes (upstream folder renames: Later) |
| 100 000-message mailboxes | Views from the store only; blobs read only for `BODY[]`; views built ahead in the background; lazy catch-up from the change feed |
| A change committed out of `seq` order is never seen | `seq` order is visibility order, a store guarantee with a conformance case |
| Thousands of idle connections | One hub reads each change batch once and wakes only the sessions of touched archives, coalesced |
| A held change that upstream later refuses or overrides | The protocol showed it at once; it changes back with a new `MODSEQ` or state, and the web app shows the failure. Clients are never told why |
| Read-only accounts frustrate clients that expect read state | Local flags per user, never sent upstream |
| A backup tool marks everything read | Implicit `\Seen` is off unless the device turns it on |
| Drafts autosave would bloat the archive | Drafts go to the draft store as `raw` drafts, archived only when sent or kept upstream |
| The client's Sent copy duplicates the outbox copy | `Message-ID` link to the outbox row; no second upstream append; Sent never refuses |
| A client retries after a lost `250` | Idempotency key from device and `Message-ID` |
| A native client replies through an alias with the real address visible | The leak guard refuses with a reason; the web app offers the fix |
| JMAP thread ids must never change, Holtmail merges threads | Old thread reported destroyed, emails updated; documented |
| Index matches words, IMAP asks for substrings | Documented deviation; header keys that need exact substrings are filtered in the store when the candidate set is small |
| Apple's 412 loops and "Revert to Server" | CLIENTS's ETag table; Schedule-Tag stays still on server-merged replies; held creates served at once |
| A spoofed iTIP REPLY rewrites an organizer's event | Applied only with `From` equal to the attendee and DKIM or DMARC pass in the stored auth facts |
| Invitations sent twice (server scheduling and the client's own iMIP) | One `Scheduler` for web and DAV; submission drops a recent duplicate iMIP |
| Holtmail talking to itself | Instance token in `ID`, the JMAP session and the SMTP greeting; sources and transports refuse it |
| A stolen app password | Per device, scoped, protocol-limited, rate-limited, revocable with live sessions closed, audited; minting one needs a recent sign-in |
| Served tables are not derivable from blobs | They are in the database and in every backup; losing them costs one client resync, never mail |

## Open questions

1. IMAP4rev2: contribute `ESEARCH` to `imap-types`, or wait?
2. Outlook classic autodiscover (`/autodiscover/autodiscover.xml`): worth serving, or document manual setup?
3. Should a device with several archives get one IMAP namespace per archive (`Archives/<name>/...`) instead of one archive per device?
4. Local flags when a lock opens: offer "send them now" by default, or keep them local silently? (Phase 67 keeps them local and offers only Clear; "send them now" can come later.)
5. `SORT` and `THREAD` (RFC 5256): which clients need them enough to justify an index sort?
6. JMAP for calendars and contacts once a client exists: JSCalendar and JSContact through `calcard`, the same `item_changes` path as a DAV `PUT`.
7. Shared archives: should a served view of an archive another user owns show that owner's identities for sending, or only the viewer's own?
8. `APPEND` into an `Import` mailbox as a way to bring old mail into the archive with `imapsync` (an import account, no upstream): worth it next to the existing importers?
9. POP3 for the few clients that only speak it: never, or a read-only `RETR` over `INBOX`?

## Phases

The steps are in [WORKPLAN, Serving](WORKPLAN.md#serving-planned-2026-11-08).
