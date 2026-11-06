//! Calendars from Radicale, Baïkal and Stalwart (and Nextcloud with its profile) through the real binary, checked through the
//! calendar API: a weekly series across the change to summer time with a moved and an excluded
//! instance, a series in a zone with a made-up name defined only by its `VTIMEZONE`, one in a
//! Windows zone name, and a recurring all-day event.

use holtmail_integration::dav::{BAIKAL, DavServer, NEXTCLOUD, RADICALE, STALWART_DAV};
use holtmail_integration::pim::PimRun;
use holtmail_integration::{require_it, unique};
use serde_json::Value;

const BERLIN: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\n\
TZOFFSETTO:+0200\r\nTZNAME:CEST\r\nDTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\n\
END:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";

const CUSTOM: &str = "BEGIN:VTIMEZONE\r\nTZID:Holtmail Eastern Test\r\nBEGIN:STANDARD\r\n\
DTSTART:19701101T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nTZOFFSETFROM:-0400\r\n\
TZOFFSETTO:-0500\r\nTZNAME:EST\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:19700308T020000\r\n\
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nTZNAME:EDT\r\n\
END:DAYLIGHT\r\nEND:VTIMEZONE\r\n";

const WINDOWS: &str = "BEGIN:VTIMEZONE\r\nTZID:W. Europe Standard Time\r\nBEGIN:STANDARD\r\n\
DTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\n\
DTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";

fn calendar(zone: &str, events: &[String]) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//Integration tests//EN\r\n{zone}{}END:VCALENDAR\r\n",
        events.concat()
    )
}

fn event(uid: &str, lines: &[&str]) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n{}\r\nEND:VEVENT\r\n",
        lines.join("\r\n")
    )
}

/// `(title, start in UTC, end in UTC, changed)` of the shown instances of the calendar
/// `collection`, by start.
fn instances(occurrences: &[Value], collection: &Value) -> Vec<(String, String, String, bool)> {
    let utc = |v: &Value| {
        chrono::DateTime::from_timestamp_millis(v.as_i64().expect("a time"))
            .expect("a valid time")
            .format("%Y-%m-%dT%H:%MZ")
            .to_string()
    };
    occurrences
        .iter()
        .filter(|o| {
            &o["collection_id"] == collection && o["removed"] == false && o["cancelled"] == false
        })
        .map(|o| {
            (
                o["title"].as_str().unwrap_or_default().to_owned(),
                utc(&o["starts_at"]),
                utc(&o["ends_at"]),
                o["changed"] == true,
            )
        })
        .collect()
}

fn expect(rows: &[(&str, &str, &str, bool)]) -> Vec<(String, String, String, bool)> {
    rows.iter()
        .map(|(t, s, e, c)| ((*t).to_owned(), (*s).to_owned(), (*e).to_owned(), *c))
        .collect()
}

