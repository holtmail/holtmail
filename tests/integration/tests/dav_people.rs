//! Copies across servers through the real binary: the same event (by `UID`) in calendars on
//! Radicale, Baïkal and Stalwart folds into one event with three copies, and cards with the same
//! email in address books on all three join one person, who also has mail in a Dovecot mailbox.

use std::time::{Duration, Instant};

use holtmail_integration::data::{Message, vcard};
use holtmail_integration::dav::{BAIKAL, DavServer, RADICALE, STALWART_DAV};
use holtmail_integration::pim::PimRun;
use holtmail_integration::{require_it, unique};
use serde_json::Value;

const GRACE: &str = "grace.example@example.org";

async fn seed(server: &DavServer, card_name: &str, phone: Option<&str>, event_uid: &str) -> String {
    let user = server.user("davpeople").await;
    let dav = server.client(&user);
    let homes = server.homes(&dav).await;
    let book = dav
        .create_address_book(
            &homes.contacts,
            "people",
            &format!("People on {}", server.name),
        )
        .await;
    let calendar = dav
        .create_calendar(
            &homes.calendars,
            "meetings",
            &format!("Meetings on {}", server.name),
        )
        .await;
    let uid = unique("grace");
    dav.put_card(&book, &uid, &vcard(&uid, card_name, GRACE, phone))
        .await;
    let other = unique("other");
    dav.put_card(
        &book,
        &other,
        &vcard(
            &other,
            &format!("Other {}", server.name),
            &format!("{other}@example.org"),
            None,
        ),
    )
    .await;
    let start = chrono::DateTime::parse_from_rfc3339("2026-12-10T15:00:00Z")
        .expect("a time")
        .with_timezone(&chrono::Utc);
    dav.put_event(
        &calendar,
        event_uid,
        &holtmail_integration::data::icalendar_event(
            event_uid,
            "Quarterly sync",
            start,
            start + chrono::Duration::hours(1),
            None,
        ),
    )
    .await;
    user
}

#[tokio::test(flavor = "multi_thread")]
async fn copies_on_three_servers_fold_into_one_event_and_one_person() {
    require_it!();
    let event_uid = unique("quarterly");
    let radicale = seed(&RADICALE, "Grace Example", Some("+15550123"), &event_uid).await;
    let baikal = seed(&BAIKAL, "Grace E.", None, &event_uid).await;
    let stalwart = seed(&STALWART_DAV, "Grace Example", None, &event_uid).await;
    let mut mailbox = RADICALE.imap.seeder(&radicale).await;
    for subject in ["Grace's first note", "Grace's second note"] {
        let mut message = Message::new(&radicale).subject(subject);
        message.from = GRACE.into();
        message.from_name = "Grace Example".into();
        mailbox.append("INBOX", &[], None, &message.bytes()).await;
    }

    let mut run = PimRun::start("Across servers").await;
    run.secrets(
        "a contact",
        &[GRACE, "Grace Example", "Grace E.", "+15550123"],
    );
    run.secrets(
        "mail or an event",
        &[
            "Quarterly sync",
            "Grace's first note",
            "Grace's second note",
        ],
    );
    for (server, user) in [
        (&RADICALE, &radicale),
        (&BAIKAL, &baikal),
        (&STALWART_DAV, &stalwart),
    ] {
        run.add_dav(server, user, None).await;
    }

    let shown = run
        .occurrences("2026-12-10T00:00:00Z", "2026-12-11T00:00:00Z")
        .await;
    let quarterly: Vec<&Value> = shown
        .iter()
        .filter(|o| o["title"] == "Quarterly sync")
        .collect();
    assert_eq!(quarterly.len(), 1, "one event: {shown:#?}");
    assert_eq!(quarterly[0]["copies"], 3, "{shown:#?}");
    assert_eq!(
        quarterly[0]["collection_ids"].as_array().map(Vec::len),
        Some(3)
    );
    let event = run
        .api
        .get(&format!(
            "/api/events/{}",
            quarterly[0]["group_id"].as_str().expect("a group")
        ))
        .await
        .ok();
    assert!(event.is_object(), "{event}");

    let deadline = Instant::now() + Duration::from_secs(60);
    let grace = loop {
        let people = run.api.people(&run.archive).await;
        let found: Vec<Value> = people
            .into_iter()
            .filter(|p| {
                p["emails"]
                    .as_array()
                    .is_some_and(|e| e.iter().any(|a| a == GRACE))
            })
            .collect();
        if found.len() == 1 && found[0]["cards"] == 3 && found[0]["messages"] == 2 {
            break found[0].clone();
        }
        assert!(
            Instant::now() < deadline,
            "Grace is not one person with three cards and two messages: {found:#?}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert_eq!(
        grace["collection_ids"].as_array().map(Vec::len),
        Some(3),
        "{grace}"
    );
    assert_eq!(grace["differences"], true, "the names differ: {grace}");
    let person = run
        .api
        .get(&format!(
            "/api/people/{}",
            grace["id"].as_str().expect("an id")
        ))
        .await
        .ok();
    assert!(person.to_string().contains("+15550123"), "{person}");
    run.check_logs();
}
