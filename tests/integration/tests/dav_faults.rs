//! CardDAV and CalDAV failures and fallbacks through the real binary: a server without
//! `sync-collection` (Baïkal behind a proxy in the test process that refuses it), a server
//! answering `500`, a wrong password, broken vCards and iCalendar objects, and a well-known
//! address on another host that leads to the DAV server.

use holtmail_integration::data::{icalendar_event, vcard};
use holtmail_integration::dav::{BAIKAL, DavServer, RADICALE, baikal_insert};
use holtmail_integration::pim::{PimRun, line};
use holtmail_integration::proxy::{Mode, Proxy};
use holtmail_integration::redirect::Redirect;
use holtmail_integration::{require_it, unique};

fn at(day: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(day)
        .expect("a time")
        .with_timezone(&chrono::Utc)
}

struct Seeded {
    user: String,
    dav: holtmail_integration::dav::Dav,
    family: String,
}

async fn seed(server: &DavServer, names: &[&str]) -> Seeded {
    let user = server.user("davf").await;
    let dav = server.client(&user);
    let homes = server.homes(&dav).await;
    let family = dav
        .create_address_book(&homes.contacts, "family", "Family")
        .await;
    let plans = dav
        .create_calendar(&homes.calendars, "plans", "Plans")
        .await;
    for name in names {
        let uid = unique("card");
        let email = format!(
            "{}@example.org",
            name.to_ascii_lowercase().replace(' ', ".")
        );
        dav.put_card(&family, &uid, &vcard(&uid, name, &email, None))
            .await;
    }
    let uid = unique("event");
    dav.put_event(
        &plans,
        &uid,
        &icalendar_event(
            &uid,
            "Budget review",
            at("2026-12-03T13:00:00Z"),
            at("2026-12-03T14:00:00Z"),
            None,
        ),
    )
    .await;
    Seeded { user, dav, family }
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_by_etags_when_the_server_has_no_sync_collection() {
    require_it!();
    let seeded = seed(&BAIKAL, &["Ada Example", "Bob Example"]).await;
    let proxy = Proxy::start(&BAIKAL.url()).await;
    proxy.set(Mode::NoSync);
    let mut run = PimRun::start("No sync-collection").await;
    run.secrets("a contact", &["Ada Example", "Bob Example", "Cleo Example"]);
    let account = run
        .add_dav(
            &BAIKAL,
            &seeded.user,
            Some(&format!("{}/dav.php/", proxy.url())),
        )
        .await;
    let log = run.log(&account).await;
    assert!(
        line(&log, "Family").ends_with(
            "2 new, 0 changed, 0 unchanged, 0 removed, 0 failed (listed in full, the server cannot list changes)"
        ),
        "{log:?}"
    );
    assert!(
        proxy
            .seen()
            .iter()
            .any(|(request, passed)| request.starts_with("REPORT") && !passed),
        "sync-collection was asked and refused"
    );

    let log = run.sync(&account).await;
    assert!(line(&log, "Family").ends_with("(unchanged)"), "{log:?}");

    let ada = run.items("contacts").await["Ada Example"].clone();
    let uid = ada["uid"].as_str().expect("a UID").to_owned();
    seeded
        .dav
        .put_card(
            &seeded.family,
            &uid,
            &vcard(&uid, "Ada Sample", "ada.example@example.org", None),
        )
        .await;
    let bob = run.items("contacts").await["Bob Example"]["uid"]
        .as_str()
        .expect("a UID")
        .to_owned();
    seeded
        .dav
        .delete(&format!("{}{bob}.vcf", seeded.family))
        .await;
    let cleo = unique("cleo");
    seeded
        .dav
        .put_card(
            &seeded.family,
            &cleo,
            &vcard(&cleo, "Cleo Example", "cleo.example@example.org", None),
        )
        .await;
    let before = proxy.seen().len();
    let log = run.sync(&account).await;
    assert!(
        line(&log, "Family").ends_with(
            "1 new, 1 changed, 0 unchanged, 1 removed, 0 failed (listed in full, the server cannot list changes)"
        ),
        "{log:?}"
    );
    assert!(line(&log, "Plans").ends_with("(unchanged)"), "{log:?}");
    let contacts = run.items("contacts").await;
    assert_eq!(contacts["Ada Sample"]["versions"], 2);
    assert!(contacts["Bob Example"]["removed_at"].is_i64());
    assert!(contacts["Cleo Example"]["removed_at"].is_null());
    let multigets = proxy.seen()[before..]
        .iter()
        .filter(|(r, passed)| r.starts_with("REPORT") && *passed)
        .count();
    assert_eq!(
        multigets, 1,
        "only the two new tags are fetched, in one multiget"
    );
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_error_fails_the_run_and_the_next_sync_recovers() {
    require_it!();
    let seeded = seed(&BAIKAL, &["Ada Example"]).await;
    let proxy = Proxy::start(&BAIKAL.url()).await;
    let mut run = PimRun::start("Server errors").await;
    run.secrets("a contact", &["Ada Example", "Bob Example"]);
    let account = run
        .add_dav(
            &BAIKAL,
            &seeded.user,
            Some(&format!("{}/dav.php/", proxy.url())),
        )
        .await;
    assert_eq!(run.collections().await["Family"]["items"], 1);

    proxy.set(Mode::Status(500));
    let failed = run.api.sync_contacts(&run.archive, &account).await;
    let error = failed["status"]["contacts_calendars"]["error"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(error.contains("answered 500"), "{failed}");
    let contacts = run.items("contacts").await;
    assert!(
        contacts["Ada Example"]["removed_at"].is_null(),
        "a failing server removes nothing"
    );

    let bob = unique("bob");
    seeded
        .dav
        .put_card(
            &seeded.family,
            &bob,
            &vcard(&bob, "Bob Example", "bob.example@example.org", None),
        )
        .await;
    proxy.set(Mode::Pass);
    let log = run.sync(&account).await;
    assert!(
        line(&log, "Family").ends_with(
            "1 new, 0 changed, 0 unchanged, 0 removed, 0 failed (changes since the last sync)"
        ),
        "{log:?}"
    );
    let contacts = run.items("contacts").await;
    assert!(contacts["Ada Example"]["removed_at"].is_null());
    assert!(contacts["Bob Example"]["removed_at"].is_null());
    run.check_logs();
}

async fn refuses_a_wrong_password(server: DavServer) {
    let user = server.imap.user("davpw");
    server.add_user(&user, "it-other-dav-password").await;
    let mut run = PimRun::start("Wrong password").await;
    run.secrets("the DAV password", &["it-other-dav-password"]);
    let account = run.add_mail(&server, &user).await;
    let refused = run.api.enable_contacts(&account, Some(&server.url())).await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(
        refused.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("refused the sign-in"),
        "{}",
        refused.body
    );
    let listed = run.api.account(&run.archive, &account).await;
    assert_eq!(listed["contacts_calendars"]["enabled"], false, "{listed}");
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_wrong_password_on_radicale() {
    require_it!();
    refuses_a_wrong_password(RADICALE).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_wrong_password_on_baikal() {
    require_it!();
    refuses_a_wrong_password(BAIKAL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stores_broken_cards_and_events_raw_as_unreadable() {
    require_it!();
    let seeded = seed(&BAIKAL, &["Ada Example"]).await;
    let homes = BAIKAL.homes(&seeded.dav).await;
    let broken_book = seeded
        .dav
        .create_address_book(&homes.contacts, "broken", "Broken cards")
        .await;
    let broken_calendar = seeded
        .dav
        .create_calendar(&homes.calendars, "broken-events", "Broken events")
        .await;
    let card_uid = unique("brokencard");
    seeded
        .dav
        .put_card(
            &broken_book,
            &card_uid,
            &vcard(&card_uid, "Faye Example", "faye.example@example.org", None),
        )
        .await;
    let event_uid = unique("brokenevent");
    seeded
        .dav
        .put_event(
            &broken_calendar,
            &event_uid,
            &icalendar_event(
                &event_uid,
                "Garbled meeting",
                at("2026-12-04T09:00:00Z"),
                at("2026-12-04T10:00:00Z"),
                None,
            ),
        )
        .await;
    let proxy = Proxy::start(&BAIKAL.url()).await;
    proxy.set(Mode::Rewrite {
        path: "/broken".into(),
        from: "BEGIN:V".into(),
        to: "BEGIN:BROKEN-V".into(),
    });
    let mut run = PimRun::start("Broken items").await;
    run.secrets(
        "a contact",
        &["Ada Example", "Faye Example", "faye.example@example.org"],
    );
    run.secrets("an event", &["Garbled meeting", "Budget review"]);
    let account = run
        .add_dav(
            &BAIKAL,
            &seeded.user,
            Some(&format!("{}/dav.php/", proxy.url())),
        )
        .await;
    let log = run.log(&account).await;
    for name in ["Broken cards", "Broken events", "Family", "Plans"] {
        assert!(
            line(&log, name)
                .ends_with("1 new, 0 changed, 0 unchanged, 0 removed, 0 failed (read in full)"),
            "{log:?}"
        );
    }
    let collections = run.collections().await;
    for (name, uid) in [("Broken cards", &card_uid), ("Broken events", &event_uid)] {
        let collection = collections[name]["id"].as_str().expect("an id");
        let ids = run.holtmail.item_ids(collection).await;
        assert_eq!(ids.len(), 1, "{name}: {ids:?}");
        let item = run.api.get(&format!("/api/items/{}", ids[0])).await.ok();
        assert_eq!(item["item"]["readable"], false, "{item}");
        let raw = run
            .api
            .bytes(&format!("/api/items/{}/versions/1/raw", ids[0]))
            .await;
        let raw = String::from_utf8(raw).expect("UTF-8");
        assert!(
            raw.starts_with("BEGIN:BROKEN-V"),
            "kept as the server sent it: {raw}"
        );
        assert!(raw.contains(uid.as_str()), "{raw}");
    }
    let readable = run.items("contacts").await;
    assert_eq!(readable["Ada Example"]["readable"], true);
    let second = run.sync(&account).await;
    assert!(
        line(&second, "Broken cards").ends_with("(unchanged)"),
        "{second:?}"
    );
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_card_the_server_cannot_send_leaves_the_others() {
    require_it!();
    let seeded = seed(&BAIKAL, &["Ada Example", "Bob Example"]).await;
    let uid = unique("mangled");
    baikal_insert(
        &seeded.family,
        &format!("{uid}.vcf"),
        &format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN Mangled Example\r\nEND:VCARD\r\n"),
    );
    let mut run = PimRun::start("Server-side broken card").await;
    run.secrets(
        "a contact",
        &["Ada Example", "Bob Example", "Mangled Example"],
    );
    let account = run.add_dav(&BAIKAL, &seeded.user, None).await;
    let log = run.log(&account).await;
    assert!(
        line(&log, "Family (new)")
            .ends_with("2 new, 0 changed, 0 unchanged, 0 removed, 1 failed (read in full)"),
        "{log:?}"
    );
    assert!(line(&log, "Plans (new)").contains("1 new"), "{log:?}");
    let contacts = run.items("contacts").await;
    assert!(contacts.contains_key("Ada Example"), "{log:?}");
    assert!(contacts.contains_key("Bob Example"), "{log:?}");

    let log = run.sync(&account).await;
    assert!(
        line(&log, "Family")
            .ends_with("0 new, 0 changed, 0 unchanged, 0 removed, 1 failed (read in full)"),
        "asked again, in full, while the server cannot send it: {log:?}"
    );
    assert!(line(&log, "Plans").ends_with("(unchanged)"), "{log:?}");
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_a_well_known_redirect_to_the_dav_host_without_credentials() {
    require_it!();
    let seeded = seed(&BAIKAL, &["Ada Example"]).await;
    let domain = Redirect::start(&format!("{}/dav.php", BAIKAL.url())).await;
    let mut run = PimRun::start("Well-known elsewhere").await;
    run.secrets("a contact", &["Ada Example"]);
    let account = run
        .add_dav(&BAIKAL, &seeded.user, Some(&domain.url()))
        .await;
    let log = run.log(&account).await;
    assert!(line(&log, "Family (new)").contains("1 new"), "{log:?}");
    let asked = domain.asked();
    assert!(
        asked
            .iter()
            .all(|a| a.path.starts_with("/.well-known/") && !a.authorized),
        "only the well-known addresses, without credentials: {asked:?}"
    );
    assert!(asked.iter().any(|a| a.path == "/.well-known/carddav"));
    run.check_logs();
}
