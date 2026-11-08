# holtmail-pim-edit

Edits to contacts and events, for held changes from CalDAV/CardDAV clients and the web app ([CLIENTS.md, Storage](../../../docs/CLIENTS.md#storage), [WRITE.md, Contacts and calendars](../../../docs/WRITE.md#contacts-and-calendars)). It validates what a client sends, classifies it against the version it was based on, re-applies it on a newer upstream version, gives the three-way field diff of the conflict view and the field patch for write-through, and turns the web app's event and contact forms into iCalendar and vCard. A pure library over [`holtmail-pim`](../../pim/docs/README.md): no I/O, no store, and the same inputs always give the same output. It reads iCalendar and vCard as content lines of its own (with `calcard` and `holtmail-pim` as the judge of validity), so whatever it does not model, such as alarms, client properties, changed instances and Apple `itemN.` labels, survives a rebase or a form edit untouched. Used by the pim-sync post-batch step, `pim-writeback`, the API and later `dav-serve`.

## API

```rust
let valid = validate(ItemKind::Event, &bytes, Some(&base))?;          // Valid { uid } or Invalid
let c = classify(ItemKind::Event, Some(&base), &bytes, &own_addresses)?; // Classified { class, fields, patch }
let json = serde_json::to_value(&c.patch)?;                          // the store's opaque `patch`
let r = rebase(&patch, &newer_upstream, &own_addresses);             // Rebased
let view = field_diff(kind, Some(&base), Some(&mine), Some(&theirs)); // Vec<FieldDiff>
let served = serve(Some(&patch), &mine, base_number, Some((n, &latest)), &own); // what is served
let write = item_patch(&patch, &served);                             // holtmail_collection::ItemPatch
let form = event_form(ItemKind::Event, &raw);                        // Option<EventForm>
let ics = build_event(ItemKind::Event, &form, Some(&base), now_ms)?; // Vec<u8>
let vcf = build_contact(&contact_form, None, now_ms)?;               // Vec<u8>
```

### Validation: `validate`

`validate(kind, raw, replaces)` accepts bytes that are:

- at most `MAX_ICALENDAR` (2 MiB) or `MAX_VCARD` (5 MiB), `max_size(kind)`;
- one well-formed `VCALENDAR` (balanced `BEGIN`/`END`) or one `VCARD`, that `holtmail-pim` (through `calcard`) reads;
- for a calendar: components of the item's kind only (`VEVENT` for events, `VTODO` for tasks, `VJOURNAL` for other), at least one, each instance (`RECURRENCE-ID`, or none for the series) once;
- with one non-empty `UID`, on every component;
- with the `UID` of `replaces`, when that is readable and has one.

Each `Invalid` reason names its precondition with `precondition(kind)`: `max-resource-size`, `no-uid-conflict` (UID changed), `supported-calendar-component` (wrong component), else `valid-calendar-data` or `valid-address-data`. Whether another item of the collection already has the `UID` needs the store and is the caller's check.

### Classification: `classify`

Both versions are cut into components (the series and each changed instance, keyed by `RECURRENCE-ID` with its `TZID`; a vCard is one component) and each component into property groups: one group per property name, one per attendee (keyed by the normalized address), one for all `VALARM`s, one per other sub-component. Groups compare as sets of canonical lines: property names uppercase, vCard `itemN.` groups dropped, parameters sorted with `TYPE` values lowercased and merged, `SCHEDULE-STATUS` dropped, folding undone. So reordering, refolding and parameter case are never changes.

Ignored entirely: `DTSTAMP`, `LAST-MODIFIED`, `CREATED`, `SEQUENCE`, `UID`, `X-MOZ-GENERATION`, everything at `VCALENDAR` level and the `VTIMEZONE`s; in a vCard `VERSION`, `PRODID`, `REV`, `UID`.

| Class | When |
| --- | --- |
| `create` | There is no base |
| `noop` | No group differs |
| `personal` | Only `VALARM`, `X-MOZ-*`, `X-APPLE-*`, `X-EVOLUTION-*` (and `X-AB*` in a vCard) differ |
| `reply` | The user's own attendee entry differs only in `PARTSTAT`/`RSVP`, plus at most personal groups and what RFC 6638 §3.2.2.1 allows an attendee (`TRANSP`, `PERCENT-COMPLETE`, `COMPLETED`). A new instance counts when it starts at its `RECURRENCE-ID` and differs from the series only like that (Apple's "decline this occurrence") |
| `edit` | Anything else, including an answer on someone else's entry, `TRANSP` alone, a removed instance, or a base that cannot be read |
| `delete` | Never returned; the caller knows a delete |

The user's entry is matched against `own` addresses in any spelling `normalize_email` accepts (`MAILTO:Ada@Example.com`). `Class::as_str` gives the store's names.

`Classified::fields` (also `Patch::fields`) lists the touched parts as `holtmail_collection::Field`, each once, in group order: `SUMMARY` → `Summary`, `DTSTART` → `Start`, `DTEND`/`DURATION`/`DUE` → `End`, `RRULE`/`RDATE`/`EXDATE`/`EXRULE` and an added or removed instance → `Recurrence`, `ATTENDEE` → `Attendees`, `VALARM` → `Alarms`, `TRANSP` → `Transparency`, `CLASS` → `Privacy`; in a vCard `FN`/`N`/`NICKNAME` → `Name`, `EMAIL`, `TEL`, `ADR`, `ORG`, `TITLE`/`ROLE`, `NOTE`, `BDAY`/`ANNIVERSARY` → `Dates`, `PHOTO`, `URL`, `CATEGORIES`; anything else is `Property(NAME)`. Empty for `noop`.

### The patch: `Patch`

Serializable (`serde`), to be stored as the change's `patch`. It holds, per component, whether the base and the change have it, the whole component text for an added one, and for each differing group the base's lines and the change's lines; plus the change's `VTIMEZONE`s. The JSON shape is private to this crate; later versions keep reading it. `Patch::class`, `Patch::kind`, `Patch::is_empty` (nothing that matters changed).

### Rebase: `rebase`

`rebase(patch, theirs, own)` re-applies a patch on a newer upstream version, group by group:

| Upstream's group | Result |
| --- | --- |
| Equal to the base's | The change's value |
| Equal to the change's | Kept |
| Different from both, the user's own answer | Upstream's entry with the change's `PARTSTAT`/`RSVP`; upstream removed the entry → a reply is obsolete, an edit conflicts |
| Different from both, `DTSTART`, `DTEND`, `DURATION`, `DUE`, `RRULE`, `RDATE`, `EXDATE`, `EXRULE`, an attendee, or `STATUS` when upstream says `CANCELLED` | Upstream's value; a conflict on that field (a reply is obsolete instead) |
| Different from both, anything else | The change's value (the user's edit stays on top) |

