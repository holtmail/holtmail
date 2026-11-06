//! Contacts and calendars without waiting for a mail sync: a JMAP account's cards arrive over
//! its event source (Stalwart), a CardDAV account with its own one-minute interval picks up a
//! change on schedule (Radicale), and a JMAP account whose session offers no calendars reads
//! them over CalDAV while its contacts stay on JMAP.

use std::time::{Duration, Instant};

use holtmail_integration::api::id;
use holtmail_integration::data::{icalendar_event, vcard};
use holtmail_integration::dav::{RADICALE, STALWART_DAV};
use holtmail_integration::pim::PimRun;
use holtmail_integration::redirect::Redirect;
use holtmail_integration::stalwart::{CONTACTS, STALWART_PASSWORD, Seeder};
use holtmail_integration::{Options, require_it, unique};
use serde_json::{Value, json};

fn at(time: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(time)
        .expect("a time")
        .with_timezone(&chrono::Utc)
}

fn watching() -> Options {
    Options {
        watch: true,
        ..Options::default()
    }
}

async fn wait_for_item(run: &PimRun, list: &str, title: &str, within: Duration) -> Duration {
    let started = Instant::now();
    loop {
        if run.items(list).await.contains_key(title) {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < within,
            "{title} not in {list} after {within:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn add_jmap(run: &mut PimRun, url: &str, user: &str) -> String {
    run.secrets("the JMAP user", &[user, STALWART_PASSWORD]);
    let added = run
        .api
        .add_jmap_account(
            &run.archive,
            json!({ "url": url, "username": user, "password": STALWART_PASSWORD }),
        )
        .await
        .ok();
    let account = id(&added);
    run.api.settle(&run.archive, &account).await;
    account
}

#[tokio::test(flavor = "multi_thread")]
async fn jmap_cards_arrive_over_the_event_source() {
    require_it!();
    let user = format!("{}@example.com", unique("pimpush"));
    let jmap = Seeder::new(&user).await.jmap;
    let contacts = jmap.account("urn:ietf:params:jmap:contacts");
    let books = jmap
        .ok(
            CONTACTS,
            "AddressBook/get",
            json!({ "accountId": contacts }),
        )
        .await;
    let book = id(&books["list"][0]);

    let mut run = PimRun::start_with("JMAP contacts push", watching()).await;
    run.secrets("a contact", &["Gil Example", "gil.example@example.org"]);
    let account = add_jmap(
        &mut run,
        &holtmail_integration::stalwart::session_url(),
        &user,
    )
    .await;
    run.api
        .update_account(
            &account,
            json!({ "contacts_calendars": { "enabled": true } }),
        )
        .await
        .ok();
    run.api.wait_for_contacts(&run.archive, &account, 1).await;
    let deadline = Instant::now() + Duration::from_secs(60);
    while run.api.account(&run.archive, &account).await["status"]["contacts_watch"]["state"]
        != "live"
    {
        assert!(
            Instant::now() < deadline,
            "the contacts watch never went live"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    jmap.ok(
        CONTACTS,
        "ContactCard/set",
        json!({ "accountId": contacts, "create": { "c": {
            "@type": "Card",
            "version": "1.0",
            "addressBookIds": { book: true },
            "name": { "full": "Gil Example" },
            "emails": { "e1": { "address": "gil.example@example.org" } },
        } } }),
    )
    .await;
    let took = wait_for_item(&run, "contacts", "Gil Example", Duration::from_secs(30)).await;
    println!("JMAP: a new card archived {took:?} after it was made");
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_carddav_account_syncs_on_its_own_interval() {
    require_it!();
    let server = RADICALE;
    let user = server.user("pimint").await;
    let dav = server.client(&user);
    let homes = server.homes(&dav).await;
    let book = dav
        .create_address_book(&homes.contacts, "own", "Own schedule")
        .await;
    let first = unique("hal");
    dav.put_card(
        &book,
        &first,
        &vcard(&first, "Hal Example", "hal.example@example.org", None),
    )
    .await;

    let mut run = PimRun::start_with("PIM interval", watching()).await;
    run.secrets(
        "a contact",
        &[
            "Hal Example",
            "Ida Example",
            "hal.example@example.org",
            "ida.example@example.org",
        ],
    );
    let account = run.add_dav(&server, &user, None).await;
    let changed = run
        .api
        .update_account(&account, json!({ "contacts_interval_minutes": 1 }))
        .await
        .ok();
    assert_eq!(changed["contacts_calendars"]["interval_minutes"], 1);
    assert_eq!(changed["sync_interval_minutes"], Value::Null);

    let second = unique("ida");
    dav.put_card(
        &book,
        &second,
        &vcard(&second, "Ida Example", "ida.example@example.org", None),
    )
    .await;
    let mail_runs = run.api.task_log("sync", &account).await;
    let took = wait_for_item(&run, "contacts", "Ida Example", Duration::from_secs(150)).await;
    println!("a changed card archived {took:?} later on a one-minute interval");
    assert_eq!(
        run.api.task_log("sync", &account).await,
        mail_runs,
        "no mail sync was needed"
    );
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jmap_account_without_calendars_reads_them_over_caldav() {
    require_it!();
    let user = format!("{}@example.com", unique("nocal"));
    let seeded = Seeder::new(&user).await.jmap;
    let mut session = seeded.session().clone();
    let calendars = "urn:ietf:params:jmap:calendars";
    session["capabilities"]
        .as_object_mut()
        .unwrap()
        .remove(calendars);
    session["primaryAccounts"]
        .as_object_mut()
        .unwrap()
        .remove(calendars);
    for account in session["accounts"].as_object_mut().unwrap().values_mut() {
        if let Some(capabilities) = account["accountCapabilities"].as_object_mut() {
            capabilities.remove(calendars);
        }
    }
    let served = Redirect::json(&session).await;

    let dav = STALWART_DAV.client(&user);
    let homes = STALWART_DAV.homes(&dav).await;
    let plans = dav
        .create_calendar(&homes.calendars, "plans", "Plans")
        .await;
    let event_uid = unique("review");
    dav.put_event(
        &plans,
        &event_uid,
        &icalendar_event(
            &event_uid,
            "Quarterly review",
            at("2026-03-02T10:00:00Z"),
            at("2026-03-02T11:00:00Z"),
            None,
        ),
    )
    .await;

    let mut run = PimRun::start_with("JMAP with CalDAV", watching()).await;
    run.secrets("an event", &["Quarterly review"]);
    let account = add_jmap(&mut run, &format!("{}/jmap/session", served.url()), &user).await;
    let enabled = run
        .api
        .update_account(
            &account,
            json!({ "contacts_calendars": {
                "enabled": true,
                "calendars_url": format!("{}/dav/cal/", STALWART_DAV.url()),
            } }),
        )
        .await
        .ok();
    assert_eq!(
        enabled["contacts_calendars"]["protocol"], "jmap",
        "{enabled}"
    );
    assert_eq!(
        enabled["contacts_calendars"]["calendars_protocol"], "dav",
        "{enabled}"
    );
    let synced = run.api.wait_for_contacts(&run.archive, &account, 1).await;
    assert_eq!(
        synced["status"]["contacts_calendars"]["error"],
        Value::Null,
        "{synced}"
    );
    let collections = run.collections().await;
    assert!(
        collections.contains_key("Plans"),
        "{:?}",
        collections.keys()
    );
    let book = collections
        .values()
        .find(|c| c["kind"] == "contacts")
        .expect("the JMAP address book");
    assert!(book["name"].is_string());
    wait_for_item(&run, "events", "Quarterly review", Duration::from_secs(10)).await;
    run.check_logs();
}
