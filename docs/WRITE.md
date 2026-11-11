# Plan: writing

Planned 2026-11-07. Phases 55–57 are built; the steps are WORKPLAN Phases 55–64.

Holtmail learns to write: send new mail, reply and forward, change flags and folders on the server, edit contacts and calendars, answer invitations, and run rules that do these things on their own. All of it is opt-in per account and per capability. An instance with nothing turned on behaves exactly as it does today.

This plan takes over the write phases reserved in [CLIENTS.md](CLIENTS.md) (Phases 53 and 54 and [Write later](CLIENTS.md#write-later)). The held-edit storage of CLIENTS Phase 51 stays the foundation for contacts and calendars.

## Principles

The principles in [PLAN.md](PLAN.md) change in two places and gain three rules:

- **Read-only by default, not read-only forever.** "Read-only towards the mail server" becomes "nothing is written to a server unless the owner turned that capability on for that account". The read path never writes, as today: sync keeps `EXAMINE` and `BODY.PEEK[]`.
- **"Read-only, honestly"** in the web app becomes: a compose button exists only in an archive where someone may send.
- **The archive never loses anything because of a write.** Moving or deleting on the server only records `removed_at` on a location, as sync does today. A sent message is in the archive before it leaves.
- **Raw bytes are the truth for outgoing mail too.** The exact bytes handed to a transport are stored as a blob and ingested like any other message. Bcc stays out of the bytes and lives in the outbox record.
- **Holtmail is not an MTA.** It never delivers to recipient MX hosts. It hands mail to a submission server (SMTP submission, JMAP, Graph), which signs, queues and delivers.

## Capabilities and who may use them

A write happens only when five locks are open:

| Lock | Where | Default |
| --- | --- | --- |
| Instance switch | `[write] enabled`, `[write] send`, `[write] rules` in config | `true`; `false` turns the capability off for everyone, whatever is stored |
| Account capability | Per account, set by an archive manager | All off |
| Archive grant | `reader`, plus new `editor` (flags, folders, contacts, calendars, answering invitations) and `sender` (send as granted identities); each includes the ones before it. Owners and admins manage archives and capabilities; only the owner gives `editor` and `sender` | Instance owner has all; existing grants are `reader`; admins write only with a grant |
| Token or device scope | `write:mail`, `send:mail`, `write:pim`, `rules` (device credentials and their scopes: [SERVE.md](SERVE.md#device-credentials); `rules` is never given to a device) | Not granted; existing tokens keep `read` |
| Identity grant | Who may send as which identity (sending only) | The identity's creator |

Account capabilities:

| Capability | What it allows |
| --- | --- |
| `mail.flags` | Seen, flagged, answered, keywords, Gmail labels |
| `mail.move` | Moving between folders |
| `mail.delete` | Moving to Trash; expunge only with `mail.expunge` |
| `mail.append` | Saving bytes into a folder: sent copies, drafts, restore to mailbox |
| `mail.send` | The account's own transport may be used by identities |
| `pim.edit` | Creating, changing and deleting contacts and events upstream (`write_through` in CLIENTS) |
| `pim.respond` | Answering invitations upstream |

Rules:

- `apps/server` hands out a writer or a transport only for an account whose capability is on. Library code cannot write without one, so a missing check fails closed.
- The API checks the grant and the scope again on every call.
- Every write is an audit log entry: who, which device or token, which account, what kind, ids and counts. Never content, subjects or addresses, also not in logs.
- Admins run the instance, they do not send as users: `admin` gets no `sender` grant by role, cannot take away an `editor` or `sender` grant by any route, and cannot change the email or password of someone who holds one (so they cannot sign in as a sender).

## Sending

### Identities, transports, routes

Three separate things, so one SMTP server can serve many addresses and one address can move to another server:

```
 Draft ──► Route (rules + defaults) ──► Identity ──► Transport ──► submission server
            picks identity, strategy,   From, name,  smtp | jmap |
            transport, extra headers    signature,   graph | account
                                        sent copy
```

**Transport** (`holtmail-submit` contract) is how bytes leave:

| Kind | Crate | Notes |
| --- | --- | --- |
| `smtp` | `submit-smtp` | Submission on 587 with STARTTLS or 465 with TLS; `AUTH PLAIN`, `LOGIN`, `XOAUTH2`; `SIZE`, `SMTPUTF8`, `8BITMIME`, per-recipient results |
| `jmap` | `submit-jmap` | `Email/import` + `EmailSubmission/set` with `onSuccessUpdateEmail` for the Sent copy |
| `graph` | `submit-graph` | `sendMail` with the MIME body; `saveToSentItems` |
| `account` | (one of the above) | Uses the account's own connection and credentials: the default for an account with `mail.send` |

- A transport belongs to an archive, or to the instance when an admin defines it (a company relay) and grants it to archives. Credentials are sealed with context `transport:<id>`.
- `allowed_from`: address patterns the transport may send as (`*@example.org`, `ada@example.com`). Holtmail refuses a send outside them, so a shared relay cannot be used to spoof another user's address.
- Limits per transport: maximum size (from `SIZE` when the server says it), recipients per message, messages per hour.
- OAuth: Google's `https://mail.google.com/` already covers SMTP. Microsoft needs `SMTP.Send` (IMAP accounts) or `Mail.Send` (Graph accounts). The first time an account transport of a Microsoft account without that consent is used (a check or a send), the account records the missing consent, shows `needs_sign_in` with a reason and fires the `account.needs_sign_in` webhook once; syncs go on and the message waits in the outbox as a temporary failure. Signing in again asks for the send scope while `mail.send` is on, and the account remembers that its token includes it (refreshes use the same scopes). Many Microsoft tenants disable SMTP AUTH, so a Graph account's own transport sends through Graph `sendMail`. An `account` transport of a password IMAP account submits to the IMAP host on 587 with `STARTTLS` and the account's own sign-in unless the transport names its own server and credentials. The account's own password or token only ever goes to the account's server: a host other than the account's needs the transport's own user name and password, and changing the host of a transport with a stored password asks for it again.
- **Library:** `mail-builder` (Stalwart, `Apache-2.0 OR MIT`, the family of `mail-parser`, which we already use) builds messages. `submit-smtp` is a small SMTP client of our own on `tokio-rustls`: `mail-send` 0.7 trusts only the operating system's roots (no `tls_ca_file`), stops at the first refused recipient and cannot tell a drop after the final dot from one before it, all of which the outbox needs.

**Identity** is what the recipient sees:

- address, display name, signature (HTML and text), `Reply-To`, the transport, the sent-copy policy (`provider` when the server keeps one itself, `append:<folder>`, `none`), Bcc-to-self;
- linked to an account or an alias, which the route uses to pick it;
- suggested automatically from account addresses, aliases in the alias store and the `own` address set, but inactive until a person confirms it and the transport check passes. Suggestions are computed when asked for, not stored; an identity becomes the creator's default when they have none.

**Route** decides, for one draft, the identity, the strategy, the transport and extra headers. Defaults without any rule:

1. A reply uses the identity of the address the original was sent to: an account address, an alias, a plus address of an identity (`ada+shop@example.com` replies from `ada+shop@`).
2. If the original came through an alias service, the alias strategy below applies.
3. Otherwise the identity of the account the original was synced from.
4. A new message uses the user's default identity.

Rules on `message.sending` (see [Rules](#rules)) override any of these. The compose window always shows the chosen route and why: "From ada@example.com through Fastmail SMTP, to SimpleLogin's reverse address for bob@example.org".

### Replies through relay mailboxes

`AliasHit` already records `alias`, `sender` and `reverse_alias`. The route turns it into one of three strategies:

| Strategy | What is sent | Services |
| --- | --- | --- |
| `reverse_alias` | `To:` the reverse alias, `From:` the real mailbox the alias forwards to, through that mailbox's transport. The service rewrites `From` to the alias | SimpleLogin, addy.io, DuckDuckGo, Apple Hide My Email (Apple's rewritten `From` is the per-sender reverse address; replies leave from the iCloud address the alias forwards to) |
| `direct` | `From:` the alias itself, from an identity of the user whose address is the alias or that is linked to it (the identity grant stays the lock; without one the route is `no_alias_identity`), through a transport whose `allowed_from` covers it | Fastmail Masked Email, own domains and catch-all rules (`alias-rules`), SimpleLogin custom domains with their own SMTP |
| `reply_header` | `To:` the `Reply-To` the service set, `From:` the real mailbox | Firefox Relay (premium) |

Each detector gets a default strategy; an archive manager (Settings → Aliases, `PUT /api/archives/{id}/alias-strategies`) or a flow can override it per alias, domain or service (the provider id, so it also covers aliases only seen in mail). The most specific override wins.

- **First mail to a new contact through an alias:** `AliasDirectory` gains an optional capability `reverse` and `reverse_alias(alias_id, contact) -> String` (SimpleLogin's contacts API). addy.io and DuckDuckGo encode the sender in the address, so `AliasDetector` gains `encode_reverse`, the mirror of the `decode_reverse` planned in [CONVERSATIONS.md](CONVERSATIONS.md).
- **The sending mailbox must be one the service accepts** (SimpleLogin accepts replies only from the alias's mailboxes, and only with passing DKIM). The identity for `reverse_alias` is therefore the alias's mailbox from the directory listing, not the account the mail was synced from.
- **Leak guard.** Before a send, the route checks that no recipient sees the real mailbox when the thread arrived through an alias: a reply-all that mixes reverse aliases with plain addresses, a forward from the real address, a `Cc` that is not a reverse alias. The send is blocked (`409` with the leaks) with a plain explanation and a one-click fix ("Send through SimpleLogin instead"), unless the user sends again with an explicit override, which is audited. A reverse alias made by the fix counts only when the service gives the same address again at sending.
- **Invitations** that arrived through an alias are answered with the alias as the attendee address in the iMIP REPLY, never the real mailbox, and the mail goes through the same strategy.
- The archived sent copy has `To:` the reverse alias; the reader and People show the real contact through the alias store, as they do for incoming mail. Every reverse alias Holtmail makes is recorded with its contact (`AliasStore::put_reverse_alias`, migration 0028) for this; a pair seen in incoming mail wins.
- **What counts as the sender** of the original, whose `To` entry the reverse alias or `Reply-To` replaces: its `From`, the hit's sender and reverse alias, and `Reply-To` only when the service signed the mail or it is the hit's reverse alias. Any other `Reply-To` (a list, a third party) stays a recipient and the leak guard checks it. Sending never adds a recipient: a sender the user removed from `To` stays removed.
- **Order of checks at sending:** the switches, the `sender` grant and the `send:mail` scope come before anything that can reach an alias service (making a reverse alias creates a contact at SimpleLogin).

### Composing (`holtmail-compose`)

A pure library that turns a draft into RFC 5322 bytes with `mail-builder`. No I/O; attachments come in as bytes or as blob ids the caller resolves.

- **Reply recipients:** `Reply-To` before `From`; `Mail-Followup-To` when present; "reply to list" through `List-Post`; reply-all drops every address in the `own` set and the identity itself.
- **Threading:** `In-Reply-To` and `References` (the original's `References` plus its `Message-ID`, shortened from the middle as RFC 5322 suggests); `Message-ID` `<uuidv7@identity-domain>`.
- **Subject:** one prefix, recognising `Re:`, `RE:`, `AW:`, `Ответ:`, `Fwd:`, `WG:` and bracketed counters, so prefixes never pile up.
- **Quoting:** text as `> `, HTML as `<blockquote type="cite">`; the reader's quote folding already knows both.
- **Forward** inline, or as an attached `message/rfc822` made from the archived raw bytes: forwarding works for mail long deleted from the server.
- **Body:** the editor's HTML goes through `holtmail-sanitize` with an outgoing profile, plus a generated text alternative. Remote images in quoted HTML stay as they were; saved copies are not inlined.
- **Automatic mail** (rules) carries `Auto-Submitted: auto-replied` or `auto-generated` (RFC 3834).
- **iMIP:** `imip` wraps an iTIP object in its mail (RFC 6047: the readable text, then `text/calendar; method=...` as the last alternative; `In-Reply-To` and `References` to the invitation for a `REPLY`). The `REPLY`, `REQUEST` and `CANCEL` objects themselves are built by `holtmail-pim-edit` over its own content lines rather than here over `calcard` (decision of Phase 62): they are edits of the stored copy, and that crate already keeps whatever it does not model untouched, while `holtmail-compose` stays a mail library.
- Fixtures: hand-written drafts and expected `.eml` files, compared after normalising `Date` and `Message-ID`.

S/MIME and OpenPGP signing are not in this plan (see [Open questions](#open-questions)).

### Outbox

A durable queue in the store. Sending is never fire-and-forget from an HTTP handler.

```
 draft ──► queued ──(undo window, send_at)──► sending ──► sent ──► (bounced)
   ▲          │                                  │
   └─ undo ───┘                                  ├──► retrying ──► sending
                                                 ├──► failed      (permanent refusal)
                                                 └──► uncertain   (connection lost after the final dot)
```

- **Record:** archive, identity, transport, route explanation, envelope (`MAIL FROM`, every `RCPT`), the blob of the exact bytes, `Message-ID`, `send_at`, state, per-recipient results, attempts, the user and device, the flow run if a rule queued it, the `Idempotency-Key`.
- **Undo:** a configurable window (default 10 s) during which the message waits in `queued`. Scheduled send uses the same `send_at`.
- **Delivery is at most once.** SMTP cannot be made idempotent. A temporary failure before the final dot retries with backoff (up to 24 h). A connection lost after the final dot and before the reply becomes `uncertain`: the user decides whether to send again, the server never guesses.
- **Partial refusal:** recipients refused at `RCPT` are reported per address; the message goes to the rest, and the user can resend to the refused ones.
- **One message in flight per transport**, enforced by the store's atomic claim, with the transport's hourly limit counted over attempts started in the last hour.
- **Restart:** `sending` rows found at start become `uncertain`; no transport can report afterwards that nothing was sent.
- **Rights are checked again at every attempt:** the identity, its users, the archive's use of the transport, `allowed_from` and the sender's grant. A message whose sender lost the right while it waited fails with the reason. An error before anything reached the transport (blob store, store, archiving) retries instead of leaving the row `sending`; the `mail_send` audit entry is written before each hand-over, and nothing is handed over without it. A closed instance switch or account capability retries (up to the 24 hours) rather than failing at once; a token endpoint that is down retries, a revoked sign-in fails.

**After a send:**

1. The bytes are stored as a blob when the message is queued and archived when it is first handed to the transport, so an undone or cancelled message never enters the archive and every message that leaves is archived before it leaves. Once the transport takes it, it gets a location without a UID in the account's Sent folder with the keyword `$HoltmailSent` ("sent by Holtmail"), until sync reads the server's copy and replaces the flags.
2. The sent copy goes upstream by the identity's policy: nothing when the provider keeps one (Gmail SMTP, Graph `saveToSentItems`, JMAP `onSuccessUpdateEmail`), otherwise `append` to Sent (needs `mail.append`).
3. When sync later sees the server's Sent copy, identical bytes are one message already (content addressing). Rewritten bytes (Microsoft, some relays) are linked by `Message-ID` to the outbox row (`synced_copy`) and collapse as two copies of one message, by the rules in [CONVERSATIONS.md §7.4](CONVERSATIONS.md). Only a copy in a Sent folder (or with Gmail's `Sent` label) is linked, since anyone can reuse a `Message-ID`. A linked copy whose `From` is not the identity's address flags the identity with a plain explanation (Gmail without "Send mail as").
4. **Bounces:** an incoming DSN (RFC 3464, or an unstructured bounce from `MAILER-DAEMON` that quotes the original) whose original `Message-ID` matches an outbox row of the same archive marks it `bounced`, per recipient with the status code and diagnostic, audited. Only live arrivals count (a folder that finished a sync before; an SMTP delivery), never a backfill or an import. A delay report only sets a note.

**Drafts** live in Holtmail's store, autosaved with versions. Saving them to the server's Drafts folder (`append` and replace) is a later option per identity. Sending a draft removes it; undoing a queued message cancels it and opens a new draft rebuilt from its bytes and envelope. Drafts never leave Holtmail and are not audited. The signature is inserted by the compose window, not added on sending.

### Submission server

The user's own mail clients can send through Holtmail too: SMTP submission and JMAP `EmailSubmission` go into the same outbox through the same identities, routes and locks, and leave through a configured transport. Design in [SERVE.md](SERVE.md#smtp-submission).

## Mailbox changes

`MailboxWriter` is a second trait in `holtmail-source`, separate from `Source`, the same split as `CollectionWriter` in `holtmail-collection`. The composition root hands one out only for accounts with the capability; `Source` stays read-only and unchanged.

```rust
#[async_trait]
pub trait MailboxWriter: Send + Sync {
    fn capabilities(&self) -> MailboxWriteCaps;
    async fn set_flags(&self, folder: &str, uids: &[u64], add: &[Flag], remove: &[Flag],
                       unchanged_since: Option<u64>) -> Result<FlagResult, SourceError>;
    async fn set_labels(&self, uids: &[u64], add: &[String], remove: &[String]) -> Result<(), SourceError>;
    async fn move_to(&self, folder: &str, uids: &[u64], target: &str) -> Result<Moved, SourceError>;
    async fn append(&self, folder: &str, raw: &[u8], flags: &[Flag], date: Timestamp)
                    -> Result<Option<u64>, SourceError>;
    async fn delete(&self, folder: &str, uids: &[u64], expunge: bool) -> Result<(), SourceError>;
}
```

| Implementation | How |
| --- | --- |
| `source-imap` | Its own connection with `SELECT`; `UID STORE` with `UNCHANGEDSINCE` (CONDSTORE), `UID MOVE` or copy, flag and `UID EXPUNGE` (UIDPLUS), `APPEND` returning `APPENDUID`; Gmail `X-GM-LABELS` |
| `source-jmap` | `Email/set` with `ifInState`, `mailboxIds` and `keywords` patches, `Email/import` |
| `source-graph` | `PATCH` of `isRead` and `flag`, `move`, MIME upload for append |

- **Optimistic, held, then sent:** a change becomes a row in `mail_changes` (the same shape as `item_changes`), the web shows it at once, and `mail-writeback` sends it first in first out per account, one per message in flight, like `pim-writeback`.
- **Sync confirms:** when the next flag sync shows the new state, the row is `applied`. If the server changed the same flag meanwhile, the server wins and the row says so. Flags are not worth a conflict dialog; moves and deletes that fail are shown.
- **Restore to mailbox** (milestone 5) is `append` of the archived bytes, and **deleting from the server after a period** (PLAN, Keep and let go) is a retention job over `delete`. Both come with this writer.
- Read state: the reader can mark as read on open only with `mail.flags`, and only when the user turns that on; the default stays "reading in Holtmail changes nothing".
- **Built contracts (Phase 60 foundation):** `MailboxWriteCaps` says what a writer can do (system flags, keywords, `unchanged_since`, labels, reported UIDs after move and append, marking `\Deleted` without expunging); what it cannot do fails with `SourceError::Unsupported` before anything changes, and an unknown folder with `SourceError::Missing`. `mail_changes` keeps `held → sending → applied | superseded_by_server | failed`; a flags or labels change the server accepted stays `sending` with `sent_at` until sync confirms it (`confirm_mail_flags` with the time the flags were read, so a read from before the send proves nothing), and `settle_mail_changes` applies those no sync confirmed after a while. Moves, deletes and appends are applied once the server accepts them. Details in the [source](../crates/source/docs/README.md#mailboxwriter) and [store](../crates/store/docs/README.md#mailchangestore) docs.
- **Built wiring (Phase 60):** the server hands `mail-writeback` a writer only while `[write] enabled` and the account's capability for the change are on; a move to the trash counts as `mail.delete` when the account lacks `mail.move`, and "delete" in the API is a move to the account's trash (marking `\Deleted` only where there is none) unless `expunge` asks for `mail.expunge`. Sync confirms open flags and labels changes by reading those messages' flags once more after its own flag read. Changes are recorded per place (folder and UID) of a message, all accounts of a request are checked before anything is recorded, and each request writes one audit entry per account and kind. Retention is the account setting `delete_after_days` (owners and admins set it; a period needs `may_write(mail.delete)`), applied by an hourly job of at most 1 000 changes per account, with a dry-run count in the API. Restore appends the archived bytes with `\Seen` to a folder of an account in the same archive. The sent copy of `append:<folder>` is recorded by the outbox right after the send and listed on the outbox row (`sent_copy`).
- **Built web (Phase 60):** the reader shows read, star, move, trash and restore only when `AccountDto.writable` (every lock of `may_write` for the current user) has the capability on an account the message is on (restore: any account of the archive with `mail.append`); toggles are sent per account so one account's closed lock never blocks another's. The held mark lists `MessageDetail.pending` in words, and `MessageDetail.failed` (failed changes nobody dismissed) offers Retry (`POST /api/mail-changes/{id}/retry` records the same change again and dismisses the old one) and Dismiss (`POST /api/mail-changes/{id}/dismiss`, a `dismissed_at` on the row, migration 0031). A flags change counts the place's open changes, so undoing a held change is recorded instead of skipped as `already`. List rows show changes asked for in this tab through a 15-minute overlay until the server shows them. Mark as read when opened is a per-browser choice, off by default, effective only with `mail.flags`. Retention is set in the account's Writing dialog with the dry-run count and a confirmation.
- **Review fixes (Phase 60):** a newer held change is never closed by sync ahead of an older open change of the same place (`confirm_mail_flags` stops at the first change that stays open), so undoing a held change cannot be lost. A change made at a place an earlier applied move left follows the message to the move's target and reported UID (`applied_move_from`, `retarget_mail_change`; `moved` when no UID was reported), so "archive, then star" works before the next sync. A rejected sign-in or connection failure pauses the whole account in the worker (one attempt per backoff step), never one sign-in per held change. Retention never records a place again once a retention change was recorded there, whatever became of it. Labels that act as folders (Gmail `Inbox`, `Sent`, `Drafts`, `Trash`, `Spam`, any `\Name`) are refused for a labels change, which only needs `mail.flags`. Without `MOVE`, the IMAP writer copies every message it moves (also one already marked `\Deleted`) and checks the expunge rule first, so a failure can duplicate a message but never lose one. `MailboxWriter` promises less than first written: a call that fails with `Unsupported` or `Missing` changed nothing, other failures may leave it partly applied, and every method but `append` is idempotent. Microsoft Graph asks for `Mail.ReadWrite` (`Provider::write_scope`) on sign-in while a mailbox capability is on; until the account has it, nothing is offered and held changes wait. Writers are rebuilt when the account's settings or stored sign-in change.

## Contacts and calendars

The design in [CLIENTS.md, Edits from native apps](CLIENTS.md#edits-from-native-apps) stays: `item_changes`, `pim-edit`, `write_mode`, `CollectionWriter`, `pim-writeback`, conflicts that never drop an edit, and the four reply routes. This plan adds:

- **Editing in the web app.** Forms for events (title, time with time zone, recurrence, location, invitees, alarms) and contacts (names, addresses, phones, photo), writing the same held changes with origin `api`. Raw iCalendar or vCard editing for experts.
- **RSVP buttons** on invitation cards and in the calendar. The reply route is chosen as in CLIENTS; the iMIP route goes through the outbox with the identity the route picks, so an invitation received through an alias is answered through the alias.
- **Organizing:** a new event with invitees sends iMIP REQUEST through the outbox, and changes and cancellations send REQUEST and CANCEL, unless the collection's server schedules itself (RFC 6638 CalDAV, Graph; JMAP does not count, see the Phase 62 decision below).
- **Creating contacts** in a chosen address book, from the People page or from a sender in the reader.

The storage half of CLIENTS Phase 51 (migration, `record_change`, `pim-edit`, the pim-sync post-batch step) is a prerequisite. If native clients have not reached it, Phase 61 builds it first without DAV serving.

- **Built storage and contract (Phase 61 foundation):** migration 0032 adds `write_mode` (accounts' collections `held`, the rest `read_only`; only `read_only` for a collection no account owns), `items.dav_name`, `item_versions.origin` and `change_id`, the append-only `item_changes` table and the `changes` log. `ItemChangeStore` records changes as a compare-and-set against `ServedState { version, change }` (the state a served ETag is computed from, so the store never needs the served bytes), supersedes the open head (one being sent stays sending until it settles), serves `effective` (latest upstream plus the open head), lists `open_changes` and `changes_of`, resolves (`Rebased`, `Applied`, `Conflict`, `Discard`, `Retry`) and feeds the writer (`due_change_collections`, `claim_change`, `settle_change`, `release_changes`). Write modes, roles and scopes are checked by callers, not the store. A delete of a create that never left is `applied` at once. `changes_since` is the first slice of the CLIENTS Phase 45 `changes` log, written by item changes only. Details in the [store docs](../crates/store/docs/README.md#itemchangestore).
- **Built writer contract (Phase 61 foundation):** `CollectionWriter` in `holtmail-collection` with `WriteCaps`, `Precondition::{None, Absent, IfMatch, IfUnchanged}` (the last for JMAP, which has no tags), `Partstat`, `Written`, and `CollectionError::{PreconditionFailed { current }, Refused}`. `ItemPatch` carries the whole item as iCalendar or vCard plus the `Field`s that differ from the base: DAV writers send the bytes, Graph and JMAP writers read only those fields out of them, so `pim-edit` never learns provider formats. `FakeCollections` is a writer and its own probe; `writer_conformance` is the suite. Details in the [collection docs](../crates/collection/docs/README.md#writing-collectionwriter).

- **Built writers (Phase 61):** `collection-dav` (`PUT` with `If-Match`/`If-None-Match: *` to a name made from the idempotency key; `respond` only where the server advertises `calendar-auto-schedule`), `collection-jmap` (`ContactCard/set` and `CalendarEvent/set` patches of only the changed fields, `ifInState` after a compare for `IfUnchanged`, creates found again by `UID`) and `collection-graph` (`PATCH` of the listed fields under `If-Match: W/"changeKey"`, `transactionId` for events, an extended property for contacts; a field Graph cannot hold is `Refused` before anything is sent). `pim-writeback` drains `item_changes` in the style of `mail-writeback`. Details in each crate's README.
- **Built wiring (Phase 61):** pim-sync runs the post-batch step after each collection (`holtmail_pim_sync::Edits::reconcile`: applied on convergence, silent rebase, `conflict` with `overlap`, `reply_obsolete`, `upstream_deleted`, `upstream_changed` or `upstream_unreadable`), and derived data follows what is served (`Ingest::derive_served`). What is served is `holtmail_pim_edit::serve` over the latest upstream version as text (`holtmail_pim::as_text`, so Graph and JMAP items are edited as iCalendar and vCard). A rebased change is written as its rebase on the version it was rebased onto (`Rebaser::rebased`), so a replace-only writer never overwrites upstream's own later edits. The server hands `pim-writeback` an upstream only for `write_through` collections of server accounts with `pim.edit` while `[write] enabled` is on, built from the protocol the collection's kind is read with (so `collection-merge` routes no writes); Microsoft Graph asks for `Contacts.ReadWrite` and `Calendars.ReadWrite` on sign-in while `pim.edit` is on (`missing_consent = "pim.edit"` until then). The API records edits with origin `api` from the event and contact forms or raw text, gated by `may_write(pim.edit)` (also in `held` mode), lists and resolves open changes (`keep_mine`, `take_theirs`, `discard`, `retry`), sets write modes (owners and admins; `write_through` with open changes needs `confirm`), carries the Local edit mark and the timeline on items, makes contacts from a sender, and routes People edits to the primary card (the person's card in the editable address book stored first). Collections of import accounts are `read_only` (migration 0033), as CLIENTS.md says for imported files. Details in the [API](../crates/api/docs/README.md#contacts-and-calendars-edits) and [server](../apps/server/docs/README.md#writing-contacts-and-calendars) docs.
- **Built web (Phase 61):** edit actions show only where `CollectionDto.editable` is true (an account's collection, not `read_only`, not removed, every lock for `pim.edit` open), so a member without an editor grant or a collection in `read_only` never sees a form. The event form covers title, start and end with a time zone, all day, a repeat preset or a custom `RRULE`, location, description, invitees and reminders; the contact form names, emails, phones and postal addresses with labels, organization, job title and notes. Photos are not in the form (the API has no photo field) and stay as they are, as do other fields the form does not show; the raw tab edits the whole item as text for experts. Saving an unchanged form sends the served times as they were, so it records `noop`. New contacts come from People, a person's _Edit card_ (`PUT /api/people/{id}/card`) and a sender in the reader (`from_message`). The Local edit mark reads the four states of CLIENTS.md; the item timeline interleaves changes with versions, and every change opens a field diff against its base and the latest upstream version (`GET /api/pim/changes/{id}/diff`, `holtmail_pim_edit::field_diff`). Settings → Contacts and calendars sets write modes (owners and admins; turning on sending always asks first, naming the open edits that go with it) and lists open changes by collection with Keep mine, Take theirs, Discard and Retry; the navigation counts the changes that need a decision (conflicts and refusals). Invitations are not sent from the form until Phase 62. Details in the [web docs](../apps/web/docs/README.md#edits).
- **Built after review (Phase 61):** a writeback version no longer flags the user's own later changes of the item (those that superseded the change in flight): the store rebases them onto the written version (`rebased_onto`), so an edit followed at once by a delete sends the delete under the new tag instead of turning it into a false conflict. Settling or resolving a change as a conflict, or settling it as rejected, clears its flags, and the post-batch step decides only held changes; conflicts and refusals wait for a person. A settle the store fails is retried as applied without a version (for an applied write) or held with `store`, so no change stays `sending`. After a writeback, derived data is rebuilt from what is served (the open head re-applied with `Rebaser::rebased`). A `reply` change (only the user's own answer changed) goes upstream only while the account also has `pim.respond` (`Writers::may_reply`; it waits with `respond_off`), because a `PUT` of the attendee copy makes an auto-scheduling server tell the organizer. Writers: `WriteCaps::keeps_uid` says whether a create keeps the patch's `UID` (false for Graph, which assigns `iCalUId`), and `writer_conformance` checks it where promised; Graph lists `canEdit` and offers nothing for a read-only or unlisted calendar once it has listed, and a write `403` is `Refused` (the server checks the sign-in's write consent before writing), so a read-only shared calendar no longer pauses the account; a DAV `PUT` answered with a weak `W/` tag no longer vouches for the bytes. JMAP takes a `UID` match for the earlier create only when the found item holds the patch; any other is `PreconditionFailed`, a conflict `exists`. The API answers `422 no-uid-conflict` for a create whose `UID` a live item of the collection has. Google sign-ins ask for `auth/calendar` instead of `auth/calendar.readonly` while `pim.edit` is on, and a Google DAV account signed in without it waits with `missing_consent = "pim.edit"`. `AddressPersonDto.has_card` lets the reader offer _Add sender to contacts_ for correspondents known from mail alone; `WriteModeDto.allowed_modes` limits the mode picker to what the server accepts.
- **Decision (Phase 61): held creates are not served yet.** CLIENTS.md asks that a held create show in lists, the calendar and search with a mark. A create has no item until it reaches the account (the item and its upstream id come from the writeback), so in `held` mode it shows only under Settings → Contacts and calendars, and the create dialog says so. Serving it needs a local item keyed by the href's `dav_name` that writeback later fills in; that comes with DAV serving (Phase 73, [SERVE.md](SERVE.md#caldav-and-carddav)). As built in Phase 73, no local item row was needed: the store already serves an href with an open create (`ChangeTarget::Href`) and `open_changes` lists it, so DAV clients see every held create, the web app's included, at once under the href it was recorded with, and writeback keeps that href as the item's `dav_name`. The web app's own lists, calendar and search still show a held create only under Settings → Contacts and calendars until writeback makes its item.
- **Built foundation (Phase 62):** `holtmail-pim-edit` builds the iTIP objects: `reply` (the user's own attendee line only, any spelling of the address, so an invitation that came through an alias is answered as the alias from `holtmail_route::attendee_address`; the series, a changed instance, or an instance made from the series; the invitation's `SEQUENCE` unchanged and a fresh `DTSTAMP`), `request`, `cancel` (series, one instance, or removed attendees only) and `organize`, which turns an organizer's change into the copy to record (`SEQUENCE` raised for start, end, duration, recurrence, `EXDATE` and status changes, answers reset when the time moves) and the messages to send. `holtmail-compose::imip` makes the mail around them. The same crate holds the two pure choices: `reply_route(host, caps, agent)` (`Server` where the host schedules and `respond` is allowed, `Quiet` for `SCHEDULE-AGENT=NONE`, else an iMIP `REPLY` through the outbox, with how the account's copy follows: `respond` without notifying, a `PUT` where that cannot make the server answer too, or held) and `organizer_route(host, caps, change)` (`Server`, `Deferred` or `Imip`). Details in the [pim-edit docs](../crates/pim-edit/docs/README.md#imip-objects-reply-request-cancel-organize).
- **Decision (Phase 62): no invitations from a held change on a scheduling server.** When the collection's server schedules (RFC 6638 CalDAV, Graph) but the change cannot be written now (`held` mode, writes off), the server sends once the change reaches the account (`OrganizerRoute::Deferred`): mailing a `REQUEST` now would make attendees get it twice once the server sends its own. Where the server schedules, also while the change waits, Holtmail mails attendees whose `SCHEDULE-AGENT` is `CLIENT` at once, since the server never tells them, and never those with `NONE`. A server that cannot be asked now is neither: a change that would mail anyone is refused (`503`) unless it says `invitations: false`.
- **Decision (Phase 62, after review): JMAP calendars count as not scheduling.** `sendSchedulingMessages` is a request a server may ignore and JMAP has no capability that says whether it honours it, so asking would lose invitations silently on a server that does not. Holtmail mails attendees through the outbox, and event writes leave the flag off so nobody hears twice. `CollectionWriter::respond` still passes it for answers routed through the server; the server route is simply not chosen for JMAP.
- **Decision (Phase 62, after review): an answer is one person's, from a known address.** An RSVP goes out only as an invited address the user has an identity for (or the account's own on the server route); an alias `ATTENDEE` never leaves with the real mailbox in `From` (`409`). An incoming `REPLY` changes the organizer's copy only when it names exactly one attendee who is the mail's `From` or the signed real sender behind an alias, and only for an instance the series has; `From` can be forged, so this limits a forger to the attendee they spoof.
- **Decision (Phase 62): every `CANCEL` raises `SEQUENCE`,** including the one to removed attendees, so no client ignores it as older than what it holds, and the stored copy is raised with it, so a later re-invite is not older than that `CANCEL`; a title or location change sends a `REQUEST` with the same `SEQUENCE` and a newer `DTSTAMP`, which RFC 5546 section 2.1.5 orders correctly.

- **Built wiring (Phase 62):** the RSVP API (`POST /api/items/{id}/rsvp`, `POST /api/messages/{id}/rsvp`) answers the copy in an account's calendar when one is writable, else the event from mail, gated by `may_write(pim.respond)` (on the copy's account, or the account the invitation mail came from) and, for an iMIP reply, the sending locks. The route is `reply_route` over the server's `Mailboxes::scheduling` (the host and writer capabilities of the collection, whatever the locks; the capabilities count only while the collection writes through and `pim.respond` is on). `Server`: a `reply` change carrying `Respond { notify: true }` that `pim-writeback` sends with `CollectionWriter::respond` (Graph `accept`/`decline` with `sendResponse`, JMAP `participationStatus` with `sendSchedulingMessages`, CalDAV's attendee-copy `PUT`). `Imip`: a `REPLY` through the outbox with the identity `holtmail-route` picks for a reply to the invitation mail (so the alias strategy applies and the alias is the attendee), waiting out the undo window, and the copy as a `reply` change with `notify: false`. The organizer path runs on every Phase 61 create, edit and delete of an event organized by an own address: `organize`'s copy is what is recorded, and its messages go through the outbox from the organizer's identity unless the server schedules (only `SCHEDULE-AGENT=CLIENT` attendees then) or would schedule once the change is written (`Deferred`: nothing); `invitations: false` records the change without mail, and an event form with invitees gets the user's identity as `ORGANIZER`. A live iMIP `REPLY` to an own event in a `write_through` calendar whose server does not schedule becomes a held change of that copy with the attendee's `PARTSTAT` (server `replies` observer); elsewhere the answer is derived data: a version of the event from mail, listed among the copies' answers. `pim.respond` alone now gets an account a writer for replies (`Writers::may_edit` holds its other edits with `edit_off`). Details in the [API](../crates/api/docs/README.md#answering-invitations), [pim-writeback](../crates/pim-writeback/docs/README.md) and [server](../apps/server/docs/README.md#writing-contacts-and-calendars) docs.
- **Decision (Phase 62): "From mail" answers are the archived reply, not a held change.** The latest version of an event from mail is whichever mail part came last (often someone's `REPLY` or a `CANCEL`), so a change on it could not be served or rebased, and with no writer it would stay open forever. The iMIP `REPLY` Holtmail sends is archived when it leaves and becomes a version of that event, so the answer shows from raw bytes (`InvitationDto.my_status`), and an undone or failed reply never claims an answer the organizer did not get. A "From mail" invitation whose organizer asked for no answers (`SCHEDULE-AGENT=NONE`) cannot be answered (`422`).
- **Decision (Phase 62): an organizer without the right to send cannot save invitees silently.** When the event's changes would be mailed and the user has no identity for the organizer, or no sender grant, the edit is refused (`403`) unless it says `invitations: false`; messages are held in the outbox until the change is recorded and cancelled when it is not, so neither happens without the other: they are linked to the change in its own transaction, the request survives a client going away, and a start after a crash releases what a recorded change left held (`Outbox::recover_held`).
- **Built web (Phase 62):** answer buttons on the invitation card and the event page, shown only while the server says the user may answer. To say before answering how an answer would leave, and to offer one occurrence, the API gained two read-only previews that WRITE did not list: `GET .../rsvp` (route, copy, the address, the alias and service, occurrences, the Local edit mark) and `GET /api/collections/{id}/organizing` (who would send a calendar's invitations). Details in the [web docs](../apps/web/docs/README.md#invitations) and [API docs](../crates/api/docs/README.md#answering-invitations).
- **Decision (Phase 62): _Undo_ of an answer is cancelling its outbox message and discarding its recorded change,** so the copy goes back to what it was, not just the mail. Discarding a `reply` change therefore needs `pim.respond` and not `pim.edit`, and an emailed answer's change waits for the undo window (`NewItemChange.not_before`) so it is still there to discard. An answer on top of an edit that has not reached the account is refused, because answering upstream would drop that edit. Cancelling rather than the compose undo: an answer has no draft to return to. An answer that goes through the account's server has no undo window and no undo.
- **Built (Phase 74): `calendar-auto-schedule` and `Schedule-Tag`** for devices ([SERVE.md](SERVE.md#calendar-auto-schedule)). The RSVP and organizer routes now deliver through the same `holtmail-schedule` `Scheduler` as CalDAV and SMTP submission: their messages stay held in the outbox until the change is recorded (Phase 62's rule, now through `Submission::hold`, `Scheduler::confirm` and `withdraw`, also for devices) and are audited when they are released; an answer that does not change the user's `PARTSTAT` sends nothing; an answer to an invitation that came through an alias service goes to its reverse alias; an incoming `REPLY` needs DKIM or DMARC pass for its sender in an `Authentication-Results` field from a trusted authserv-id, with "Apply anyway" in the web for editors while writing is on.

## Rules

### Why not Sieve and sieve-rs

- **License.** `sieve-rs` is `AGPL-3.0-only`. Linking it into the `holtmail` binary would make the binary AGPL, against the `MIT OR Apache-2.0` promise in PLAN.md.
- **Fit.** Sieve filters incoming mail at the moment the delivering server stores it. Holtmail is not that server: mail is already delivered when sync sees it, so `reject`, `discard` and implicit keep mean nothing here. Sieve has no outgoing routing, no calendars, no schedules and no branching beyond `if`/`elsif`.

What we take from Sieve: tests plus actions, `stop`, vacation with `:days` per sender, and the loop rules of RFC 3834 and RFC 5230. Holtmail writes no Sieve: rules stay Holtmail flows. Importing a Sieve subset is a later option.

### Flows

A flow is a trigger plus a graph of nodes, stored as versioned JSON per archive.

```
 [message.archived] ─► [query: from:*@shop.example.com category:purchases] ─yes─► [label: Orders]
                                     │                                              │
                                     no                                             ▼
                                     ▼                                    [forward to: accounting@example.org
                              [has invitation] ─yes─► [RSVP: tentative,         via identity: work]
                                                       needs confirmation]
```

**Triggers:** `message.archived` (live mail only), `message.sending` (routing), `invitation.received`, `event.upcoming`, `schedule` (cron), `manual` (run on a selection or a search).

**Conditions:**

- `query`: the search language users already know (`from:`, `has:attachment`, `category:`, `alias:`, ...), matched against one message. `holtmail-query` gains a `Matcher` that evaluates a parsed query on a message's stored facts, without the index, with a test that both agree on a corpus.
- header test, alias hit and its service, trust, insight kind, sender is a person in People, time window, counter ("more than 3 from this sender today").

**Actions,** each declaring the capability it needs: label or tag (local, free), set flags, move, delete (upstream), reply from a template, forward, send a new message, RSVP, create event or contact, notify a device, call a webhook (through the existing webhook sender), stop. On `message.sending`: set identity, set transport, set alias strategy, add Bcc, require confirmation, block with a reason.

**Templates:** `minijinja` (Apache-2.0) in a sandbox with a whitelisted context (sender name, subject, dates, insight fields), size and time limits, no includes, no network.

**Node catalogue as code.** Each node type in `holtmail-rules` has an id, a version, an input schema (JSON Schema via `schemars`), output ports, the capability it needs, and documentation in English and Russian. The editor, the API validation and the docs page are generated from it, so documentation always matches what the engine accepts.

### Engine

- `holtmail-rules` is pure: `evaluate(flow, event, facts) -> Plan`, a list of intended actions with the path taken. No I/O. The server executes a plan through the outbox and the writers, so every safety check sits in one place.
- **Dry run** is `evaluate` without execution: "test with this message" shows the path and the actions, and a new flow starts in test mode (log only) until it is turned on.
- **Live mail only.** Flows never run on backfills, imports, resyncs or rebuilds; they read the ingest `live` flag (CLIENTS Phase 45, built here first if needed).
- **At most once per event:** a run is keyed by flow version and event id and claimed before it acts, so a restart or a retry never repeats it; a run the process stopped in the middle shows as `interrupted` in the runs log and its remaining actions are not carried out (sends cannot be exactly once either, see the outbox).
- **Order:** flows of an archive run in a set order; `stop` ends the chain. Nothing is ever discarded from the archive.
- **Runs log** (like n8n executions): each run keeps the nodes visited, the decisions and each action's result, for 30 days, in the store. Server logs carry only ids.

**Safety:**

- Auto-replies follow RFC 3834: never to mail with `Auto-Submitted` other than `no`, with `List-Id` or `Precedence: bulk|list|junk`, from `noreply`-like or `mailer-daemon` addresses, or from own addresses; at most once per sender per N days.
- Rate limit per flow and per identity; a circuit breaker pauses a flow after too many actions in an hour and notifies the owner.
- Per-action "needs confirmation": the action lands in the outbox as a draft or in a pending list instead of running.
- A flow's actions are limited by the grants of the user who saved it, checked again at run time.

- **Built (Phase 63):** `holtmail-query`'s `Matcher` and `holtmail-rules` (details in their docs); `RuleStore` with migration 0043 (flows with every version, runs unique on flow, version and event id, carried-out actions, waiting events); the executor `holtmail_api::Rules` with the REST routes (`/api/rules/catalogue`, `/api/archives/{id}/flows`, `/api/flows/{id}` with `versions`, `test`, `run`, `runs`, and `/api/flow-runs/{id}` with confirm and dismiss of held actions) and `holtmail rules list|test|export|import`. Details in the [API](../crates/api/docs/README.md#rules), [store](../crates/store/docs/README.md#rulestore) and [server](../apps/server/docs/README.md#rules) docs.
- **Decision (Phase 63): the executor lives in `holtmail-api`.** Rule actions are the API's own paths (drafts and `send_stored_draft` over the `Submission`, `change_messages`, the RSVP path, item creates), so it reuses them rather than a new crate duplicating routing and the leak guard; `apps/server` only wires the ingest observer, the loop and the webhook hooks.
- **Decision (Phase 63): events wait in the store.** Ingest's observer only keeps an event (`message.archived:<message>`, `invitation.received:<message>`); the executor's loop runs flows after the message is stored, so a crash between arrival and run loses nothing, and the run key stops repeats. Schedules and upcoming events are kept by a tick that resumes at most an hour back after a stop.
- **Decision (Phase 63): who a flow acts as.** The user who saved its current version, with the full web scopes (there are no tokens yet), while they pass `may_use_rules` (switches, an `editor` grant); saving a flow that sends needs a `sender` grant. A confirmed held action runs as the person confirming. `rules` stays a scope no device gets.
- **Decision (Phase 63): limits.** 60 actions per flow and 30 sends per identity an hour (`rate_limited`), the circuit breaker at more than 100 acting runs of a flow in an hour; it pauses the flow, tells the `flow.paused` webhook and writes an audit entry (there are no device notifications yet, so `notify` is the `flow.notified` webhook plus the runs log). Constants in `RuleLimits`, not config.
- **Decision (Phase 63): held actions.** A sending action with `confirm` leaves its draft (shown in the outbox's drafts); any other waits as `pending` in its run until confirmed or dismissed. A rule send the leak guard stops also stays a draft.
- **Decision (Phase 63): `message.sending` covers the web's send only.** It runs in `POST /api/drafts/{id}/send`; SMTP submission and JMAP clients are routed without it. `set_transport` picks an identity of the same address that uses that transport, since the outbox binds an identity to one transport.
- **Decision (Phase 63): labels are local labels** (`LabelStore`), so a Gmail label sync may replace one; a separate tag store is left for later.
- **Decision (Phase 63 review): at most once, and stops survive retries.** A flow that had a run for an event under any version gets none again; when a retried event meets an earlier run, its `stop` still ends the chain, and so does a live run that never recorded its plan. A run in test mode is only logged: it never ends the chain and never counts as acting. `acted` means a run carried out an action, which is what the breaker and the `flow` counter count.
- **Decision (Phase 63 review): `message.sending` flows are per user.** Only the sending user's own flows apply to their send, rechecked at each send (`may_use_rules`, and a `sender` grant for routing actions); a blind copy, identity or transport a rule sets is a `mail_send` audit entry. Someone other than the saver needs a `sender` grant to turn on or run by hand a flow that sends, since it sends as the saver; the saver's own `sender` grant is rechecked at every run.
- **Decision (Phase 63 review): replies and forwards.** An automatic reply goes only to the sender the RFC 3834 checks ran on (the real sender behind an alias, else `From`), never `Reply-To`; the per-identity limit applies to the identity the route picked, so `auto` counts. A forward of a message with `Auto-Submitted` other than `no` is skipped, so two archives forwarding to each other stop at the first hop. Only actions carried out count for the limits and the once-per-sender rule; a reply the leak guard kept as a draft does not.
- **Decision (Phase 63 review): held actions are claimed.** Confirming moves the result from `pending` to `confirming` with a compare-and-set on that one result (`RuleStore::set_flow_run_result`), then carries it out with the key `rule-confirm:<run>:<index>` and records it; of two confirmations one wins, and decisions on other actions of the run never overwrite each other.
- **Decision (Phase 63 review): webhooks rules may call** are those whose managers subscribed them to `flow.run` (now in `holtmail_hooks::EVENTS`), checked when a flow is saved and at each call.
- **Decision (Phase 63 review): facts are complete for live mail.** Ingest tells observers about a message after its categories and index document (with the attachment text) are written, so `category:` and free-text conditions see them; templates get the insight values of the first order, invoice, parcel, reservation and pass as `insights.<kind>_<field>` (never codes or links) and an upcoming event's location and organizer. `manual` runs on a selection (up to 100) or the newest 100 messages of a search.

### Editor

- **Canvas** with React Flow (`@xyflow/react`, MIT), restyled to [design.md](design.md): square nodes, hairline edges, no radius, tokens only.
- Node palette with search on the left; an inspector generated from the node's JSON Schema on the right; a docs panel per node.
- The query field reuses the search box with its operator suggestions; the template field has a preview against a chosen message.
- **Test with a message:** runs a dry run and highlights the path on the canvas.
- Tabs: Runs (the log), Versions (with a diff), Import and export (JSON).
- **List mode** for linear flows ("When … if … then …"), which is what most people need and the only mode on phones. A flow that branches opens on the canvas.
- CLI: `holtmail rules list|test|export|import`.
- **Built (Phase 64, first item):** Settings → Rules and the editor at `/rules/$flowId` (details in the [web app docs](../apps/web/docs/README.md#rules-editor)); the template preview is `POST /api/archives/{id}/flows/preview` ([API](../crates/api/docs/README.md#rules)).
- **Decision (Phase 64): the template preview is a server route.** The sandbox and its whitelisted context live in `holtmail-rules`, so the preview renders there with the facts a run of the flow's trigger would load (`TemplateContext::new`), never a second template engine in the browser; the upcoming event of `event.upcoming` is not filled in a preview.
- **Decision (Phase 64): the query field counts with the index.** It shows how many archived messages the search box finds for the query; the rule matcher and the index agree by the Phase 63 agreement test, so the count stands for what the condition would match.
- **Built (Phase 64, rest):** Test with a message (path drawn on the canvas and marked in the list), Runs with confirm and dismiss of held actions, Versions with a diff, Import and export of the flow JSON, List mode, and turning a flow on from test mode in the editor ([web app docs](../apps/web/docs/README.md#rules-editor)). No server change was needed: the routes of Phase 63 serve it.
- **Decision (Phase 64): what "linear" means.** From the trigger each condition continues only through `yes` with `no` unlinked (no means the flow ends, which the list says), each action through `next`, a step without ports (`stop`) only last, every step reached exactly once and every type known. Anything else branches and opens on the canvas; on phones it can be tested, its runs and versions read and its JSON exported, but not changed. List edits lay the steps out in one row so the canvas shows the same order.
- **Decision (Phase 64): tests and imports are of the editor's flow.** Test with a message sends the unsaved flow (`POST /api/flows/{id}/test` with `flow`), Export downloads it and Import and _Open in the editor_ of a version replace it unsaved; only Save stores a version, so import, restore and the server's validation are one path. The export is the same JSON as `holtmail rules export`.
- **Decision (Phase 64): version diffs leave positions out** and compare the flow JSON with sorted keys line by line, since moving a box changes nothing the engine reads.
- **Decision (Phase 64): turning on applies to the saved version,** so _Turn on_ waits for a save; a paused flow gets _Run again_ (its own mode again clears the pause).
- **Decision (Phase 64 review): the editor follows the grant and the version.** Steps that send are disabled in the palette without a `sender` grant; a run's path is drawn only while the editor holds that run's version unchanged; a `manual` flow gets _Run by hand_ (a search or one message) above its runs.
- **Decision (Phase 64): the editor is a full-screen page** outside the settings layout, so the palette, canvas and inspector have room; Settings → Rules is the list. Field labels and choice names are in the web app's strings (English and Russian); the schema's descriptions stay English and show in English only.

## API, CLI, MCP

- REST through `api-client`: `identities`, `transports` (with `check`), `drafts`, `outbox` (send, undo, retry, resend), `mail/changes`, `pim/changes`, `flows` (with `test`, `runs`, `versions`), `rules/catalogue`.
- Every mutation accepts an `Idempotency-Key`, kept 24 h in a table. A request that never finishes releases its key, and after a crash an unfinished claim is taken over after 10 minutes. A `4xx` answer is kept too, so a client that changes its request after one uses a new key.
- CLI: `holtmail send` (from an `.eml` or flags, for scripts), `holtmail outbox`, `holtmail rules`.
- MCP: a `draft_reply` tool creates drafts only. Sending from MCP needs `send:mail` on the token and the owner's explicit setting; by default a person confirms in the web app.

## Web app

- **Compose** as a pane over the reader (full screen on phones): From picker with the route explained, recipient fields with People suggestions, the leak guard, rich text with TipTap (MIT) or plain text, attachments from disk or from the archive, signature, schedule, undo toast.
- **Reader actions:** Reply, Reply all, Reply to list, Forward, Forward as attachment, Edit as new; flags, move and delete only when the capability is on, with the held mark ("not yet on the server").
- **Outbox** under each archive: queued, scheduled, failed, uncertain, bounced, with Retry, Resend and Cancel.
- **Settings → Sending:** identities, transports (with a check button); alias strategies on **Settings → Aliases**, next to the aliases they apply to; **Settings → Rules**: flows and the editor; per account **Writing** with the capability switches and their consequences in plain words.

Built in Phases 55 and 57 (the leak guard, alias strategies, flags, move and delete come with their phases):

- **Gating** comes from the server: `Archive.rights { role, manage, compose }`. `compose` is true only when the server sends mail, the `[write]` switches allow sending, the grant is `sender`, the credentials carry `send:mail` and the user may send as an active identity of the archive. Without it the reader keeps its `mailto:` Reply (the user's own mail app, nothing through Holtmail) and there is no compose button, reply action or outbox link.
- **Reply to list** shows only when `MessageDetail.list_post` names a list address.
- **The editor** is TipTap 3.32 (`@tiptap/react`, `@tiptap/starter-kit`, ProseMirror; all MIT) in a lazy chunk. A template's quoted or forwarded original stays outside the editor, shown in a sandboxed frame that loads nothing remote, and goes out unchanged after the edited part, so ProseMirror's schema never rewrites the original's HTML. In rich mode only the HTML is sent and the server makes the text alternative.
- **Drafts** autosave about a second after the last change with their version; a `409` from another tab takes the server's version and saves again, so the open window wins. Closing deletes a draft the window started (new, reply, forward) when the user never touched it, and an empty new draft; replacing the open draft with another, switching archives or leaving the page saves pending edits first. Attachments come from disk or, through a search of the archive, from any archived message. Saved drafts are listed at the top of the outbox.
- **Send** carries one `Idempotency-Key` per send attempt of a compose window: repeated after a network error or a `5xx`, renewed after a definite `4xx` so the corrected draft can be sent. The undo toast counts down to the message's `send_at` and calls undo inside it; a wait of more than a minute is shown as a scheduled send.
- **Strings** of the writing screens are in English and Russian (`strings.writing`), picked by `<html lang>`; the rest of the app is English until it gets a language switch.

Built in Phases 58 and 59:

- **Transports** of every kind have one form; a JMAP or Graph transport, or an account transport of a JMAP or Graph account, shows only what that kind takes. A new JMAP or Graph address asks for the secret again in the form, as the server does.
- **Sent copy:** `append` is offered once the account the copy would go to (the identity's, else its account transport's) has `mail.append` in effect, and says why not otherwise.
- **Alias strategies** by service and by domain sit on Settings → Aliases with a plain explanation of each; the per-alias choice is on each alias row.
- **Route line** for a reply through an alias service names the path: "To SimpleLogin's reverse address for bob@example.org, from your mailbox ada@example.com" (`RouteDto.provider` and `contact`).
- **Leak guard:** each leak offers its one-click fixes (the reverse alias, sending as the alias's identity when one exists, removing the recipient); sending with leaks left goes through an explicit confirmation that names the real address.
## Modules

| Module | Kind | Job |
| --- | --- | --- |
| `holtmail-submit` | Contract | Handing prebuilt bytes and an envelope to a submission server; per-recipient results; conformance suite |
| `submit-smtp`, `submit-jmap`, `submit-graph` | Implementations | As in the transport table |
| `holtmail-compose` | Library | Drafts, replies, forwards, iMIP to bytes |
| `holtmail-route` | Library | Identity, strategy and transport for a draft; the leak guard |
| `holtmail-outbox` | Library over store and submit | The queue, retries, undo, sent copies, bounce matching |
| `holtmail-rules` | Library | Flows, the node catalogue, `evaluate`, templates |
| `mail-writeback` | Library over store and `MailboxWriter` | Draining `mail_changes` |

`crates/smtp` is the one SMTP server, for inbound mail and for submission from mail clients ([SERVE.md](SERVE.md#smtp-submission)); tests also use it as the fake submission server.

## Tests

- Conformance suites for `holtmail-submit`, `MailboxWriter` and `CollectionWriter`, each run by every implementation against in-process fakes (`holtmail-smtp` as the SMTP target, the existing IMAP and JMAP fakes made writable).
- `holtmail-compose` and `holtmail-route` with hand-written fixtures; property tests for subject prefixes and `References` shortening; `holtmail-rules` with golden plans per flow.
- Integration (`mise run it`): Stalwart for SMTP submission, IMAP writes, JMAP submission and CalDAV scheduling (an organizer's event seen by a second Stalwart user, `invitations` suite); GreenMail and Postfix as submission sinks; the SimpleLogin stack (`sl-app`, `sl-mx`) for a real reverse-alias reply that arrives with the alias as `From`; Radicale for write-through.
- VERIFY: Gmail SMTP with XOAUTH2 and a "Send mail as" alias, Microsoft `SMTP.Send` and Graph `sendMail`, real SimpleLogin, addy.io and Apple Hide My Email replies, Apple Calendar and Outlook receiving our iMIP.

## Hard parts and their answers

| Problem | Answer |
| --- | --- |
| A send cannot be made exactly once | At most once; `uncertain` is a state the user resolves |
| Gmail replaces `From` with the account address when the alias is not set up as "Send mail as" | The transport check sends nothing but warns; after the first send, sync reads the Sent copy and flags a rewritten `From` on the identity |
| Microsoft tenants disable SMTP AUTH | Graph `sendMail` is the default for Microsoft accounts |
| The provider's Sent copy duplicates ours | Content addressing, then the `Message-ID` link to the outbox row |
| An alias reply leaks the real address | Strategies per service, the identity from the alias's mailbox, the leak guard |
| A rule mails thousands of people or loops | Live mail only, RFC 3834 rules, rate limits, the circuit breaker, test mode first |
| Local state and the server disagree | Held changes with marks; the server wins for flags; conflicts for contacts and calendars as in CLIENTS |
| An XSS sends mail as the user | `guard_writes`, scopes, `sender` grants, the undo window and the audit log |

## Open questions

1. S/MIME and OpenPGP (and Autocrypt headers): which first, and where keys live (`holtmail-secret` or the client).
2. Drafts in the server's Drafts folder: worth the replace-on-every-save traffic?
3. Shared mailboxes in companies: "send on behalf of" (`Sender:` header) as an identity option?
4. Should flows be shareable between archives (an instance-wide flow library defined by an admin)?
5. Sieve import: which subset (`fileinto`, `addflag`, `vacation`) is worth a parser?
