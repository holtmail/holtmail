//! Expanding invented calendar objects read through `holtmail-pim`.

use holtmail_pim::{CalendarObject, ItemKind, Normalized, normalize};
use holtmail_recur::{Occurrence, Span, Window, expand, span};

fn object(events: &str) -> CalendarObject {
    let raw = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n{}END:VCALENDAR\r\n",
        events.replace('\n', "\r\n")
    );
    match normalize(ItemKind::Event, raw.as_bytes()) {
        Normalized::Calendar(calendar) => calendar,
        other => panic!("not a calendar: {other:?}"),
    }
}

fn ms(utc: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(utc)
        .unwrap()
        .timestamp_millis()
}

fn utc(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn window(from: &str, to: &str) -> Window {
    Window {
        from: ms(from),
        to: ms(to),
    }
}

fn starts(occurrences: &[Occurrence]) -> Vec<String> {
    occurrences.iter().map(|o| utc(o.start)).collect()
}

fn all(object: &CalendarObject) -> Vec<Occurrence> {
    expand(object, Window::ALL).occurrences
}

#[test]
fn weekly_in_berlin_keeps_its_wall_clock_time_across_daylight_saving() {
    let o = object(
        "BEGIN:VEVENT
UID:standup@example.com
DTSTART;TZID=Europe/Berlin:20250317T090000
DTEND;TZID=Europe/Berlin:20250317T093000
RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=4
SUMMARY:Standup
END:VEVENT
",
    );
    let found = all(&o);
    assert_eq!(
        starts(&found),
        [
            "2025-03-17T08:00:00Z",
            "2025-03-24T08:00:00Z",
            "2025-03-31T07:00:00Z",
            "2025-04-07T07:00:00Z",
        ]
    );
    assert_eq!(found[2].start_local, "2025-03-31T09:00:00");
    assert_eq!(found[2].end_local, "2025-03-31T09:30:00");
    assert_eq!(found[2].end - found[2].start, 30 * 60_000);
    assert_eq!(found[2].time_zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(
        found[2].recurrence_id.as_deref(),
        Some("2025-03-31T07:00:00Z")
    );
    assert!(!found[2].changed && !found[2].all_day && !found[2].floating);
}

#[test]
fn new_york_gaps_move_forward_and_overlaps_take_the_earlier_time() {
    let o = object(
        "BEGIN:VEVENT
UID:night@example.com
DTSTART;TZID=America/New_York:20250308T023000
DURATION:PT1H
RRULE:FREQ=DAILY;COUNT=2
END:VEVENT
BEGIN:VEVENT
UID:night@example.com
RECURRENCE-ID;TZID=America/New_York:20250308T023000
DTSTART;TZID=America/New_York:20251102T013000
DURATION:PT1H
END:VEVENT
",
    );
    let found = all(&o);
    assert_eq!(
        starts(&found),
        ["2025-03-09T07:30:00Z", "2025-11-02T05:30:00Z"],
        "02:30 on 9 March does not exist and reads as 03:30 EDT; 01:30 on 2 November is the first one"
    );
    assert_eq!(found[0].start_local, "2025-03-09T02:30:00");
    assert!(found[1].changed);
    assert_eq!(
        found[1].recurrence_id.as_deref(),
        Some("2025-03-08T07:30:00Z")
    );
}

#[test]
fn last_friday_of_each_month() {
    let o = object(
        "BEGIN:VEVENT
UID:review@example.com
DTSTART;TZID=Europe/Berlin:20250131T160000
DTEND;TZID=Europe/Berlin:20250131T170000
RRULE:FREQ=MONTHLY;BYDAY=-1FR
END:VEVENT
",
    );
    let found = expand(&o, window("2025-02-01T00:00:00Z", "2025-06-01T00:00:00Z"));
    let locals: Vec<&str> = found
        .occurrences
        .iter()
        .map(|o| o.start_local.as_str())
        .collect();
    assert_eq!(
        locals,
        [
            "2025-02-28T16:00:00",
            "2025-03-28T16:00:00",
            "2025-04-25T16:00:00",
            "2025-05-30T16:00:00",
        ]
    );
    assert!(!found.truncated);
    assert_eq!(span(&o).unwrap().end, None, "no COUNT or UNTIL: no end");
}

#[test]
fn until_in_utc_and_in_local_time() {
    let event = |until: &str| {
        object(&format!(
            "BEGIN:VEVENT
UID:u@example.com
DTSTART;TZID=Europe/Berlin:20250317T090000
RRULE:FREQ=WEEKLY;UNTIL={until}
END:VEVENT
"
        ))
    };
    assert_eq!(
        all(&event("20250331T070000Z")).len(),
        3,
        "UNTIL is inclusive"
    );
    assert_eq!(all(&event("20250331T065959Z")).len(), 2);
    assert_eq!(
        all(&event("20250331T090000")).len(),
        3,
        "local UNTIL in the start's zone"
    );
    assert_eq!(all(&event("20250331T085959")).len(), 2);
    assert_eq!(
        all(&event("20250331")).len(),
        3,
        "a date includes its whole day"
    );
    let last = span(&event("20250331T070000Z")).unwrap();
    assert_eq!(
        last,
        Span {
            start: ms("2025-03-17T08:00:00Z"),
            end: Some(ms("2025-03-31T07:00:00Z")),
        }
    );
}

#[test]
fn exdates_in_other_zones_and_count_including_them() {
    let o = object(
        "BEGIN:VEVENT
UID:x@example.com
DTSTART;TZID=Europe/Berlin:20250317T090000
DTEND;TZID=Europe/Berlin:20250317T100000
RRULE:FREQ=WEEKLY;COUNT=5
EXDATE;TZID=W. Europe Standard Time:20250324T090000
EXDATE:20250407T070000Z
EXDATE;TZID=America/New_York:20250414T030000
END:VEVENT
",
    );
    assert_eq!(
        starts(&all(&o)),
        ["2025-03-17T08:00:00Z", "2025-03-31T07:00:00Z"],
        "COUNT counts the excluded instances too (RFC 5545 3.8.5.1)"
    );
}

#[test]
fn an_override_moves_one_instance_and_a_cancelled_one_stays_marked() {
    let o = object(
        "BEGIN:VEVENT
UID:weekly@example.com
SEQUENCE:2
DTSTART;TZID=Europe/Berlin:20250317T090000
DTEND;TZID=Europe/Berlin:20250317T093000
RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=4
SUMMARY:Weekly
END:VEVENT
BEGIN:VEVENT
UID:weekly@example.com
RECURRENCE-ID;TZID=Europe/Berlin:20250324T090000
DTSTART;TZID=Europe/Berlin:20250325T140000
DTEND;TZID=Europe/Berlin:20250325T150000
SUMMARY:Weekly (moved)
END:VEVENT
BEGIN:VEVENT
UID:weekly@example.com
RECURRENCE-ID:20250331T070000Z
DTSTART;TZID=Europe/Berlin:20250331T090000
STATUS:CANCELLED
END:VEVENT
",
    );
    let found = all(&o);
    assert_eq!(
        starts(&found),
        [
            "2025-03-17T08:00:00Z",
            "2025-03-25T13:00:00Z",
            "2025-03-31T07:00:00Z",
            "2025-04-07T07:00:00Z",
        ]
    );
    let moved = &found[1];
    assert_eq!(moved.recurrence_id.as_deref(), Some("2025-03-24T08:00:00Z"));
    assert!(moved.changed);
    assert_eq!(
        o.components[moved.component].summary.as_deref(),
        Some("Weekly (moved)")
    );
    assert_eq!(moved.end - moved.start, 3_600_000);
    assert!(found[2].cancelled && found[2].changed);
    assert!(!found[3].cancelled);
    let march_24 = expand(&o, window("2025-03-24T00:00:00Z", "2025-03-25T00:00:00Z"));
    assert!(
        march_24.occurrences.is_empty(),
        "the moved instance left its old day"
    );
    let march_25 = expand(&o, window("2025-03-25T00:00:00Z", "2025-03-26T00:00:00Z"));
    assert_eq!(march_25.occurrences.len(), 1);
}

#[test]
fn this_and_future_moves_every_later_instance() {
    let o = object(
        "BEGIN:VEVENT
UID:f@example.com
DTSTART:20250303T090000Z
DURATION:PT1H
RRULE:FREQ=WEEKLY;COUNT=4
SUMMARY:Old
END:VEVENT
BEGIN:VEVENT
UID:f@example.com
RECURRENCE-ID;RANGE=THISANDFUTURE:20250310T090000Z
DTSTART:20250310T110000Z
DURATION:PT2H
SUMMARY:New
END:VEVENT
",
    );
    let found = all(&o);
    assert_eq!(
        starts(&found),
        [
            "2025-03-03T09:00:00Z",
            "2025-03-10T11:00:00Z",
            "2025-03-17T11:00:00Z",
            "2025-03-24T11:00:00Z",
        ]
    );
    assert_eq!(
        found[3].recurrence_id.as_deref(),
        Some("2025-03-24T09:00:00Z")
    );
    assert_eq!(
        o.components[found[3].component].summary.as_deref(),
        Some("New")
    );
    assert_eq!(found[3].end - found[3].start, 7_200_000);
}

#[test]
fn all_day_events_stay_on_their_dates_across_daylight_saving() {
    let o = object(
        "BEGIN:VEVENT
UID:d@example.com
DTSTART;VALUE=DATE:20250323
DTEND;VALUE=DATE:20250324
RRULE:FREQ=WEEKLY;COUNT=3
EXDATE;VALUE=DATE:20250330
RDATE;VALUE=DATE:20250402
END:VEVENT
",
    );
    let found = all(&o);
    let days: Vec<(&str, &str)> = found
        .iter()
        .map(|o| (o.start_local.as_str(), o.end_local.as_str()))
        .collect();
    assert_eq!(
        days,
        [
            ("2025-03-23", "2025-03-24"),
            ("2025-04-02", "2025-04-03"),
            ("2025-04-06", "2025-04-07"),
        ]
    );
    assert!(found.iter().all(|o| o.all_day && o.time_zone.is_none()));
    assert_eq!(utc(found[2].start), "2025-04-06T00:00:00Z");
    assert_eq!(found[2].end - found[2].start, 86_400_000);
    assert_eq!(found[2].recurrence_id.as_deref(), Some("2025-04-06"));
}

#[test]
fn floating_times_and_single_events() {
    let o = object(
        "BEGIN:VEVENT
UID:float@example.com
DTSTART:20250601T080000
DTEND:20250601T083000
RRULE:FREQ=DAILY;COUNT=2
END:VEVENT
",
    );
    let found = all(&o);
    assert!(found.iter().all(|o| o.floating));
    assert_eq!(
        starts(&found),
        ["2025-06-01T08:00:00Z", "2025-06-02T08:00:00Z"]
    );
    let single = object(
        "BEGIN:VEVENT
UID:once@example.com
DTSTART;TZID=Europe/Berlin:20250601T080000
DTEND;TZID=Europe/Berlin:20250601T090000
STATUS:CANCELLED
END:VEVENT
",
    );
    let found = all(&single);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].recurrence_id, None);
    assert!(found[0].cancelled);
    assert!(
        expand(
            &single,
            window("2025-06-01T07:00:00Z", "2025-06-02T00:00:00Z")
        )
        .occurrences
        .is_empty()
    );
    assert_eq!(
        expand(
            &single,
            window("2025-06-01T06:30:00Z", "2025-06-01T06:31:00Z")
        )
        .occurrences
        .len(),
        1,
        "an event overlapping the window's start is in it"
    );
}

