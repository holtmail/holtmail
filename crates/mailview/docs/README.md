# holtmail-mailview

The contract for the served mail view: what IMAP, JMAP and the web app show of one archive to one person, and the one way they change it. A `MailView` computes the mailbox tree, assigns served UIDs and `MODSEQ`, merges flags across places, hands out raw bytes and their MIME structure, searches and reports changes. A `MailActions` turns a client's flag change, move, copy, delete or append into a held mail change, a local flag, a local place or a draft, behind the five write locks, and says per message whether it was held, applied or refused. Neither ever deletes a message from the archive. The design is in [SERVE.md, The mail view](../../../docs/SERVE.md#the-mail-view); the archive-backed implementation is `mailview-archive`.

## Types

- `Viewer { user, device, archive, scopes }`: who looks at which archive. `Viewer::web(user, archive)` carries every scope. Reading needs `read`; upstream changes need `write:mail` too.
- `MailboxId`: a mailbox keyed by what it shows, never by name, so an account rename keeps UIDs (a folder renamed upstream arrives from sync as a new folder, so a new mailbox). `key()`/`parse()` round-trip: `role:<role>` (unified role mailbox, several accounts), `folder:<id>`, `all` (All Mail), `kept` (Only in Holtmail), `drafts`, `node:<path>` (a placeholder level that holds no mail). The key is also the subscription key and the JMAP mailbox id.
- `Mailbox { id, path, role, selectable, subscribed, rights }`: `path` is the levels from the top; no level contains `/` (an upstream `/` shows as `∕`), the inbox is `["INBOX"]`. All Mail carries `FolderRole::All`.
- `MailboxRights { read, add, remove, set_seen, set_keywords, local_flags }`: a right is given when at least one place behind the mailbox allows it, or local flags apply for `set_seen`/`set_keywords`; a single message may still be refused. `remove` also governs `\Deleted` in `PERMANENTFLAGS`.
- `MailboxStatus { messages, unseen, uidvalidity, uidnext, highest_modseq }`, `Member { uid, email, modseq }`, `MemberFlags`, `Snapshot { mailbox, status, members }`, `MailboxChanges { status, changed, vanished }`.
- `EmailRef`: `Message(MessageId)` (`M<id>`) or `Draft { draft, version }` (`D<id>.<version>`); `key()`/`parse()` round-trip.
- `Envelope`: stored fields plus `blob` (BLAKE3 of the raw bytes), `size`, `crlf_size` (the IMAP form with bare LF made CRLF), `in_reply_to`, `sender`, `reply_to`, `received_at` (falls back to archive or save time), `preview`, `has_attachment` and the selectable `mailboxes` the email is in for this viewer.
- `Part`: one MIME part with IMAP section `id` (empty for the whole message; a `message/rfc822` part has one child, the embedded message, with the same id), lowercase `content_type`, `params`, `disposition`, `disposition_params`, `content_id`, `description`, `transfer_encoding` (`7bit` when missing), `language`, `location`, and byte ranges into the raw bytes: `header` through its empty line, `body` without the line break before the next boundary delimiter. Protocols cut `BODY[n.MIME]`, `BODY[n]` and JMAP part blobs from these ranges.
- `Filter` and `TextField`: `All`, `And`, `Or`, `Not`, `Text(field, words)`, `Flag`, `ReceivedBefore`/`Since`, `SentBefore`/`Since` (whole UTC days, as IMAP), `Larger`, `Smaller`, `Uids`, `ModseqSince`, `HasAttachment`.
- `ViewState { email, mailbox, thread }` and `ViewChanges { upto, more, emails, mailboxes, counts_changed, threads }` with `Changed { created, updated, destroyed }`.
- Actions: `FlagChange { add, remove }`, `NewEmail { raw, flags, received_at }`, `Outcome::{Held, Applied, Refused(Refusal)}`, `Refusal::{Locked(Lock), Cannot, Modified, NotFound}`, `Lock::{Switch, Capability, Grant, Scope, Consent}` (the order `holtmail-access` checks them), `FlagResult { email, outcome, flags, modseq }`, `PlaceResult { email, outcome, uid }`, `Appended { outcome, email, uid }`.

