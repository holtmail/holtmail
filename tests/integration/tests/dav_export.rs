//! Contacts and calendars exported and read again, through the real binary: an archive synced
//! from Baïkal exports `.vcf` and `.ics`; the files imported into a new archive give the same
//! contacts and instances; and the same files put into Radicale and synced from there give them
//! once more.

use std::collections::BTreeMap;

use holtmail_integration::data::vcard;
use holtmail_integration::dav::{BAIKAL, RADICALE};
use holtmail_integration::pim::{PimRun, line};
use holtmail_integration::{require_it, unique};
use serde_json::Value;

const ZONE: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\n\
TZOFFSETTO:+0200\r\nTZNAME:CEST\r\nDTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\n\
END:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";

fn series(uid: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//Integration tests//EN\r\n{ZONE}\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
         DTSTART;TZID=Europe/Berlin:20260316T090000\r\nDTEND;TZID=Europe/Berlin:20260316T100000\r\n\
         RRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE;TZID=Europe/Berlin:20260330T090000\r\n\
         SUMMARY:Export weekly\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
         RECURRENCE-ID;TZID=Europe/Berlin:20260323T090000\r\n\
         DTSTART;TZID=Europe/Berlin:20260323T160000\r\nDTEND;TZID=Europe/Berlin:20260323T170000\r\n\
         SUMMARY:Export weekly late\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn holiday(uid: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//Integration tests//EN\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART;VALUE=DATE:20260501\r\n\
         DTEND;VALUE=DATE:20260502\r\nSUMMARY:Export holiday\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

/// Contacts of `archive` by title, with their sorted emails and phones.
async fn contacts(run: &PimRun, archive: &str) -> BTreeMap<String, (Vec<String>, Vec<String>)> {
    let sorted = |v: &Value| {
        let mut out: Vec<String> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    };
    run.api
        .items(archive, "contacts")
        .await
        .into_iter()
        .filter(|c| c["removed_at"].is_null())
        .map(|c| {
            (
                c["title"].as_str().unwrap_or_default().to_owned(),
                (sorted(&c["emails"]), sorted(&c["phones"])),
            )
        })
        .collect()
}

/// The instances of `archive` in March to May 2026 as `(title, starts_at, ends_at, all_day)`.
async fn instances(run: &PimRun, archive: &str) -> Vec<(String, i64, i64, bool)> {
    let range = run
        .api
        .calendar(archive, 1_772_323_200_000, 1_780_272_000_000, false)
        .await;
    range["occurrences"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|o| {
            (
                o["title"].as_str().unwrap_or_default().to_owned(),
                o["starts_at"].as_i64().unwrap_or_default(),
                o["ends_at"].as_i64().unwrap_or_default(),
                o["all_day"] == true,
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn exports_reimport_and_go_into_radicale() {
    require_it!();
    let user = BAIKAL.user("davexport").await;
    let dav = BAIKAL.client(&user);
    let homes = BAIKAL.homes(&dav).await;
    let book = dav
        .create_address_book(&homes.contacts, "export", "Export book")
        .await;
    let calendar = dav
        .create_calendar(&homes.calendars, "export", "Export calendar")
        .await;
    for (name, email, phone) in [
        ("Ada Export", "ada.export@example.org", Some("+15550140")),
        ("Bob Export", "bob.export@example.org", None),
        ("Cleo Export", "cleo.export@example.org", Some("+15550141")),
    ] {
        let uid = unique("export");
        dav.put_card(&book, &uid, &vcard(&uid, name, email, phone))
            .await;
    }
    let weekly = unique("weekly");
    dav.put_event(&calendar, &weekly, &series(&weekly)).await;
    let day = unique("holiday");
    dav.put_event(&calendar, &day, &holiday(&day)).await;

    let mut run = PimRun::start("Export source").await;
    run.secrets(
        "a contact",
        &[
            "Ada Export",
            "ada.export@example.org",
            "+15550140",
            "Bob Export",
            "bob.export@example.org",
            "Cleo Export",
            "cleo.export@example.org",
            "+15550141",
        ],
    );
    run.secrets("an event", &["Export weekly", "Export holiday"]);
    run.add_dav(&BAIKAL, &user, None).await;
    let original_contacts = contacts(&run, &run.archive).await;
    let original_instances = instances(&run, &run.archive).await;
    assert_eq!(original_contacts.len(), 3, "{original_contacts:?}");
    assert_eq!(
        original_instances.len(),
        4,
        "three of the series (one excluded) and the holiday: {original_instances:?}"
    );

    let vcf = run
        .api
        .bytes(&format!(
            "/api/archives/{}/contacts/export?format=vcf",
            run.archive
        ))
        .await;
    let ics = run
        .api
        .bytes(&format!(
            "/api/archives/{}/calendar/export?format=ics",
            run.archive
        ))
        .await;
    let vcf_text = String::from_utf8(vcf.clone()).expect("UTF-8");
    let ics_text = String::from_utf8(ics.clone()).expect("UTF-8");
    assert_eq!(vcf_text.matches("BEGIN:VCARD").count(), 3, "{vcf_text}");
    assert_eq!(ics_text.matches("BEGIN:VCALENDAR").count(), 1, "{ics_text}");
    assert_eq!(ics_text.matches("BEGIN:VEVENT").count(), 3, "{ics_text}");

    let copy = run.api.create_archive("Reimported").await;
    let (_, task) = run.api.import(&copy, "contacts.vcf", vcf.clone()).await;
    assert_eq!(task["state"], "done", "{task}");
    let (_, task) = run.api.import(&copy, "calendar.ics", ics.clone()).await;
    assert_eq!(task["state"], "done", "{task}");
    assert_eq!(contacts(&run, &copy).await, original_contacts);
    assert_eq!(instances(&run, &copy).await, original_instances);

    let target = RADICALE.user("davimport").await;
    let radicale = RADICALE.client(&target);
    let homes = RADICALE.homes(&radicale).await;
    let imported_book = format!("{}imported-book/", homes.contacts);
    let imported_calendar = format!("{}imported-calendar/", homes.calendars);
    radicale
        .put(&imported_book, "text/vcard; charset=utf-8", &vcf_text)
        .await;
    radicale
        .put(
            &imported_calendar,
            "text/calendar; charset=utf-8",
            &ics_text,
        )
        .await;
    let (status, back) = radicale.get(&imported_book).await;
    assert!(status.is_success(), "{status}");
    for name in original_contacts.keys() {
        assert!(back.contains(name.as_str()), "{name} in {back}");
    }
    let (status, back) = radicale.get(&imported_calendar).await;
    assert!(status.is_success(), "{status}");
    assert!(back.contains(&weekly) && back.contains(&day), "{back}");

    let third = run.api.create_archive("Radicale copy").await;
    let saved = std::mem::replace(&mut run.archive, third.clone());
    let account = run.add_dav(&RADICALE, &target, None).await;
    let log = run.log(&account).await;
    assert!(
        line(&log, &format!("{target}/imported-book (new)")).contains("3 new"),
        "Radicale names a collection without a display name by its path: {log:?}"
    );
    assert!(
        line(&log, &format!("{target}/imported-calendar (new)")).contains("2 new"),
        "{log:?}"
    );
    assert_eq!(contacts(&run, &third).await, original_contacts);
    assert_eq!(instances(&run, &third).await, original_instances);
    run.archive = saved;
    run.check_logs();
}