#[test]
fn custom_vtimezone_rules_are_used_for_unknown_names() {
    let o = object(
        "BEGIN:VTIMEZONE
TZID:Harbour Time
BEGIN:STANDARD
DTSTART:19701025T030000
TZOFFSETFROM:+0200
TZOFFSETTO:+0100
RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:19700329T020000
TZOFFSETFROM:+0100
TZOFFSETTO:+0200
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU
END:DAYLIGHT
END:VTIMEZONE
BEGIN:VEVENT
UID:h@example.com
DTSTART;TZID=Harbour Time:20250324T090000
RRULE:FREQ=WEEKLY;COUNT=2
END:VEVENT
",
    );
    assert_eq!(
        starts(&all(&o)),
        ["2025-03-24T08:00:00Z", "2025-03-31T07:00:00Z"]
    );
}

#[test]
fn endless_rules_reach_far_windows_quickly_and_bounded() {
    let o = object(
        "BEGIN:VEVENT
UID:daily@example.com
DTSTART;TZID=Europe/Berlin:19900101T070000
RRULE:FREQ=DAILY
END:VEVENT
",
    );
    let found = expand(&o, window("2040-01-01T00:00:00Z", "2040-02-01T00:00:00Z"));
    assert_eq!(found.occurrences.len(), 31);
    assert_eq!(utc(found.occurrences[0].start), "2040-01-01T06:00:00Z");
    let minutely = object(
        "BEGIN:VEVENT
UID:tick@example.com
DTSTART:20250101T000000Z
RRULE:FREQ=MINUTELY
END:VEVENT
",
    );
    let found = expand(
        &minutely,
        window("2025-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
    );
    assert!(found.truncated);
    assert_eq!(found.occurrences.len(), holtmail_recur::MAX_OCCURRENCES);
}

#[test]
fn a_changed_instance_without_its_series_is_still_an_instance() {
    let o = object(
        "BEGIN:VEVENT
UID:lone@example.com
RECURRENCE-ID;TZID=Europe/Berlin:20250324T090000
DTSTART;TZID=Europe/Berlin:20250324T100000
DTEND;TZID=Europe/Berlin:20250324T110000
END:VEVENT
",
    );
    let found = all(&o);
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].recurrence_id.as_deref(),
        Some("2025-03-24T08:00:00Z")
    );
    assert_eq!(starts(&found), ["2025-03-24T09:00:00Z"]);
}

