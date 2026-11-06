# holtmail-calendar

The derived calendar of an archive, over the store contract: which event items are copies of one event, the instances each item contributes, the instances of a span with copies folded together, and how copies differ. A library, not a contract (there is one calendar), like `holtmail-people`. Everything here is derived from the items' normalized fields and rebuilt with them; nothing here logs.

## Groups of copies

Events in several calendars, and the event Holtmail reads from invitations in mail, are copies of one event when they share an iCalendar `UID`. `group_id(archive, uid, item)` is a UUIDv8 (`EventGroupId` in core) from the BLAKE3 hash of the archive id and `uid:<UID>`, or `item:<item id>` for an event without `UID`; `group_of(summary)` reads it from a stored item. The same archive and `UID` always give the same id, so nothing about groups is stored except as derived rows.

Copies are ranked by `leads`: the highest `SEQUENCE` first, then the latest of `DTSTAMP` and `LAST-MODIFIED` (`EventSeries::stamp`), then copies still on their source before removed ones, then item id. The first is the leading copy, the one an event is shown from.

## API

- `series(summary, now) -> Option<(EventSeries, Vec<NewOccurrence>)>` — what an event item contributes: its group, rank fields, span and the instances to store. Items that are no event or cannot be read give `None`. An event that does not repeat, or repeats at most `STORED_WHOLE` (1 000) times, has every instance stored and `window: None`; a longer or endless one has the instances within `WINDOW_MS` (two years) of `now` and that window recorded.
- `rebuild(store, item, now)` — replaces the item's series and instances with what its current fields give (`CalendarStore::replace_series`). `holtmail-ingest` calls the same on every new version, and `holtmail reindex` for every item.
- `instances(store, query)` — every instance overlapping the query's span: stored ones, plus, for items whose stored window does not cover the span (`CalendarStore::uncovered_series`), instances expanded on the fly by `holtmail-recur`. Out-of-window spans are therefore always complete; nothing is written for them.
- `fold(instances) -> Vec<Folded>` — copies of one instance (same group and `recurrence_id`) together, the leading copy first; by the leading copy's start.
- `differences(copies)` — the fields whose values differ between copies (`CopyDifference { field, values per copy }`), compared as sets; attendees and the organizer by who they are, without answers; `UID` and kind never differ.
- `answers(copies)` — every attendee of the event (not of changed instances) named by any copy, with each copy's `PARTSTAT` (`AttendeeAnswers`).
- `object(summary)` — the `CalendarObject` of a stored item's normalized fields.
- `MAIL_COLLECTION` — the `remote_id` of each archive's "From mail" calendar (re-exported by `holtmail-ingest`).

## Limits

- Folding is by `recurrence_id`: when only a non-leading copy has an instance in the asked span (the leading copy moved it out), the instance is shown from that copy.
- The stored window is relative to the time of the last rebuild; it drifts until the item changes or `holtmail reindex` runs, which only costs on-the-fly expansion for spans outside it.
