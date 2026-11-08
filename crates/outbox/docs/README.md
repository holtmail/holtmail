# holtmail-outbox

The durable queue between "send" and a submission server ([WRITE.md, Outbox](../../../docs/WRITE.md#outbox)). `Outbox::queue` checks a message against its identity and transport, stores the exact bytes as a blob and puts a row in the store; a worker hands due rows to their transport, one in flight per transport, and records what became of every recipient. Delivery is at most once: when the outcome is unknown the row becomes `uncertain` and only a person sends it again. A library over `holtmail-store`, `holtmail-blob` and `holtmail-submit`; the composition root supplies the two traits below.

## Public API

### Traits the caller implements

| Trait | Guarantees it must keep |
| --- | --- |
| `Archiver::archive(archive, raw, folder, flags) -> MessageId` | Archives the bytes like sync does, idempotent per bytes (the same bytes give the same message). With `folder`, also records a location there without a UID and with `flags`. `apps/server` implements it over `holtmail-ingest` (`Placement { folder, flags, .. }`) and commits the index |
| `Transports::submitter(&Transport) -> Arc<dyn Submitter>` | Builds the transport's submitter (opening its sealed secret with context `transport:<id>`). `SubmitError::Invalid` or `Auth` mean it cannot work as configured, and the message fails; any other error is tried again later, so a lock closed for a while (an instance switch, a capability) answers with a retryable error |
| `Transports::may_send(archive, user) -> bool` | Whether the user who queued a message still may send from the archive by their grant. Asked before every attempt; `false` fails the message, an error tries again later |

### `Outbox`

`Outbox::new(store, blobs, transports, archiver, OutboxConfig)`; `with_clock(f)` swaps the clock for tests.

`OutboxConfig` (defaults): `undo_window` 10 s, `retry_for` 24 h, `first_retry` 60 s doubling per attempt up to `max_retry` 1 h, `poll` 1 s.

| Method | Guarantees |
| --- | --- |
| `queue(Queue { archive, user, device, route, envelope, bytes, send_at, hold, flow_run, idempotency_key })` | `device` is kept on the row and on the sent copy's change, so every write a device makes names it. A message that repeats its `From` or `Sender` field is `Invalid`. Refuses with `Invalid` (the reason names the field, never a value) unless: the route's identity is active, in the archive, uses the route's transport, and `user` (when given) is among its users; the archive may use the transport and it is enabled; `Envelope::validate` passes; the bytes parse, have a `Message-ID`, have no `Bcc`, and the route's `From`, every `From` in the bytes and a non-empty return path are covered by the transport's `allowed_from`; size and recipient count are within its limits. Adds the identity's address to the envelope with Bcc-to-self. Stores the blob before the row. The row is `queued` (or `draft` with `hold`), due at `send_at` but never before the undo window ends. Idempotent per user and `idempotency_key`. Nothing is archived yet |
| `send_due()` | One attempt at the earliest due row of every transport, transports in parallel, never two rows of one transport at once (`claim_outbox`). A disabled transport or one at its hourly limit (`per_hour`, counted over attempts started in the last hour) is skipped and its rows wait. Returns how many were attempted |
| `run()` | `send_due` in a loop, woken by `queue` and the operations below or every `poll`; runs until the process ends |
| `recover_held()` | Releases into the undo window every held message whose item change was recorded (`ItemChangeStore::held_change_messages`), so a stop between recording a change and releasing its mail loses neither; audited as `outbox_changed` with `recovered`. A held message with no recorded change stays held. Call once at start |
| `recover()` | Moves rows left `sending` by a stopped process to `uncertain` and returns their ids. Call once at start, before `run` |
| `undo(id)` | `queued` → `draft`. A row already handed to the transport cannot be undone |
| `release(id, send_at)` | `draft` → `queued`, with a new undo window |
| `release_within(id, send_at, window)` | `release` with this undo window instead of the configured one; `Duration::ZERO` lets it leave at once |
| `cancel(id)` | `draft`, `queued` or `retrying` → `cancelled` |
| `retry(id, user)` | `retrying`, `failed` or `uncertain` → `queued` now, to every recipient that has not accepted; a new retry window starts. The identity checks of `queue` apply again for `user`, the one asking |
| `resend(id, user, recipients)` | After `sent`, `failed`, `uncertain`, `bounced` or `cancelled`: a new row with the same bytes (`resend_of` set) to `recipients`, which must be among the original envelope's, or by default to the refused ones (everyone when nobody accepted). The identity checks of `queue` apply again |

Operations from a state that does not allow them fail with `OutboxError::State(current)`; unknown ids with `NotFound`.

### Reports for mail clients

A message queued with a `device` was answered `250` long ago, so its user hears about a later failure this way. When an attempt ends with the row `failed`, or `sent` with a recipient refused, the outbox archives an RFC 3464 report (`multipart/report` with `message/delivery-status` and the original's headers, `Auto-Submitted: auto-replied`, `From: mailer-daemon@<the sender's domain>`) through `Archiver::archive` without a folder, and sets the row's `note`. It lists each recipient who did not get the message; a recipient refused at `RCPT` keeps the server's words. The `Message-ID` is `<outbox id>.<attempts>.delivery-report@<domain>`, so Phase 67 can place it as a local place in `INBOX`. An attempt that ended `uncertain` makes no report: nobody knows whether it left. The web app needs none, its Outbox shows the same. A failure to make the report is logged with ids and leaves the row as it is. The report is ingested as not live, so it never counts as a bounce of the row it describes.

### What sync learns after sending

`observe(Arrival { archive, message, raw, parsed, folder, labels, live }) -> Observed { linked, bounced, delayed }` reads a message ingest just added (`apps/server` calls it from a `holtmail-ingest` `Observer`). Idempotent; only ids and counts are logged.

- **Sent copies.** A message in a folder with the Sent role (or with Gmail's `Sent` label) whose `Message-ID` is that of rows of the same archive in `sent`, `bounced` or `uncertain`, and whose bytes are not the sent bytes, becomes the rows' `synced_copy`: two copies of one message. Identical bytes are one message already and never reach `observe`. Copies outside Sent are not linked, since anyone can reuse a `Message-ID`.
- **Rewritten `From`.** For each linked copy, a first `From` other than the identity's address flags the identity (`RewrittenFrom { seen, at }`, lowercased); the identity's own address clears the flag.
- **Bounces**, live arrivals only. A report read by `holtmail_mime::parse_dsn`, or else `parse_unstructured_bounce`, whose original `Message-ID` matches rows of the same archive in `sent`, `bounced` or `uncertain`: each `failed` recipient naming an envelope recipient (by `Final-Recipient` or `Original-Recipient`, case-insensitively) becomes `bounced { status, text }` (the diagnostic code); a failure naming nobody counts for the only accepted recipient, and with several the row becomes `bounced` with a note and no recipient changed. Recipients the report names that are not in the envelope are ignored. The row becomes `bounced` (an `uncertain` row too: the bounce proves it left), after an `outbox_changed` audit entry with targets `[id, "bounced"]` and the count of recipients. The same report again changes nothing.
- **Delays.** A `delayed` report naming envelope recipients only sets the row's `note`.

### One attempt

1. A transport that no longer exists, or missing bytes: `failed`.
2. The checks of `queue` run again, since rights may have changed while the message waited: the identity still exists, is active, uses the transport and the user is still among its users; the archive may still use the transport; `Transports::may_send` still allows the user; the bytes still pass `allowed_from` and the limits. One that no longer holds: `failed` with the reason.
3. The first attempt archives the bytes (`Archiver::archive` without a folder) and records the message on the row.
4. The submitter is built, one `mail_send` audit entry is stored (the user, archive, account, the outbox and message ids, and the count of recipients tried), and the bytes go to every recipient not yet accepted. Nothing is handed over when the entry cannot be stored.
5. Any error before the hand-over (blob store, store, archiving, audit) is retried like a temporary failure, so a row never stays `sending` and never blocks its transport. An error recording the result after the hand-over makes the row `uncertain`.
6. The result:
4. The result:
   - **Taken** (`Ok`): `sent` with each recipient `accepted` or `refused`, `sent_at` set. The message is located in the Sent folder of the identity's account (or the account transport's) with `\Seen` and `SENT_BY_HOLTMAIL` (`$HoltmailSent`) and no UID, until sync reads the server's copy and replaces the flags. No Sent folder known: no location.
   - **`Uncertain`** (connection lost after the final dot): `uncertain`, never retried on its own.
   - **Retryable** (`SubmitError::is_retryable`: temporary refusals, broken sessions, unreachable servers, recipient refusals with a temporary one): `retrying` with `next_attempt_at` after the backoff, as long as that is within `retry_for` of `send_at`; then `failed`.
   - **Anything else** (permanent refusal, authentication, too large, invalid): `failed`. Recipient refusals are recorded per address either way.

Errors stored on the row may hold server text that names addresses; they are shown to the user, never logged. Logs carry outbox, archive and transport ids, attempt numbers and counts only.

## Decisions and limits

- **Archived at the first attempt, not at queueing.** A message undone or cancelled inside the undo window never enters the archive, and a scheduled one enters it when it leaves; the blob is stored at queueing so the bytes are durable from the start. The archive holds the message before the transport sees it, as WRITE.md promises.
- **Restart:** every `sending` row becomes `uncertain`; a submitter cannot report afterwards that nothing was sent.
- **Sent copies upstream** (`SentCopyPolicy`): `provider` needs nothing (JMAP `onSuccessUpdateEmail`, Graph `saveToSentItems`, Gmail SMTP), `none` keeps the archive's copy only, `append:<folder>` records, right after the send, an `append` change of the sent blob with `\Seen` in `mail_changes` (origin `sent_copy`, with the outbox id, the archived message and the send time as the date, audited `mail_append`) for the identity's account, else the transport's account; `mail-writeback` sends and retries it like any held change, and `MailChangeStore::outbox_mail_changes` lists it for the outbox row. The folder is the account's folder with that path (exactly, then ignoring case), or its Sent folder for `append:Sent`. Without an account, without `mail.append` on it, or without the folder, nothing is recorded and the message gets a note saying why; the send itself never fails because of its copy.
- **Bounces** are trusted as far as a `Message-ID` and an envelope recipient go: a recipient who knows the `Message-ID` could forge a report about themselves. The report never changes another archive's rows.
- The outbox does not check capabilities or scopes: the API does that before calling it, and `Transports` refuses a closed switch or capability when it builds the submitter. Grants are asked again through `Transports::may_send` before every attempt, so a message queued by someone who lost the right, or was removed, fails instead of leaving.

## Tests

`src/tests.rs` runs on SQLite and the filesystem blob store in a temporary directory, with a scripted in-process `Submitter`, a fake `Archiver` and a settable clock: the undo window and exact bytes, undo, release, schedule and cancel, every refusal of `queue` and Bcc-to-self, idempotent queueing, backoff doubling and the 24-hour window, permanent and partial refusals with `resend`, `uncertain` and `retry`, restart recovery, one message in flight and the hourly limit, transports that cannot be built or no longer exist, a blob error before the hand-over that retries and frees the transport, rights (grant, `allowed_from`, identity users) taken away while a message waited, Sent copies linked only from Sent (or the `Sent` label) with the rewritten-`From` flag set and cleared, structured and unstructured bounces only from live arrivals with per-recipient status and an audit entry, and delay notes.
