# holtmail-schedule

The contract for calendar scheduling ([SERVE.md, calendar-auto-schedule](../../../docs/SERVE.md#calendar-auto-schedule)). A `Scheduler` makes what a calendar client writes fit to record (client scheduling parameters stripped, an attendee's write reduced to what RFC 6638 section 3.2.2.1 allows), delivers the iTIP messages a change calls for (organizer `REQUEST` and `CANCEL`, attendee `REPLY`) with RFC 6638 `SCHEDULE-STATUS` codes, decides what an incoming iTIP message in mail does, and answers whether a message was just sent. It never writes to the store: it returns the bytes to record and the decisions, and the caller records them in its own transaction. It sends only through `holtmail_submission::Submission`, so every message is held to the sending locks and the per-user limits. CalDAV (`serve-dav`), the web app's RSVP and organizer routes, the ingest hook and `serve-submit` all use the same scheduler, so nothing is answered or sent twice.

## Types

```rust
pub enum Origin { Device, Web, Itip }
pub enum Host { CalDav { auto_schedule: bool }, Graph, Jmap { scheduling: bool }, Mail }
pub struct HostCaps { pub create: bool, pub update: bool, pub delete: bool, pub respond: bool }
pub enum Role { Organizer, Attendee, None }
pub struct ClientPut { pub previous: Option<Bytes>, pub body: Bytes, pub own: Vec<String>,
    pub keep_answers: bool }
pub struct Prepared { pub body: Bytes, pub role: Role, pub stripped: bool, pub reduced: bool }
pub struct ReplyAs { pub attendee: String, pub route: holtmail_route::Route, pub to: Option<String> }
pub struct Change { pub actor: Actor, pub archive: ArchiveId, pub origin: Origin, pub host: Host,
    pub caps: HostCaps, pub own: Vec<String>, pub previous: Option<Bytes>, pub next: Option<Bytes>,
    pub reply_as: Option<ReplyAs>, pub references: Vec<String>, pub comment: Option<String> }
pub enum ScheduleStatus { Pending, Sent, Delivered, NoRoute, Failed } // 1.0 1.1 1.2 3.7 5.1
pub enum Way { Nothing, Server, Deferred, Mail, Quiet }
pub struct Delivery { pub body: Option<Bytes>, pub rewritten: bool, pub role: Role, pub way: Way,
    pub statuses: Vec<RecipientStatus>, pub queued: Vec<OutboxId>, pub suppressed: usize }
pub struct ItipKey { pub uid: String, pub sequence: u32, pub method: ItipMethod,
    pub recurrence_id: Option<String>, pub partstat: Option<String> }
pub struct AuthFacts { pub dkim_pass: Vec<String>, pub dmarc_pass: Vec<String> }
pub struct IncomingItip { pub calendar: Bytes, pub from: Vec<String>, pub signed_sender: Option<String>,
    pub recipients: Vec<String>, pub own: Vec<String>, pub arrival: Arrival, pub auth: AuthFacts,
    pub upstream_holds_uid: bool, pub organizer_copy: Option<Bytes>, pub force: bool }
pub enum Incoming { Ignored(Ignored), Invitation { method, uid, from_mail: FromMail }, Reply { uid, outcome: ReplyOutcome } }
```

- `Origin::Device` is a CalDAV client: `SCHEDULE-AGENT=CLIENT` on an attendee means that client tells the attendee itself. `Origin::Web` is Holtmail's own client: `CLIENT` there means Holtmail mails the attendee because a scheduling upstream will not (the Phase 62 rule). `Origin::Itip` is the scheduler's own change (a merged `REPLY`) and never sends.
- `Host` and `HostCaps` describe the upstream calendar and what the account may write there now (all false while the change stays held), the same inputs as `pim_edit::organizer_route` and `reply_route`.
- `own` is the user's `calendar-user-address-set` in any spelling; addresses are compared normalized.
- `Actor` is `holtmail_submission::Actor`: messages are submitted as the person and device that made the change.
- `ReplyAs::to` is where an answer goes instead of the `ORGANIZER`: the reverse alias or the alias service's `Reply-To` that relay routing chose (`Routed::to`), so an answer to an invitation that came through SimpleLogin or addy goes back through the service and never straight to the organizer's real address.
- `ClientPut::keep_answers` is set by a CalDAV write with a matching `If-Schedule-Tag-Match`: the client's copy may predate answers merged since (the tag stays pinned while the ETag moves on), so an organizer's write takes every other attendee's `PARTSTAT` from `previous`, as RFC 6638 section 3.2.10 asks. A deliberate change of another attendee's answer by the organizer's client is not told apart; RFC 5546 gives the organizer no reason to make one, and a time change still resets answers in `deliver`.
- `AuthFacts` come only from the topmost `Authentication-Results` whose authserv-id the operator trusts for the account the mail came through; `dmarc_pass` lists the `header.from` domains DMARC passed for.
- `RECENT` (10 minutes) is the window of `just_sent` and of dropping a repeated outgoing message. `Clock` is the time source (`system_clock()` in production).

## Scheduler

| Method | Guarantees |
| --- | --- |
| `prepare(put)` | Client-sent `SCHEDULE-STATUS` and `SCHEDULE-FORCE-SEND` removed from every `ATTENDEE` and `ORGANIZER`; `SCHEDULE-AGENT` stays. With `keep_answers`, an organizer's write takes each other attendee's `PARTSTAT` from the matching event (same `RECURRENCE-ID`, else the series) of `previous`. An attendee's write over an existing copy keeps only the user's own `PARTSTAT` (series and instances; an instance or an `EXDATE` the attendee added is a decline of that instance), alarms, `TRANSP`, `PERCENT-COMPLETE`, `COMPLETED` and client properties; the rest stays as the copy has it and `reduced` is set. Nothing to change returns the bytes as given. Unreadable bytes are `Invalid`. Deterministic |
| `deliver(change)` | Called before the caller records `Delivery::body`. Every message is held: none leaves before `confirm`. The caller records `Delivery::queued` with the change (`NewItemChange::messages`), so after a stop the outbox releases the held messages of recorded changes and keeps the others held. Organizer: create `REQUEST`, delete `CANCEL`, update as RFC 5546 asks; answers, alarms and client properties alone send nothing. Scheduling upstream that may write the change: `Server`, `1.1`; that cannot yet: `Deferred`, `1.0`; otherwise `Mail`. `SCHEDULE-AGENT=NONE` attendees are never told; `CLIENT` ones only for `Origin::Web` on a scheduling upstream. Attendee: a changed own `PARTSTAT` or deleting the event (a decline) answers through the upstream (`Server`, the caller writes the `reply` change with `respond` and `notify`), not at all for `SCHEDULE-AGENT=NONE` on the organizer (`Quiet`), else as an iMIP `REPLY` as `reply_as` (to `reply_as.to` when set) or the invited own address (`Mail`). No identity for the address: `3.7`; a submission refused or rate-limited when it is made: `5.1` (a message that fails or bounces later is reported in the outbox, not written back as `5.1`). A message identical to one delivered within `RECENT` and not undone since is not queued again (`1.1`, counted in `suppressed`); one the outbox holds as cancelled is queued anew. `SCHEDULE-STATUS` written on the attendees the change concerns (on `ORGANIZER` for an answer), others keep the previous copy's; `rewritten` when the bytes differ from `next` (no ETag then). `Unavailable` leaves nothing queued |
| `confirm(actor, delivery)` | Lets the held messages of a recorded change go: each leaves once its undo window has passed from now. Twice is harmless |
| `withdraw(actor, delivery)` | Undoes the delivery's held messages when the change could not be recorded; messages that already left are left alone; twice is harmless; only a message actually taken back stops counting as just sent |
| `incoming(mail)` | Never sends. Ignored, in order: not live, Sent folder, outbox copy, `From` an own address, not a readable `REQUEST`/`REPLY`/`CANCEL`, not addressed to an own address. `REQUEST`/`CANCEL`: "From mail" `Update`, unless the organizer is own (`OwnOrganizer`) or an upstream calendar holds the `UID` (`UpstreamHolds`). `REPLY`: `Merged { body }` only when it names exactly one attendee who is `From` (or the signed sender behind an alias), the mail has exactly one `From` address and DKIM passed for the attendee's domain or a parent of it, or DMARC passed for exactly that domain (or the alias service signed it, or `force` for "Apply anyway"), and `organizer_copy` is the user's own event with that `UID`; else `Unapplied(NotFromAttendee | Unauthenticated | NoCopy | Unchanged)`. A merge keeps the `Schedule-Tag` |
| `just_sent(archive, key, recipients)` | Whether this scheduler delivered a message with that key (`UID`, `SEQUENCE`, method, `RECURRENCE-ID`, and for a `REPLY` the answer) to every one of `recipients` in the archive within `RECENT`, not undone since; false for no recipients. `serve-submit` drops a client's duplicate iMIP with it, and only a duplicate: another answer, another instance or a recipient the scheduler did not tell makes a message of its own |

Implementations are safe to share between tasks.

## Errors

| `ScheduleError` | Meaning |
| --- | --- |
| `Invalid(reason)` | The iCalendar object cannot be read |
| `Unavailable(text)` | The outbox could not be reached; nothing was queued |

Reasons never carry addresses or content.

## Writing an implementation

Enable the `testing` feature under `[dev-dependencies]` and run the suite with a `Harness` that builds the scheduler over the `Submission` and `Clock` it is given:

```rust
holtmail_schedule::testing::conformance(&harness).await;
```

`World` also carries the fake's identity id and its `Driver`, which hands a released message to its transport and leaves a held one alone. The suite runs the scheduler over `holtmail_submission::testing::FakeSubmission` wrapped in a `Recorder` (which keeps every new message queued) and a `ManualClock`. Fixtures are single invented events: Ada (`ada@example.com`, the fake submission's identity) organizes `planning-1@example.com` with Bob, Carol (`SCHEDULE-AGENT=CLIENT`) and Dave (`NONE`); Bob organizes `review-1@example.org` with Ada. It checks stripping, the attendee reduction, keeping merged answers under a matching tag, the organizer and attendee paths for each host, an answer through a reverse alias, `3.7` and `5.1`, duplicates, the window, recipients and instances of `just_sent`, an outbox undo, holding until `confirm`, deletes, withdrawal, and every incoming decision (including DMARC for another domain and several `From` addresses), and that incoming messages never queue anything.

`testing::FakeScheduler` keeps the contract for single events without recurrence or folded lines, for tests of front ends; it does not raise `SEQUENCE`.
