# holtmail-recur

Recurrence expansion for the calendar: given a normalized `holtmail_pim::CalendarObject`, the instances that fall into a window of time, in UTC and as wall-clock times in their own zone. It reads `RRULE`, `RDATE`, `EXDATE`, changed and cancelled instances (`RECURRENCE-ID`, with `RANGE=THISANDFUTURE`), all-day and floating events, IANA and Windows zone names and the object's own `VTIMEZONE` rules. A library, not a contract: RFC 5545 has one right answer. Nothing here does I/O or logs.

## Why its own engine

`calcard` (which `holtmail-pim` parses with) has an `expand_dates`, but its rule iterator is private, it matches changed instances by `SEQUENCE` as well as `RECURRENCE-ID` (Google and Outlook bump `SEQUENCE` on single changed instances, so those would be missed), it has no window (only a count) and does not apply `EXDATE` to `RDATE`s. The `rrule` crate is the same code `calcard` vendors. So the rules are expanded here, on wall-clock times, in about 500 lines with their own tests; `calcard` is still what reads the text and names zones.

## API

- `expand(object, window) -> Expansion` — the instances overlapping `window` (`Window { from, to }`, UTC milliseconds, `to` excluded; `Window::ALL` for everything), by start. The same object and window always give the same instances.
  - An event without `RRULE` or `RDATE` has one instance with no `recurrence_id`.
  - A recurring event has `DTSTART` as its first instance (it counts toward `COUNT` even when the rule would not produce it, as RFC 5545 says), then what the rule produces up to `COUNT` or `UNTIL`, plus `RDATE`s (a period's start), less `EXDATE`s. `EXDATE`s also count toward `COUNT`.
  - `UNTIL` in UTC (`…Z`) is compared with each instance in UTC; a local `UNTIL` with wall-clock times; a date `UNTIL` includes its whole day. All inclusive.
  - `EXDATE`s match by instant, read in their own zone (so an `EXDATE` written with a Windows zone name removes the right Berlin instance), or by day for dates.
  - A changed instance replaces the instance its `RECURRENCE-ID` names wherever it moved it, and keeps that instance's `recurrence_id`; one with `STATUS:CANCELLED` stays, marked `cancelled`. With `RANGE=THISANDFUTURE` every later instance moves by the same shift and takes its fields and length. A changed instance whose series is missing (an invitation for one date) is still listed.
  - Times are generated in the zone of `DTSTART` and keep their wall-clock length across daylight saving changes: a weekly 09:00 Berlin meeting is 08:00 UTC in winter and 07:00 UTC in summer. A local time skipped by a change reads with the offset before it (02:30 becomes 03:30); one repeated reads as the earlier.
  - Dates count from midnight UTC and last whole days; floating times read as if they were UTC and are marked `floating`.
  - At most `MAX_OCCURRENCES` (20 000) instances per call; `Expansion::truncated` says when more were cut. Endless rules jump straight to the window (without `COUNT`). A rule ends when its periods stay empty for 400 years in a row (the Gregorian calendar, weekdays included, repeats every 400 years, so nothing can match later): impossible rules (`FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30`) stop after about 400 periods, while sparse ones (`FREQ=HOURLY;BYMONTH=1`, `FREQ=DAILY;BYMONTH=2;BYMONTHDAY=29` across 2096–2104) keep producing instances. Hourly, minutely and secondly rules step over whole days that `BYMONTH`, `BYMONTHDAY`, `BYYEARDAY` or `BYDAY` rule out. A rule also stops after 2 000 000 periods in all and at a period that would hold more than 100 000 times (`BYHOUR`, `BYMINUTE` and `BYSECOND` lists across a year); both set `truncated`, since instances may follow. It ends past the year 9999, whatever its frequency.
- `Occurrence` — `component` (the index of the component whose fields the instance has), `recurrence_id` (`2025-03-10T08:00:00Z` in UTC for times, `2025-03-10` for dates, as `holtmail_pim::EventKey` writes it, so copies of an event in other calendars name the same instance the same way), `start`/`end` (UTC ms), `start_local`/`end_local`, `time_zone` (the `TZID` as written), `all_day`, `floating`, `cancelled`, `changed`.
- `span(object) -> Option<Span>` — the earliest start and, unless the object repeats without `COUNT` or `UNTIL`, the latest end.
- `Rule::parse(text)` — an `RRULE` into its parts (`Frequency`, `interval`, `count`, `until` as written, `by_day` as `WeekdayNum { ordinal, weekday }`, `by_month_day`, `by_month`, `by_year_day`, `by_week_no`, `by_set_pos`, `by_hour`, `by_minute`, `by_second`, `week_start`), any case; `RuleError::{Frequency, Value}` for what it cannot read. `weekday_code(Weekday)` writes `MO` … `SU`.
- `wall_clock(utc, zone)` — `2025-03-10T09:00:00` in an IANA or Windows zone, `UTC` or an offset such as `+05:30`; `None` for an unknown name.

## Time zones

A `TZID` is read, in order, as: the object's own `VTIMEZONE` with that `TZID` when it names an IANA zone (by its `TZID`, `X-LIC-LOCATION` or Microsoft zone number, which `holtmail-pim` records as `ZoneDefinition::iana`); `UTC`; a fixed offset; an IANA name, a Windows name (`W. Europe Standard Time`, `Eastern Standard Time`, the full table `calcard` carries) or a vendor prefix around an IANA name (`/softwarestudio.org/…/America/Chicago`); otherwise the object's own `VTIMEZONE` rules (`STANDARD`/`DAYLIGHT` onsets from their `DTSTART`, `RRULE` and `RDATE`, up to the year 2200, from at most 64 observances; the rules of a `VTIMEZONE` are worked out once per thread and reused). A `TZID` none of these explain is read as floating, like `holtmail-pim` reads its `utc`. IANA data comes from `chrono-tz` 0.10 (compiled in; a zone rule change needs a new build).

## Limits

- `RSCALE`, `SKIP` and `BYEASTER` are ignored; `BYSECOND=60` counts as 59.
- `RANGE=THISANDPRIOR` (deprecated) is treated as a single changed instance.
- Expansion works on wall-clock times of `DTSTART`'s zone; an `RDATE` in another zone is moved into it by its instant.