Instances: one the change added is written from its text when upstream lacks it, merged otherwise; one the change touched that upstream removed is a conflict (`Recurrence`) or makes a reply obsolete; one the change removed is removed only if upstream left it as the base had it, else a conflict. A reply is also obsolete when the series or a touched instance is `STATUS:CANCELLED` upstream. Upstream's `DTSTAMP`, `SEQUENCE`, `PRODID` and everything the patch does not name are kept; missing `VTIMEZONE`s for `TZID`s the change brought are added from the patch. Contacts have no upstream-wins fields: the change wins every overlap.

| `Rebased` | Meaning | `reason()` |
| --- | --- | --- |
| `Applied` | Upstream already holds every group as the change has it (or the patch is empty) | — |
| `Clean(bytes)` | Serve these bytes | — |
| `Conflict { served, fields }` | Upstream won `fields`; `served` is the merge | `overlap` |
| `ReplyObsolete { served }` | The reply no longer applies | `reply_obsolete` |
| `Unreadable` | Upstream cannot be read; keep serving the change | `upstream_unreadable` |

`served()` gives the bytes of the three middle cases. `Patch::attendees()` lists the normalized addresses of the attendee entries a patch touches; for a `reply` they are the user's own entries, so a reply can be rebased without knowing the user's addresses (the server's write-through does this). An item deleted upstream is the caller's case (`upstream_deleted`); a delete change is never rebased.

### What is served: `serve`

`serve(patch, mine, based_on, latest, own)` gives the bytes a held change serves: `mine` while the item's newest upstream version (`latest`, as iCalendar or vCard text with its number) is still the one the change was based on, or when there is no patch or no upstream version; otherwise `rebase` on `latest`: upstream's own bytes when it already holds the change, the merge for `Clean`, `Conflict` and `ReplyObsolete`, and `mine` when upstream cannot be read. Deterministic, so a served ETag computed from it is stable. The pim-sync post-batch step, `pim-writeback` and the API all serve through it.

### Conflict view: `field_diff`

`field_diff(kind, base, mine, theirs)` compares the normalized models of `holtmail-pim` (`Normalized::fields`, so the names and value texts are those of `holtmail_pim::diff`), `None` standing for a side without an item. It returns the fields where `mine` or `theirs` differs from `base`, in order of first appearance, each with the three value lists and `mine_changed`, `theirs_changed` and `conflict` (both changed and differ from each other). Several values compare as a set. `sequence`, `rev` and `uid` are left out; alarms and client properties are not in the model and never conflict.

### Write-through: `item_patch`

`item_patch(patch, served)` makes the `ItemPatch` of `holtmail-collection`: `content_type` from the kind (`text/calendar` or `text/vcard`), `raw` = `served` (the client's bytes or a rebase), `uid` from them, and `fields` = `Patch::fields`, or `None` for a create so a field-patching writer sends everything.

### Forms: `EventForm`, `ContactForm`

`event_form(kind, raw)` and `contact_form(raw)` read a form from bytes; `build_event(kind, form, base, now)` and `build_contact(form, base, now)` write one (`now` in milliseconds since the epoch). Without a base the form needs a `uid` (the caller makes one).

- **Event:** `title`, `start`/`end` (`YYYY-MM-DD` for all-day, end exclusive; else `YYYY-MM-DDTHH:MM[:SS]`), `all_day`, `time_zone` (`UTC`, an IANA or Windows name, or `None` for a floating time), `recurrence` (an `RRULE` value with `FREQ`, letters, digits and `=;,:+-` only), `location`, `description`, `organizer`, `attendees` (`Person { email, name, partstat }`), `alarms` (`Alarm { minutes_before }`, display alarms relative to the start). A missing end is one day (all-day) or one hour. A named zone gets a `VTIMEZONE` generated by `calcard` for the years of the event when the object has none. New attendees get `ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE`.
- **Contact:** `full_name` (else made from the name parts, the organization or the first email), `name` (`NameParts`), `emails` and `phones` (`Labeled { value, kinds }`), `addresses` (`PostalAddress`), `organization`, `title`, `notes`. A new card is vCard 3.0, which every CardDAV server (Google included) takes.
- **With a base,** only what the form changed is rewritten; everything else stays: changed instances, `VTIMEZONE`s, client properties, attendee parameters (an attendee whose name and answer the form left alone keeps its line; a changed one keeps its other parameters), alarms with the offsets the form shows (Apple alarm UIDs), the vCard version, pictures, birthdays, and the lines of unchanged emails, phones and addresses with their `itemN.` labels. A label left without its property is dropped. `DTSTAMP` becomes `now`, `LAST-MODIFIED` too when present or when rescheduled, and `SEQUENCE` goes up by one when the time or the rule changed; a vCard's `REV` becomes `now`. Saving an unchanged form classifies as `noop`.
- Alarms not relative to the start (absolute, or relative to the end) are not shown; changing the alarm list in the form drops them.
- `FormError`: `MissingUid`, `BadTime(field)`, `EndBeforeStart`, `UnknownZone`, `BadRule`, `BadAddress`, `Empty`, or `Invalid` when the base cannot be read or the result fails `validate`.

### iMIP objects: `reply`, `request`, `cancel`, `organize`

The iTIP objects (RFC 5546) Holtmail sends by mail; `holtmail-compose`'s `imip` wraps them in the message (RFC 6047). They live here because they are edits of the same content lines: whatever is not sent stays in the copy untouched. `now` is milliseconds since the epoch and becomes `DTSTAMP`. Each returns `Imip { method, calendar, uid, sequence, recurrence_id, organizer, recipients, summary, when, location }`: the `VCALENDAR` with `METHOD` (CRLF, folded at 75 octets), and the facts the mail needs (`recipients` normalized, each once, never empty; `when` a readable start such as `2026-12-01 10:00 (Europe/Berlin)`).

```rust
let imip = reply(&invitation, attendee_address(&routed), Partstat::Accepted, None, Some("Note"), now)?;
let imip = request(&copy, Audience::Everyone, now)?;
let imip = cancel(&copy_before, CancelScope::Instance("20261210T150000Z"), Audience::Everyone, now)?;
let Organized { event, messages } = organize(Some(&before), Some(&after), server_schedules, now)?;
```

- **`reply(invitation, attendee, answer, recurrence_id, comment, now)`**, an attendee's `REPLY`: the series (or every changed instance when the invitation holds only those), or the one instance named, from its changed instance or made from the series (`RECURRENCE-ID` and `DTSTART` with the series' `TZID` or `VALUE=DATE`). Each component has `UID`, `RECURRENCE-ID`, a fresh `DTSTAMP`, the invitation's `SEQUENCE` unchanged (RFC 5546 section 2.1.4), `ORGANIZER`, exactly one `ATTENDEE` (the user's own line, any spelling of the address, with its parameters kept, `PARTSTAT` set and `RSVP` dropped), `SUMMARY`, the start and end, and `COMMENT`. Other attendees, alarms and descriptions are not sent. The attendee is the address the invitation was sent to: through an alias, the alias (`holtmail_route::attendee_address`).
- **`request(event, audience, now)`**, the organizer's `REQUEST`: the series and every changed instance as stored, with a fresh `DTSTAMP` and `SEQUENCE` (0 added when absent); alarms and `X-MOZ-*` stay home.
- **`cancel(event, scope, audience, now)`**, the organizer's `CANCEL`, from the copy as it was before: `CancelScope::Series`, `Instance(rid)` (from its changed instance or made from the series) or `Attendees(list)` (only those, listing only them). Every scope carries `SEQUENCE` one above the copy's (the highest of series and instance), `STATUS:CANCELLED`, the organizer, `SUMMARY` and the start.
- **`organize(previous, next, server_schedules, now)`**, the organizer's side of a change: a create is one `REQUEST`, a delete one series `CANCEL`; in an update removed attendees get a `CANCEL` naming only them, each `EXDATE` added to the series a `CANCEL` of that instance, and a change of anything but answers, alarms, client properties and `EXDATE`s a `REQUEST` to everyone (to the new attendees alone when attendees were only added; answers alone send nothing). `SEQUENCE` goes one above the previous copy's for every component whose start, end, duration, recurrence, `EXDATE` or status changed (RFC 5545 section 3.8.7.4) unless the new copy already raised it, and a changed start, end or recurrence resets every other attendee to `PARTSTAT=NEEDS-ACTION;RSVP=TRUE`. The returned `event` is the copy to record (the bytes as given when nothing needed changing). An event without organizer or without anyone to tell gives no messages and no error.
- **`Audience`:** `Everyone`, `ClientScheduled` (only attendees with `SCHEDULE-AGENT=CLIENT`, for a server that schedules the rest) or `Only(list)`. The organizer and attendees with `SCHEDULE-AGENT=NONE` never get a message.
- `SCHEDULE-AGENT`, `SCHEDULE-STATUS` and `SCHEDULE-FORCE-SEND` never leave in a message; the `VTIMEZONE`s the message uses are copied.
- `organizer(raw)` is the organizer's normalized address; `reply_agent(raw)` the `SCHEDULE-AGENT` on `ORGANIZER`, which decides who sends an attendee's answer (RFC 6638 section 7.1).
- `ImipError`: `Unreadable`, `NoOrganizer`, `NotAttendee`, `NoInstance` (also for a malformed `RECURRENCE-ID`, which can never inject a line), `NoRecipients`.

