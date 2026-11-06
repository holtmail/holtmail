//! CardDAV and CalDAV accounts against Radicale, Baïkal and Stalwart (and Nextcloud with its
//! profile), through the real binary and its API: discovery through the well-known addresses,
//! first sync, unchanged and incremental syncs, changed, removed and new items, a removed
//! collection, and a sync token the server does not know (Radicale and Baïkal refuse it and the
//! collection is read in full; Stalwart answers an unknown token with every item).

use holtmail_integration::data::{icalendar_event, vcard};
use holtmail_integration::dav::{BAIKAL, DavServer, NEXTCLOUD, RADICALE, STALWART_DAV};
use holtmail_integration::pim::{PimRun, line};
use holtmail_integration::{require_it, unique};

fn at(day: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(day)
        .expect("a time")
        .with_timezone(&chrono::Utc)
}

async fn syncs_changes_and_removals(server: DavServer) {
    let user = server.user("dav").await;
    let dav = server.client(&user);
    let homes = server.homes(&dav).await;
    let family = dav
        .create_address_book(&homes.contacts, "family", "Family")
        .await;
    let work = dav
        .create_address_book(&homes.contacts, "work", "Work")
        .await;
    let plans = dav
        .create_calendar(&homes.calendars, "plans", "Plans")
        .await;
    let ada_uid = unique("ada");
    let ada = dav
        .put_card(
            &family,
            &ada_uid,
            &vcard(&ada_uid, "Ada Example", "ada.example@example.org", None),
        )
        .await;
    let bob_uid = unique("bob");
    let bob = dav
        .put_card(
            &family,
            &bob_uid,
            &vcard(
                &bob_uid,
                "Bob Example",
                "bob.example@example.org",
                Some("+15550100"),
            ),
        )
        .await;
    let cleo_uid = unique("cleo");
    dav.put_card(
        &work,
        &cleo_uid,
        &vcard(&cleo_uid, "Cleo Example", "cleo.example@example.org", None),
    )
    .await;
    let planning_uid = unique("planning");
    let planning = dav
        .put_event(
            &plans,
            &planning_uid,
            &icalendar_event(
                &planning_uid,
                "Planning meeting",
                at("2026-12-01T09:00:00Z"),
                at("2026-12-01T10:00:00Z"),
                None,
            ),
        )
        .await;
    let standup_uid = unique("standup");
    dav.put_event(
        &plans,
        &standup_uid,
        &icalendar_event(
            &standup_uid,
            "Weekly standup",
            at("2026-12-07T08:30:00Z"),
            at("2026-12-07T08:45:00Z"),
            Some("FREQ=WEEKLY;COUNT=4"),
        ),
    )
    .await;

    let mut run = PimRun::start(&format!("DAV on {}", server.name)).await;
    run.secrets(
        "a contact",
        &[
            "Ada Example",
            "Ada Sample",
            "ada.example@example.org",
            "Bob Example",
            "bob.example@example.org",
            "+15550100",
            "Cleo Example",
            "cleo.example@example.org",
            "Dana Example",
            "dana.example@example.org",
            "Eve Example",
            "eve.example@example.org",
        ],
    );
    run.secrets(
        "an event",
        &["Planning meeting", "Weekly standup", "Review meeting"],
    );
    let account = run.add_dav(&server, &user, None).await;

    let log = run.log(&account).await;
    let collections = run.collections().await;
    for (name, kind, items) in [
        ("Family", "contacts", 2),
        ("Work", "contacts", 1),
        ("Plans", "calendar", 2),
    ] {
        let collection = &collections
            .get(name)
            .unwrap_or_else(|| panic!("no {name} in {:?}", collections.keys()));
        assert_eq!(collection["kind"], kind, "{collection}");
        assert_eq!(collection["items"], items, "{collection}");
        assert!(
            line(&log, name).ends_with(&format!(
                "{items} new, 0 changed, 0 unchanged, 0 removed, 0 failed (read in full)"
            )),
            "{log:?}"
        );
        assert!(line(&log, name).starts_with(&format!("{name} (new):")));
    }
    let contacts = run.items("contacts").await;
    assert!(contacts.values().all(|c| c["readable"] == true));
    assert!(
        contacts["Bob Example"]["emails"]
            .as_array()
            .is_some_and(|e| e.contains(&serde_json::json!("bob.example@example.org"))),
        "{}",
        contacts["Bob Example"]
    );
    let events = run.items("events").await;
    assert_eq!(
        events["Planning meeting"]["starts_at"],
        1_796_115_600_000_i64
    );
    assert_eq!(events["Weekly standup"]["recurring"], true);

    let unchanged = run.sync(&account).await;
    for name in ["Family", "Work", "Plans"] {
        assert!(
            line(&unchanged, name).ends_with("0 removed, 0 failed (unchanged)"),
            "{unchanged:?}"
        );
    }

    dav.put_card(
        &family,
        &ada_uid,
        &vcard(&ada_uid, "Ada Sample", "ada.example@example.org", None),
    )
    .await;
    assert!(ada.ends_with(".vcf"));
    dav.delete(&bob).await;
    let dana_uid = unique("dana");
    dav.put_card(
        &family,
        &dana_uid,
        &vcard(&dana_uid, "Dana Example", "dana.example@example.org", None),
    )
    .await;
    dav.delete(&planning).await;
    let review_uid = unique("review");
    dav.put_event(
        &plans,
        &review_uid,
        &icalendar_event(
            &review_uid,
            "Review meeting",
            at("2026-12-02T14:30:00Z"),
            at("2026-12-02T15:00:00Z"),
            None,
        ),
    )
    .await;
    dav.delete(&work).await;
    let log = run.sync(&account).await;
    assert!(
        line(&log, "Family").ends_with(
            "1 new, 1 changed, 0 unchanged, 1 removed, 0 failed (changes since the last sync)"
        ),
        "{log:?}"
    );
    assert!(
        line(&log, "Plans").ends_with(
            "1 new, 0 changed, 0 unchanged, 1 removed, 0 failed (changes since the last sync)"
        ),
        "{log:?}"
    );
    assert!(
        log.iter().any(
            |l| l == "1 address books or calendars are gone from the server and marked removed"
        ),
        "{log:?}"
    );
    let contacts = run.items("contacts").await;
    assert_eq!(contacts["Ada Sample"]["versions"], 2);
    assert!(contacts["Ada Sample"]["removed_at"].is_null());
    assert!(contacts["Bob Example"]["removed_at"].is_i64());
    assert!(contacts["Dana Example"]["removed_at"].is_null());
    assert!(
        contacts["Cleo Example"]["removed_at"].is_i64(),
        "kept in the archive with its address book"
    );
    let events = run.items("events").await;
    assert!(events["Planning meeting"]["removed_at"].is_i64());
    assert!(events["Review meeting"]["removed_at"].is_null());
    let collections = run.collections().await;
    assert!(collections["Work"]["removed_at"].is_i64());
    assert_eq!(collections["Work"]["removed_items"], 1);
    assert!(collections["Family"]["removed_at"].is_null());

    let eve_uid = unique("eve");
    dav.put_card(
        &family,
        &eve_uid,
        &vcard(&eve_uid, "Eve Example", "eve.example@example.org", None),
    )
    .await;
    run.holtmail.rewrite_collection_tokens("szzzzzz").await;
    let log = run.sync(&account).await;
    let mode = if server.is_stalwart() {
        "changes since the last sync"
    } else {
        "read in full, the server forgot the last sync"
    };
    assert!(
        line(&log, "Family").ends_with(&format!(
            "1 new, 0 changed, 0 unchanged, 0 removed, 0 failed ({mode})"
        )),
        "{log:?}"
    );
    assert!(
        line(&log, "Plans").ends_with(&format!(
            "0 new, 0 changed, 0 unchanged, 0 removed, 0 failed ({mode})"
        )),
        "{log:?}"
    );
    let contacts = run.items("contacts").await;
    assert!(contacts["Eve Example"]["removed_at"].is_null());
    assert_eq!(
        contacts["Ada Sample"]["versions"], 2,
        "an unchanged card read again is not a new version"
    );
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_changes_and_removals_on_radicale() {
    require_it!();
    syncs_changes_and_removals(RADICALE).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_changes_and_removals_on_baikal() {
    require_it!();
    syncs_changes_and_removals(BAIKAL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_changes_and_removals_on_stalwart() {
    require_it!();
    syncs_changes_and_removals(STALWART_DAV).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_changes_and_removals_on_nextcloud() {
    require_it!();
    if !holtmail_integration::profile("nextcloud") {
        eprintln!("skipped: needs IT_PROFILES=nextcloud");
        return;
    }
    syncs_changes_and_removals(NEXTCLOUD).await;
}
