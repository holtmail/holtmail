# holtmail-pim

Contacts and calendar events for Holtmail. It cuts `.vcf` and `.ics` streams into items, reads one raw item into a normalized model, compares two versions field by field, and gives the keys that match the same event or person across collections. A library, not a contract: each format has one right reading. Parsing goes through [`calcard`](https://docs.rs/calcard) (Stalwart Labs, Apache-2.0 OR MIT) with its `jmap` feature for JSContact and JSCalendar; no `calcard` type appears in this crate's API. Besides vCard and iCalendar it reads the JSON of Microsoft Graph contacts and events and JSContact/JSCalendar (JMAP) items. Nothing here does I/O.

## API

```rust
let items = holtmail_pim::split(&file_bytes);           // Vec<RawItem { kind, uid, raw }>
let name = holtmail_pim::calendar_name(&file_bytes);    // X-WR-CALNAME, if any
let read = holtmail_pim::read(ItemKind::Contact, &raw); // Read { normalized, photo }
let normalized = holtmail_pim::normalize(kind, &raw);   // read() without the picture's bytes
let read = holtmail_pim::read_as(Format::GraphEvent, ItemKind::Event, &json); // any format
let format = Format::stored(&version.content_type, kind); // the format of stored bytes
let changes = holtmail_pim::diff(&before, &after);      // Vec<Change { field, before, after }>
let method = holtmail_pim::calendar_method(&part);      // iMIP METHOD of a calendar part, if any
let view = holtmail_pim::merge(&[(Method::Request, &v1), (Method::Reply, &v2)]); // Normalized
let steps = holtmail_pim::timeline(&versions);          // Vec<Vec<Change>>, one per version
```

### Splitting: `split`

- Every top-level `VCARD` is one item whose bytes are exactly its lines in the file (a byte order mark at the start of the file is left with the first item).
- Inside a `VCALENDAR`, components with the same type and `UID` (a recurring event and its changed instances) are one item. A component without `UID` is an item of its own. The item's bytes are the calendar's own lines (`BEGIN:VCALENDAR`, `VERSION`, `PRODID`, `X-WR-CALNAME`, …), the `VTIMEZONE`s it names in a `TZID`, and its components, each copied byte for byte in file order, then `END:VCALENDAR`. A file holding one object in that order therefore gives back its own bytes. Once the copies of the calendar's lines and zones add up to more than 16 times the stream (at least 64 MiB), later items carry only `BEGIN:VCALENDAR`, their components and `END:VCALENDAR`, so a hostile file cannot multiply into gigabytes.
- A component still open at the end of the stream runs to the end, so nothing is lost; it usually reads as unreadable. Lines outside any component are ignored.
- `RawItem::uid` is the unfolded `UID`; `RawItem::kind` comes from the `BEGIN` line: `VCARD` → `Contact`, `VEVENT` → `Event`, `VTODO` → `Task`, anything else (`VJOURNAL`) → `Other`.

### Reading: `read` and `normalize`

Never fails. Bytes that do not parse give `Normalized::Unreadable`; the caller still archives them. Text is read as UTF-8 with invalid sequences replaced; vCard 2.1 quoted-printable and base64 values are decoded with their `CHARSET` by `calcard`. The same bytes always give the same value.

`Normalized` is `Contact(Box<Contact>)`, `Calendar(CalendarObject)` or `Unreadable`, and serializes with a `type` tag (`contact`, `calendar`, `unreadable`).

| `Contact` field | From |
| --- | --- |
| `uid`, `rev` | `UID`, `REV` (a time as ISO 8601) |
| `full_name`, `name` | `FN`; `N` by part (`family`, `given`, `additional`, `prefix`, `suffix`) |
| `nicknames` | `NICKNAME` |
| `emails` | `EMAIL`: `address` as written, `normalized`, `kinds` (`TYPE`, lowercase, without `internet` and `pref`) |
| `phones` | `TEL`: `number` as written, `e164`, `kinds` |
| `organization`, `title` | `ORG` units joined by `, `; `TITLE` |
| `addresses` | `ADR` by part, empty addresses dropped |
| `birthday` | `BDAY`: `YYYY-MM-DD`, `--MM-DD` without a year, or the text |
| `note` | every `NOTE`, joined by an empty line |
| `photo` | the first `PHOTO`: `Embedded { blob, content_type, size }` for bytes (base64, `ENCODING=b`, or a `data:` URI), `Link { url }` for an address, which is never fetched |

`Read::photo` carries the embedded picture's bytes so the caller can store them as a blob of their own; `Photo::Embedded::blob` is their BLAKE3 id. The media type is sniffed from the bytes (PNG, JPEG, GIF, WebP) before the vCard's `TYPE`/`MEDIATYPE` is trusted.

`CalendarObject::components` holds the `VEVENT`s, `VTODO`s and `VJOURNAL`s of the item, the one without `RECURRENCE-ID` first, then changed instances by their `RECURRENCE-ID`; `main()` is the first. Alarms and time zone definitions are not components. Each `Event` has `kind`, `uid`, `recurrence_id`, `summary`, `description`, `location`, `start`, `end` (`DTEND`, `DUE`, or start plus `DURATION`), `all_day`, `recurring` (`RRULE` or `RDATE`), `organizer` and `attendees` (`email`, `name` from `CN`, `partstat`, `role`), `sequence`, `dtstamp` (UTC milliseconds) and `status`. For recurrence (expanded by [`holtmail-recur`](../../recur/docs/README.md)) it also keeps `rrule` (the `RRULE` as `calcard` writes it back, `FREQ=WEEKLY;UNTIL=20250428T070000Z;BYDAY=MO`), `rdates` and `exdates` (each value as an `EventTime`, a period's start for `RDATE;VALUE=PERIOD`), `this_and_future` (`RECURRENCE-ID;RANGE=THISANDFUTURE`) and `last_modified` (UTC milliseconds). These fields are left out of the JSON when empty, so events without them serialize as before.

`CalendarObject::zones` lists the object's `VTIMEZONE`s as `ZoneDefinition { tzid, iana, observances }`: `iana` is the IANA zone its `TZID`, `X-LIC-LOCATION` or Microsoft zone number names (`W. Europe Standard Time` → `Europe/Berlin`); only when none is known are its `STANDARD`/`DAYLIGHT` `observances` kept (`daylight`, `start` as a local time, `offset_from`/`offset_to` in seconds, `rrule`, `rdates`). `merge` keeps the zones of every merged version. `iana_zone(tzid)` names the IANA zone of a `TZID` alone (IANA, Windows names, vendor prefixes), or `None`. Tasks fill the same fields as far as they have them; nothing else of them is modeled.

`EventTime` keeps three things: `value` as written in ISO form (`2025-03-10`, `2025-03-10T09:00:00`, with `Z` or an offset when present), `time_zone` (the `TZID` as written, `UTC`, an offset, or `None` for floating times and dates) and `utc` in milliseconds. Zones resolve through the item's own `VTIMEZONE`s and the IANA names `calcard` knows, including Microsoft names and `X-LIC-LOCATION`. Dates count from midnight UTC; floating times and unknown zones count as UTC; a local time skipped by a daylight saving change counts as one hour later, a repeated one as the earlier.

`Normalized::title()` is a contact's display name (`FN`, else `N`, else the organization, else the first email or phone) or the main event's summary. `emails()` lists normalized addresses (a contact's, or an event's organizer and attendees) once each; `phones()` a contact's E.164 numbers.

### Formats: `Format`, `read_as`, `normalize_as`

`Format` is `VCard`, `ICalendar`, `GraphContact`, `GraphEvent`, `JsContact` or `JsCalendar`. `content_type()` gives its media type (`text/vcard`, `text/calendar`, `application/vnd.microsoft.graph.contact+json`, `application/vnd.microsoft.graph.event+json`, `application/jscontact+json`, `application/jscalendar+json`), the same strings as `holtmail_collection::media`; `from_content_type` reads one back (parameters and case ignored); `stored(content_type, kind)` falls back to `Format::of(kind)` (vCard for contacts, iCalendar otherwise) for versions stored before formats were recorded. `kind(raw)` tells what one item is: contacts for the contact formats, events for Graph, the first component for iCalendar, `Task` for a JSCalendar `@type: Task`. `read(kind, raw)` is `read_as(Format::of(kind), kind, raw)`. A contact format read as an event, or the other way round, is `Unreadable`.

**JSContact and JSCalendar** are converted by `calcard` to vCard and iCalendar and read by the same code as those, so a card has the same fields whichever way it arrived. Before converting, RFC 8984 names are mapped to the JSCalendar 2.0 ones `calcard` reads (`recurrenceRules[0]` → `recurrenceRule`, a participant's `sendTo.imip` or `email` → `calendarAddress`, `replyTo.imip` → `organizerCalendarAddress`; later rules of `recurrenceRules` are dropped), and the event is wrapped in a `Group`. A `recurrenceOverrides` patch becomes a changed instance; what the patch leaves out (title, description, location, organizer, attendees, status, sequence, the start as the recurrence id, the end from the series' length) is the series' value, and `excluded` is an `EXDATE`. A conversion that panics inside `calcard` gives `Unreadable`.

**Graph contacts** map field by field:

| `Contact` | Graph |
| --- | --- |
| `full_name`, `name` | `displayName`; `givenName`, `surname`, `middleName`, `title` (prefix), `generation` (suffix) |
| `nicknames` | `nickName` |
| `emails` | `emailAddresses[].address`, no kinds |
| `phones` | `businessPhones` (`work`), `homePhones` (`home`), `mobilePhone` (`cell`) |
| `organization`, `title` | `companyName`, `department` joined by `, `; `jobTitle` |
| `addresses` | `homeAddress` (`home`), `businessAddress` (`work`), `otherAddress` (`other`): `street`, `city`, `state`, `postalCode`, `countryOrRegion`; empty ones dropped |
| `birthday`, `note`, `rev` | the date of `birthday`; `personalNotes`; `lastModifiedDateTime` |

`uid` stays empty (Graph contacts have none) and photos are not read.

**Graph events**: the master (or single event) is the first component and each of `exceptionOccurrences` a changed instance. `uid` is `iCalUId` (the master's for its exceptions, whose own `iCalUId` differs); `summary` `subject`; `description` the `body` (HTML reduced to text when Graph sent HTML); `location` `location.displayName`, else every `locations[].displayName`. Times: Graph sends `start`/`end` in UTC by default (or in the zone it names); they are shown in the zone the event was created in, `originalStartTimeZone` (else `recurrence.range.recurrenceTimeZone`, else the zone of `start`), so `value` is the wall-clock time there and `time_zone` the name as written, Windows names such as `W. Europe Standard Time` included. `tzone://Microsoft/Utc` is `UTC`; `tzone://Microsoft/Custom` and unknown names give the UTC time with `Z`. All-day events (`isAllDay`) keep the date of `start` and `end` as written. `recurrence_id` of an exception is `originalStart` in the series' zone. `recurring` is a `recurrence` being set, and its pattern becomes the `rrule`:

| Graph pattern | `RRULE` |
| --- | --- |
| `daily` | `FREQ=DAILY` |
| `weekly` + `daysOfWeek` | `FREQ=WEEKLY;BYDAY=MO,WE`, `WKST` from `firstDayOfWeek` unless Monday |
| `absoluteMonthly` + `dayOfMonth` | `FREQ=MONTHLY;BYMONTHDAY=d` |
| `relativeMonthly` + `daysOfWeek` + `index` | `FREQ=MONTHLY;BYDAY=…;BYSETPOS=1…4 or -1` |
| `absoluteYearly` + `month` + `dayOfMonth` | `FREQ=YEARLY;BYMONTH=m;BYMONTHDAY=d` |
| `relativeYearly` + `month` + `daysOfWeek` + `index` | `FREQ=YEARLY;BYMONTH=m;BYDAY=…;BYSETPOS=…` |
| `interval` > 1 | `INTERVAL=n` |
| range `endDate` / `numbered` / `noEnd` | `UNTIL=YYYYMMDD` (the whole last day, local) / `COUNT=n` / nothing |

`cancelledOccurrences` (`OID.<master id>.YYYY-MM-DD`) become `exdates` at the series' local start time on that date. The organizer and attendees come from `emailAddress`; `status.response` `accepted`/`organizer` → `ACCEPTED`, `declined` → `DECLINED`, `tentativelyAccepted` → `TENTATIVE`, others `NEEDS-ACTION`; `type` `required`/`optional`/`resource` → `REQ-PARTICIPANT`/`OPT-PARTICIPANT`/`NON-PARTICIPANT`. `isCancelled` sets `status` `CANCELLED`; `lastModifiedDateTime` is `last_modified`. Graph has no `SEQUENCE` and no `DTSTAMP`, so both stay empty: a Graph copy of a meeting ranks after copies that state a `SEQUENCE` (see `holtmail-calendar`), and is ranked against other Graph copies by `lastModifiedDateTime`.

### Writing as text: `as_text`

`as_text(format, kind, raw) -> Option<Cow<[u8]>>` gives one item version as a vCard (contacts) or an iCalendar object (the rest), for `.vcf` and `.ics` exports. vCard and iCalendar bytes come back unchanged (borrowed). JSContact goes through `calcard`'s conversion to vCard, as it is read. Graph contacts are written as vCard 4.0 from the normalized `Contact` (`FN`, `N`, `NICKNAME`, `EMAIL`, `TEL` with `TYPE`, `ORG`, `TITLE`, `ADR`, `BDAY` in basic form, `NOTE`, `PHOTO` links, `REV`, and the Graph `id` as `UID`). Graph events and JSCalendar objects are written as iCalendar 2.0 from the normalized `CalendarObject`: one `VEVENT`/`VTODO`/`VJOURNAL` per component with `UID`, `DTSTAMP` (the object's, else `LAST-MODIFIED`, else the epoch), `RECURRENCE-ID` (with `RANGE=THISANDFUTURE`), `DTSTART`, `DTEND` or `DUE`, `SUMMARY`, `DESCRIPTION`, `LOCATION`, `RRULE`, `RDATE`, `EXDATE`, `ORGANIZER` and `ATTENDEE` (`CN`, `ROLE`, `PARTSTAT`), `SEQUENCE`, `STATUS` and `LAST-MODIFIED`. JSCalendar goes through the model rather than `calcard`'s writer because an override is a patch: the changed instance gets what it inherits from the series, such as its end. Times keep the zone the source gave, written as its IANA name when one is known (`W. Europe Standard Time` becomes `Europe/Berlin`), else as given, without a `VTIMEZONE`; values with an offset are written in UTC. Text is escaped and lines are folded at 75 octets between characters. Reading the text back gives the same names, addresses, phones, times, rules and exceptions (`tests/json.rs` checks every JSON fixture). What the model does not hold, such as pictures, categories and alarms, is not written. `None` when the bytes cannot be read in `format` or `format` does not fit `kind`.

### One calendar file: `IcsWriter`

`IcsWriter` writes many calendar items into one `VCALENDAR`, for the `.ics` export: `begin()` (`BEGIN:VCALENDAR`, `VERSION:2.0`, Holtmail's `PRODID`, `CALSCALE:GREGORIAN`), then per item `native(raw)` or `object(&CalendarObject)`, then `end()`.

- `native(raw)` takes a stored iCalendar object: its components other than `VTIMEZONE`, each byte for byte with a CRLF added where one lacks a line end, preceded by the `VTIMEZONE`s they name in a `TZID` that this writer has not written yet. The object's own `VCALENDAR` lines (`METHOD`, `PRODID`, `X-WR-CALNAME`, …) are dropped. Zones are written once per `TZID`, the first definition winning, and always before the first component that names them. `None` when the bytes hold no component.
- `object(object)` writes the components of a normalized object as `as_text` writes Graph and JSCalendar events (zones by name, no `VTIMEZONE`); the export uses it for items from mail (the `merge` view) and JSON sources.

Cutting the file again with `split` gives each native item its components and named zones byte for byte, under the export's calendar lines instead of its own.

### Comparing: `diff`

`fields()` flattens a normalized value into `(name, value)` pairs, a name repeated for several values. `diff` compares two versions by those fields:

- a field with at most one value on each side gives one change from the old value to the new;
- events also have `rule` (the `RRULE`), `rdate` and `exdate` fields, so a changed repetition or a new exception shows on the timeline;
- a field with several values (emails, phones, addresses, attendees, exceptions) is compared as a set: each value gone is a change with only `before`, each new one a change with only `after`, and reordering is no change;
- fields of a changed instance are prefixed `instance <RECURRENCE-ID> `;
- `DTSTAMP` is left out, since servers rewrite it on every export;
- equal versions give no changes; an unreadable version shows as the field `unreadable`.

### Invitations in mail: `Method`, `merge`, `timeline`

`calendar_method` reads `METHOD` from the calendar's own lines (never from inside a component), uppercase. `Method::of(Option<&str>)` maps it to `Publish` (also for none), `Request`, `Reply`, `Add`, `Cancel`, `Refresh`, `Counter`, `DeclineCounter` or `Unknown`; `is_answer()` is true for `Reply`, `Refresh`, `Counter`, `DeclineCounter` and `Unknown`, which speak for an attendee rather than change the event.

`merge` turns the versions of one event, in their order (by `SEQUENCE`, `DTSTAMP`, message date, as the store keeps them), into the event as the mail describes it:

- the base is the latest `PUBLISH` or `REQUEST` that holds the event itself (a component without `RECURRENCE-ID`), else the latest readable version of any method;
- later `REQUEST`s or `PUBLISH`es of single instances replace or add those instances, later `ADD`s add missing instances, later `CANCEL`s set `status` `CANCELLED` on the event or the instances they name (the cancelled fields are kept, nothing is removed); versions before the base change none of its fields;
- every attendee's `PARTSTAT` is the last one said by any `REQUEST`, `PUBLISH` or `REPLY`, matched by address and instance, so a reply after an update shows, and an update after a reply that resets answers wins; attendees known only from a reply are added;
- `COUNTER`, `DECLINECOUNTER`, `REFRESH` and unknown methods change nothing; unreadable versions are skipped, and with none readable the result is `Unreadable`.

`timeline` gives each version's changes for a history: answers list their attendees as `answer` changes, a `CANCEL` gives one `status` change to `CANCELLED` per event or instance it names, and every other version is diffed against the latest earlier version that was neither an answer nor a cancellation. The same versions always give the same results.

### Identity keys

- `normalize_email`: trimmed, without `mailto:` and angle brackets, lowercased as a whole. Lowercasing the local part is not promised by RFC 5321, but common providers treat it that way. `None` for text that is not one address.
- `normalize_phone`: E.164 (`+15550100`) only when the number says its country (`+`, `00` or `tel:+`); spaces, dots, dashes, slashes and brackets are dropped, and an extension (`ext`, `x`, `;ext=`, `#`, `,`) is cut off. Numbers without a country code give `None`: there is no default region and no guessing, and no per-country rules, so an unassigned but well-formed number passes. A full numbering plan (the `phonenumber` crate and its metadata) was left out to keep this crate small.
- `Normalized::contact_keys()`: `email:<address>` and `tel:<E.164>` for a contact. A name is never a key.
- `Event::key()` / `Normalized::event_keys()`: `EventKey { uid, recurrence_id }`, with the recurrence id as a UTC time (`2025-03-17T08:00:00Z`) when its zone is known, else as written.

## Tests

Unit tests cover splitting, the keys, `calendar_method`, and `merge` and `timeline` over requests, updates, replies, counters and cancellations of the event and of one instance; `tests/read.rs` reads the hand-written fixtures in `tests/fixtures/` (vCard 2.1 quoted-printable, 3.0 with folded lines and a base64 photo, 4.0 with a `data:` URI and a birthday without a year; an iCalendar file with a `VTIMEZONE`, a weekly event with a moved instance, an all-day event, a `DURATION` and a task) and checks diffs and JSON round trips; a test reads an `RRULE` with `UNTIL`, `EXDATE`s in a Windows zone, a period `RDATE`, `RANGE=THISANDFUTURE`, `LAST-MODIFIED` and a custom `VTIMEZONE` next to a Windows-named one. `tests/json.rs` reads a Graph contact, a Graph series with a moved and a cancelled occurrence in a Windows zone across the change to summer time, a cancelled all-day Graph event in a custom zone, a JSContact card, an RFC 8984 JSCalendar event with a moved and an excluded instance, and a JSCalendar 2.0 all-day monthly series; unit tests map Graph patterns to rules and HTML bodies to text. Every name, address and number in them is invented.

## Limits

- Recurrence is not expanded; `recurring` only says that a rule exists. Expansion comes with `holtmail-recur`.
- Only the first `PHOTO` is kept apart; logos, sounds and keys stay inside the raw bytes only.
- Text in other charsets than UTF-8 is read lossily unless the vCard names its `CHARSET`.
- Unknown properties are kept in the raw bytes and not shown.