## MailView

Every method answers `NotFound("archive")` for a viewer who may not read the archive or lacks `read`. Mailbox reads catch the served UIDs up with the change log first.

| Method | Guarantees |
| --- | --- |
| `tree(viewer)` | The layout below, parents before children, paths unique ignoring ASCII case; never reads a blob |
| `status(viewer, mailbox)` | Counts and UID state without members |
| `select(viewer, mailbox)` | Members ascending by UID. New members get UIDs in archive order; a UID is never reused, also when an email leaves and comes back (it gets a new one); `uidvalidity` lasts as long as the served tables. `NotFound("mailbox")` for unknown mailboxes and placeholders |
| `mailbox_changes(viewer, mailbox, since)` | Members with `MODSEQ` above `since` with flags, and UIDs that left after `since`, both ascending. A vanished UID may be one the viewer never saw |
| `flags(viewer, mailbox, emails)` | Synced flags of the places behind the mailbox (every place with `None`), open mail changes on top, joined, then the viewer's local flags. Unknown emails left out; deduplicated ignoring case, sorted. Drafts show `\Draft \Seen` |
| `envelopes(viewer, emails)` | In the order asked, unknown left out |
| `raw(viewer, email)` | The archived bytes unchanged; their hash is `Envelope::blob` |
| `structure(viewer, email)` | Deterministic for the same bytes |
| `blob(viewer, blob)` | Only blobs that are the raw form of an email the viewer can read; `NotFound("blob")` otherwise |
| `search(viewer, mailbox, filter)` | Members ascending by UID; text matches words and word beginnings ignoring case (stems may match too), never a piece from inside a word (a documented IMAP deviation). Never cut short: `TooMany { limit }` when a complete answer cannot be given |
| `thread(viewer, thread)` | Emails oldest first; `NotFound("thread")` when none |
| `state(viewer)` | Per-type highest change `seq`, ignoring other users' own rows; never goes down |
| `changes(viewer, since, limit)` | At most `limit` change rows after `since`; an id is in at most one list; one that appeared and went away inside the window is never created and may be destroyed (RFC 8620 allows both); mailboxes include every layout entry a changed folder is part of (role mailboxes, tree levels); other users' own rows skipped; `CannotCalculateChanges` for a state the log no longer covers (or one from the future) |

### Layout