#[test]
fn wall_clock_in_named_zones() {
    let at = ms("2025-07-01T07:00:00Z");
    assert_eq!(
        holtmail_recur::wall_clock(at, "Europe/Berlin").as_deref(),
        Some("2025-07-01T09:00:00")
    );
    assert_eq!(
        holtmail_recur::wall_clock(at, "Pacific Standard Time").as_deref(),
        Some("2025-07-01T00:00:00")
    );
    assert_eq!(
        holtmail_recur::wall_clock(at, "+05:30").as_deref(),
        Some("2025-07-01T12:30:00")
    );
    assert_eq!(holtmail_recur::wall_clock(at, "Nowhere"), None);
}

#[test]
fn hostile_rules_end_without_panicking_or_running_away() {
    for (dtend, rule) in [
        ("20250101T093000", "FREQ=WEEKLY;INTERVAL=65535;COUNT=500"),
        ("99991231T090000", "FREQ=DAILY;INTERVAL=65535;COUNT=5000"),
        (
            "20250101T093000",
            "FREQ=YEARLY;COUNT=3;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23;BYMINUTE=0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59",
        ),
    ] {
        let o = object(&format!(
            "BEGIN:VEVENT
UID:hostile@example.com
DTSTART:20250101T090000
DTEND:{dtend}
RRULE:{rule}
SUMMARY:Hostile
END:VEVENT
"
        ));
        let _ = span(&o);
        let _ = all(&o);
    }
}