async fn shows_the_right_instances(server: DavServer) {
    let user = server.user("davcal").await;
    let dav = server.client(&user);
    let homes = server.homes(&dav).await;
    let seasons = dav
        .create_calendar(&homes.calendars, "seasons", "Seasons")
        .await;
    let berlin = unique("berlin");
    dav.put_event(
        &seasons,
        &berlin,
        &calendar(
            BERLIN,
            &[
                event(
                    &berlin,
                    &[
                        "DTSTART;TZID=Europe/Berlin:20260316T090000",
                        "DTEND;TZID=Europe/Berlin:20260316T100000",
                        "RRULE:FREQ=WEEKLY;COUNT=6",
                        "EXDATE;TZID=Europe/Berlin:20260406T090000",
                        "SUMMARY:Berlin weekly",
                    ],
                ),
                event(
                    &berlin,
                    &[
                        "RECURRENCE-ID;TZID=Europe/Berlin:20260323T090000",
                        "DTSTART;TZID=Europe/Berlin:20260324T140000",
                        "DTEND;TZID=Europe/Berlin:20260324T150000",
                        "SUMMARY:Berlin weekly moved",
                    ],
                ),
            ],
        ),
    )
    .await;
    let custom = unique("custom");
    dav.put_event(
        &seasons,
        &custom,
        &calendar(
            CUSTOM,
            &[event(
                &custom,
                &[
                    "DTSTART;TZID=Holtmail Eastern Test:20261026T100000",
                    "DTEND;TZID=Holtmail Eastern Test:20261026T103000",
                    "RRULE:FREQ=WEEKLY;COUNT=3",
                    "SUMMARY:Eastern weekly",
                ],
            )],
        ),
    )
    .await;
    let windows = unique("windows");
    dav.put_event(
        &seasons,
        &windows,
        &calendar(
            WINDOWS,
            &[event(
                &windows,
                &[
                    "DTSTART;TZID=W. Europe Standard Time:20261019T090000",
                    "DTEND;TZID=W. Europe Standard Time:20261019T091500",
                    "RRULE:FREQ=WEEKLY;COUNT=2",
                    "SUMMARY:Windows weekly",
                ],
            )],
        ),
    )
    .await;
    let holiday = unique("holiday");
    dav.put_event(
        &seasons,
        &holiday,
        &calendar(
            "",
            &[event(
                &holiday,
                &[
                    "DTSTART;VALUE=DATE:20261224",
                    "DTEND;VALUE=DATE:20261226",
                    "RRULE:FREQ=YEARLY;COUNT=2",
                    "SUMMARY:Winter holiday",
                ],
            )],
        ),
    )
    .await;

    let mut run = PimRun::start(&format!("Calendar on {}", server.name)).await;
    run.secrets(
        "an event",
        &[
            "Berlin weekly",
            "Eastern weekly",
            "Windows weekly",
            "Winter holiday",
        ],
    );
    let account = run.add_dav(&server, &user, None).await;
    let log = run.log(&account).await;
    assert!(
        holtmail_integration::pim::line(&log, "Seasons (new)").contains("4 new"),
        "{log:?}"
    );
    let events = run.items("events").await;
    assert!(events.values().all(|e| e["readable"] == true), "{events:?}");
    assert_eq!(events["Winter holiday"]["all_day"], true);
    let seasons = run.collections().await["Seasons"]["id"].clone();

    let spring = run
        .occurrences("2026-03-01T00:00:00Z", "2026-05-01T00:00:00Z")
        .await;
    assert_eq!(
        instances(&spring, &seasons),
        expect(&[
            (
                "Berlin weekly",
                "2026-03-16T08:00Z",
                "2026-03-16T09:00Z",
                false
            ),
            (
                "Berlin weekly moved",
                "2026-03-24T13:00Z",
                "2026-03-24T14:00Z",
                true
            ),
            (
                "Berlin weekly",
                "2026-03-30T07:00Z",
                "2026-03-30T08:00Z",
                false
            ),
            (
                "Berlin weekly",
                "2026-04-13T07:00Z",
                "2026-04-13T08:00Z",
                false
            ),
            (
                "Berlin weekly",
                "2026-04-20T07:00Z",
                "2026-04-20T08:00Z",
                false
            ),
        ]),
        "{spring:#?}"
    );
    let berlin_first = spring
        .iter()
        .find(|o| o["title"] == "Berlin weekly")
        .expect("an instance");
    assert_eq!(berlin_first["time_zone"], "Europe/Berlin");
    assert_eq!(berlin_first["start_local"], "2026-03-16T09:00:00");

    let autumn = run
        .occurrences("2026-10-01T00:00:00Z", "2027-01-01T00:00:00Z")
        .await;
    assert_eq!(
        instances(&autumn, &seasons),
        expect(&[
            (
                "Windows weekly",
                "2026-10-19T07:00Z",
                "2026-10-19T07:15Z",
                false
            ),
            (
                "Windows weekly",
                "2026-10-26T08:00Z",
                "2026-10-26T08:15Z",
                false
            ),
            (
                "Eastern weekly",
                "2026-10-26T14:00Z",
                "2026-10-26T14:30Z",
                false
            ),
            (
                "Eastern weekly",
                "2026-11-02T15:00Z",
                "2026-11-02T15:30Z",
                false
            ),
            (
                "Eastern weekly",
                "2026-11-09T15:00Z",
                "2026-11-09T15:30Z",
                false
            ),
            (
                "Winter holiday",
                "2026-12-24T00:00Z",
                "2026-12-26T00:00Z",
                false
            ),
        ]),
        "{autumn:#?}"
    );
    let holiday = autumn
        .iter()
        .find(|o| o["title"] == "Winter holiday")
        .expect("the holiday");
    assert_eq!(holiday["all_day"], true);
    assert_eq!(holiday["start_local"], "2026-12-24");
    let eastern = autumn
        .iter()
        .find(|o| o["title"] == "Eastern weekly")
        .expect("an instance");
    assert_eq!(eastern["start_local"], "2026-10-26T10:00:00");

    let next_year = run
        .occurrences("2027-12-01T00:00:00Z", "2028-01-01T00:00:00Z")
        .await;
    assert_eq!(
        instances(&next_year, &seasons),
        expect(&[(
            "Winter holiday",
            "2027-12-24T00:00Z",
            "2027-12-26T00:00Z",
            false
        )]),
        "{next_year:#?}"
    );
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_the_right_instances_from_radicale() {
    require_it!();
    shows_the_right_instances(RADICALE).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_the_right_instances_from_baikal() {
    require_it!();
    shows_the_right_instances(BAIKAL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_the_right_instances_from_stalwart() {
    require_it!();
    shows_the_right_instances(STALWART_DAV).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_the_right_instances_from_nextcloud() {
    require_it!();
    if !holtmail_integration::profile("nextcloud") {
        eprintln!("skipped: needs IT_PROFILES=nextcloud");
        return;
    }
    shows_the_right_instances(NEXTCLOUD).await;
}