### Answers on copies: `answered`, `apply_reply`, `Respond`

- **`answered(copy, attendee, answer, recurrence_id)`** is the account's copy with the user's own answer set (`PARTSTAT` set, `RSVP` and `SCHEDULE-STATUS` dropped): the bytes of the `reply` change the RSVP API records. `None` answers the series and every changed instance that lists the attendee; `Some(rid)` answers that instance only, adding a changed instance made from the series (its `RECURRENCE-ID` and `DTSTART`, a `DURATION` for the series' length, everything else as the series has it) when the copy has none, which `classify` reads as a `reply`. Nothing but the attendee's own lines changes. `NotAttendee` when no answered component lists the address, `NoInstance` for an instance the event cannot have.
- **`apply_reply(copy, reply, senders)`** is the organizer's copy with an attendee's iMIP `REPLY` applied: that attendee's `PARTSTAT` on the series or the instance the reply names (added from the series as above when the copy has no changed instance for it and the series really has that instance). `None` when it changes nothing: not a `REPLY`, another `UID`, not exactly one `ATTENDEE`, an attendee who is none of `senders`, a `SEQUENCE` older than the copy's, an attendee the copy does not list, or an answer it already has. Never touches the organizer's own line.
- **Instances** are matched by their instant (`RECURRENCE-ID` resolved with its `TZID` or the event's `VTIMEZONE`s), so a reply in UTC finds a changed instance written in a zone, and the same wall-clock time in another zone does not. `answered`, `reply` and `apply_reply` make an instance only when the series has it (its rule or `RDATE`s, not removed by an `EXDATE`); anything else is `NoInstance` or ignored.
- **`holds_series(raw)`** says whether an event holds its series or only changed instances; **`with_instances(base, updates)`** lays later instance-only updates over a series, replacing by instant, so an invitation from mail stays answerable as a series after an instance-only `REQUEST`.
- **`Respond { answer, recurrence_id, notify }`** rides inside a `reply` change's stored `Patch` (`Patch::with_respond`, read back with `Patch::respond`; ignored for other classes): the writer then answers through `CollectionWriter::respond` where it offers that, telling the organizer only with `notify` (the `Server` route). An answer that was mailed as an iMIP `REPLY` carries `notify: false`, so a copy written later, also from `held` mode once the collection writes through, never makes the server answer a second time.

### Who sends: `reply_route`, `organizer_route`

Pure choices over `Host` (the collection holding the event: `CalDav { auto_schedule }` from RFC 6638's `calendar-auto-schedule`, `Graph`, `Jmap { scheduling }` for `sendSchedulingMessages`, or `Mail` for "From mail") and the `WriteCaps` the account may use there now (`WriteCaps::default()` when the collection is not `write_through` or the capability `pim.respond` or `pim.edit` is off). `Host::schedules` says whether writing makes the server send iTIP messages.

`reply_route(host, caps, agent) -> ReplyRoute` ([CLIENTS.md, Invitations and RSVP](../../../docs/CLIENTS.md#invitations-and-rsvp)):

| Situation | Route |
| --- | --- |
| `SCHEDULE-AGENT` server, the host schedules, `caps.respond` | `Server`: `respond` with `notify_organizer`, no mail |
| `SCHEDULE-AGENT=NONE` | `Quiet { copy }`: nobody is told |
| Anything else ("From mail", writes off, a host that does not schedule, `SCHEDULE-AGENT=CLIENT`) | `Imip { copy }`: a `REPLY` through the outbox |

`copy` (`CopyUpdate`) is how the account's copy follows: `Respond` (`respond` without `notify_organizer`) when `caps.respond`, else `Put` (the held `reply` change written) for CalDAV with `caps.update` where a `PUT` cannot make the server answer too (no auto-scheduling, or an agent other than the server), else `Held`. The organizer is never told twice.

`organizer_route(host, caps, change) -> OrganizerRoute` for `OrganizerChange::{Create, Update, Delete}`: `Server` where the host schedules and `caps` allows the change (Holtmail then mails only `SCHEDULE-AGENT=CLIENT` attendees: `organize(.., server_schedules: true, ..)`), `Deferred` where it schedules but the change cannot be written now (the server sends when it is, so nobody gets a message twice; Holtmail mails only `SCHEDULE-AGENT=CLIENT` attendees, whom the server never tells), `Imip` wherever the host does not schedule.

## Limits

- vCard 2.1 quoted-printable values are unfolded but not decoded in forms; CardDAV servers speak 3.0 and 4.0.
- `SEQUENCE` is not raised by a rebase; `organize` raises it for the organizer's own changes, also when an attendee is only removed, so the stored copy is never older than the `CANCEL` that attendee holds and a later re-invite is not ignored as stale.
- iMIP covers events (`VEVENT`); tasks are never scheduled. `COUNTER`, `REFRESH`, `ADD` and delegation are not built.
- A `REPLY` for an address that is not on the invitation (a party crasher, an invitation sent to a list) is refused with `NotAttendee`.
- A recurring series edited in one instance is a whole-resource change, classified and rebased as a whole (by component).

## Tests

`cargo test -p holtmail-pim-edit`. Hand-written fixtures in `tests/fixtures` (invented people on `example.com`/`example.org`) mimic how clients save: Apple Calendar accepting an invitation and declining one occurrence, Thunderbird resaving and snoozing an alarm, DAVx5 moving an event, Apple and DAVx5 saving contacts; plus upstream versions that move, rename, cancel and uninvite.

`tests/imip.rs` compares hand-written expected objects in `tests/fixtures/imip/` byte for byte (after making line endings CRLF): replies as an alias to a series, to a changed instance and to an instance made from the series; a `REQUEST` without alarms, client properties or scheduling parameters; a `CANCEL` of one instance; and an update that moves the event, removes one attendee and adds another (the `CANCEL` to the removed one and the `REQUEST` with the raised `SEQUENCE` and reset answers). Every output is read back by `holtmail-pim` and checked for 75-octet lines. Further cases: refusals, all-day instances, audiences, answers or alarms alone sending nothing, attendees only added, an `EXDATE`, a new title keeping `SEQUENCE`, creates, deletes and server-scheduled changes. `src/schedule.rs` tests both route choosers; `src/answer.rs` answers a series and an instance (both classified `reply`), refuses strangers and impossible instances, and applies a reply to the organizer's copy once, never an older `SEQUENCE`, another method or another `UID`.