- **One account:** the account's folders at the top with their roles; the inbox-role folder is `INBOX`. The drafts-role folder is not listed: it is part of `Drafts`. Then `Drafts`, `All Mail`, `Only in Holtmail`. A top-level folder named like one of these gets ` (<account>)` appended.
- **Several accounts:** `INBOX`, and `Sent`, `Junk`, `Trash`, `Archive` where some account has that role, as unified role mailboxes (`role:*`); `Drafts`, `All Mail`, `Only in Holtmail`; then `Accounts/<account>/<path>` for every folder, without roles.
- Levels with no folder are placeholders (`node:*`, not selectable, no rights). Folders with the `All` role (Gmail's All Mail) are not served.
- **Default subscriptions:** role mailboxes, `Drafts` and folders without a role are subscribed; `All Mail`, `Only in Holtmail`, placeholders and role folders under `Accounts/` are not. With one account every folder is subscribed. Choices are per user, not per device.
- **Members:** a folder shows messages with a live place there (held moves, deletes and appends applied); a role mailbox every live place with that role in any account plus the viewer's local places of that role (with one account, local places show in the folder with that role); `All Mail` every archived message once; `Only in Holtmail` messages with no live place; `Drafts` the viewer's drafts (newest version of each) and the drafts-role folders' messages. Views are shared by everyone who reads the archive; local places and drafts are shown to their user only.

## MailActions

Every method checks the five locks per place before anything is written, answers one result per distinct email in the order asked, and answers `NotFound("archive")`/`NotFound("mailbox")` like `MailView`. A held change shows at once; every change raises the `MODSEQ` of what it touched. A refused action writes nothing for that email.

| Method | Guarantees |
| --- | --- |
| `set_flags(viewer, mailbox, emails, change, unchanged_since)` | Open places get a held flags change; closed places get the viewer's local flag when local flags are on and `\Deleted` is not touched; a message with no place gets local flags only. `Held` when any mail change, `Applied` when only local flags, else refused; the result carries the flags shown afterwards. `\Deleted` needs `mail.delete` and has no local form (`Cannot`). `unchanged_since` below the email's `MODSEQ` is `Modified` and writes nothing. Setting what is shown is `Applied` and writes nothing. Draft flags are fixed (`Applied`, unchanged). A local flag raises the `MODSEQ` for every reader, but only its user sees it and its change row |
| `move_to(viewer, from, emails, to)` | Same account: a held move (into the trash, `mail.delete` instead of `mail.move`). Another account: an append there (`mail.append`) and a delete here. Into a role mailbox: that role's folder in each place's account, `Cannot` where there is none. `Cannot` into `All Mail`, `Only in Holtmail`, `Drafts` or a placeholder, and out of `All Mail`, `Only in Holtmail` or `Drafts`. The result carries the UID in `to`. A held move keeps its target UID when sync confirms it; a move upstream refuses puts the email back with a new UID |
| `copy_to(viewer, from, emails, to)` | A held append of the archived bytes in the target folder's account (`mail.append`); a role mailbox picks the first account by name that takes it. Already there: `Applied` with the UID |
| `remove(viewer, mailbox, emails)` | In a mailbox: each place gets a held delete, to the account's trash (`mail.delete`) or, in the trash or without one, expunged (`mail.expunge`); a local place is dropped and a draft deleted (`Applied`). With `None`: every place expunged, the email shows in `Only in Holtmail`. `Cannot` in `All Mail`, `Only in Holtmail` and for an email with no place left |
| `append(viewer, mailbox, email)` | Drafts: a new draft (`Applied`, a new UID per save, never archived). A folder or role mailbox (first account by name that takes it): archived, then a held append; the same bytes are the same email and UID. Sent where no account takes it: the viewer's local place (`Applied`), because clients retry a refused Sent copy forever, but only when the viewer's own locks are open (switch, grant, `write:mail`) and every Sent folder is closed by its account alone; otherwise `Locked` with the viewer's lock, and nothing is archived. `Cannot` into `All Mail`, `Only in Holtmail` or a placeholder. A refused append archives nothing. `TooLarge` above `append_limit` |
| `append_limit(viewer)` | The `APPENDLIMIT` |
| `subscribe(viewer, mailbox, subscribed)` | Whether it differs from what the user had, default included |

Errors are `MailViewError::{NotFound(what), Building { done, total }, CannotCalculateChanges, TooLarge { limit }, TooMany { limit }, Invalid(reason), Unavailable(text)}`. `Building` is IMAP `NO [UNAVAILABLE]` while a view's UIDs are first built; `TooMany` is IMAP `NO [LIMIT]` for a search whose answer would be incomplete. Reasons never carry addresses or content.

## Writing an implementation

Enable the `testing` feature under `[dev-dependencies]` and run the suite with a `Harness`:

```rust
holtmail_mailview::testing::conformance(&harness).await;
```

`Harness::start(&Scenario, Setup)` builds a fresh archive and returns a `World`:

- `Scenario::two_accounts()` and `Scenario::one_account()` describe accounts (name, capabilities as `Write`, folders with roles) and invented messages on `example.com`/`example.org` with their places, synced flags and whether each place is still live. The harness archives the messages in order.
- `Setup { local_flags }` is `[serve] local_flags`.
- The `World` holds the view and actions over one archive, a `Driver` (`settle(applied)` settles every open mail change as the server taking or refusing it; `deliver(account, folder, raw)` archives a new arrival), viewers (`editor`: owner on a `read`+`write:mail` device; `reader`: the same person with `read` only; `member`: another person with a reader grant; `stranger`: no grant), and folder and message ids by name.

`testing::FakeMailView::world(&scenario, setup)` keeps the whole contract in memory for tests of protocol front ends (`FakeHarness` runs it through the suite). Its append limit is `FAKE_APPEND_LIMIT` (10 MiB); it never prunes its change log and never reports `Building`.
