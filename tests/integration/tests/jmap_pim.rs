//! Contacts and calendars of a JMAP account against Stalwart (RFC 9610 and JMAP Calendars),
//! through the real binary and its API: first sync, changes, removals, a removed address book,
//! a state the server cannot calculate changes from, and an API address on another host than
//! the session.

use std::collections::BTreeMap;

use holtmail_integration::api::id;
use holtmail_integration::jmap::Jmap;
use holtmail_integration::redirect::Redirect;
use holtmail_integration::stalwart::{self, CALENDARS, CONTACTS, STALWART_PASSWORD, Seeder};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it};
use serde_json::{Value, json};

struct Pim {
    jmap: Jmap,
    contacts: String,
    calendars: String,
}

impl Pim {
    async fn new(user: &str) -> Self {
        let jmap = Seeder::new(user).await.jmap;
        let contacts = jmap.account("urn:ietf:params:jmap:contacts");
        let calendars = jmap.account("urn:ietf:params:jmap:calendars");
        Self {
            jmap,
            contacts,
            calendars,
        }
    }

    async fn default_book(&self) -> String {
        let books = self
            .jmap
            .ok(
                CONTACTS,
                "AddressBook/get",
                json!({ "accountId": self.contacts }),
            )
            .await;
        id(&books["list"][0])
    }

    async fn default_calendar(&self) -> String {
        let calendars = self
            .jmap
            .ok(
                CALENDARS,
                "Calendar/get",
                json!({ "accountId": self.calendars }),
            )
            .await;
        id(&calendars["list"][0])
    }

    async fn create_book(&self, name: &str) -> String {
        let created = self
            .jmap
            .ok(
                CONTACTS,
                "AddressBook/set",
                json!({ "accountId": self.contacts, "create": { "b": { "name": name } } }),
            )
            .await;
        id(&created["created"]["b"])
    }

    async fn destroy_book(&self, book: &str) {
        self.jmap
            .ok(
                CONTACTS,
                "AddressBook/set",
                json!({ "accountId": self.contacts, "destroy": [book], "onDestroyRemoveContents": true }),
            )
            .await;
    }

    async fn card(&self, book: &str, name: &str, email: &str) -> String {
        let created = self
            .jmap
            .ok(
                CONTACTS,
                "ContactCard/set",
                json!({ "accountId": self.contacts, "create": { "c": {
                    "@type": "Card",
                    "version": "1.0",
                    "addressBookIds": { book: true },
                    "name": { "full": name },
                    "emails": { "e1": { "address": email } },
                } } }),
            )
            .await;
        id(&created["created"]["c"])
    }

    async fn update_card(&self, card: &str, patch: Value) {
        self.jmap
            .ok(
                CONTACTS,
                "ContactCard/set",
                json!({ "accountId": self.contacts, "update": { card: patch } }),
            )
            .await;
    }

    async fn destroy_card(&self, card: &str) {
        self.jmap
            .ok(
                CONTACTS,
                "ContactCard/set",
                json!({ "accountId": self.contacts, "destroy": [card] }),
            )
            .await;
    }

    async fn event(&self, calendar: &str, event: Value) -> String {
        let mut event = event;
        event["@type"] = json!("Event");
        event["calendarIds"] = json!({ calendar: true });
        let created = self
            .jmap
            .ok(
                CALENDARS,
                "CalendarEvent/set",
                json!({ "accountId": self.calendars, "create": { "e": event } }),
            )
            .await;
        id(&created["created"]["e"])
    }

    async fn destroy_event(&self, event: &str) {
        self.jmap
            .ok(
                CALENDARS,
                "CalendarEvent/set",
                json!({ "accountId": self.calendars, "destroy": [event] }),
            )
            .await;
    }
}

struct Run {
    holtmail: Holtmail,
    api: Api,
    archive: String,
    scanner: LogScanner,
}

impl Run {
    async fn start(user: &str) -> Self {
        let holtmail = Holtmail::start(Options::default()).await;
        let api = Api::setup_owner(&holtmail).await;
        let archive = api.create_archive("JMAP contacts").await;
        let scanner = LogScanner::new()
            .secret("the JMAP password", STALWART_PASSWORD)
            .secret("the JMAP user", user);
        Self {
            holtmail,
            api,
            archive,
            scanner,
        }
    }

    fn secrets(&mut self, what: &str, values: &[&str]) {
        self.scanner = std::mem::take(&mut self.scanner).secrets(what, values.iter().copied());
    }

    async fn add(&self, url: &str, user: &str) -> String {
        let added = self
            .api
            .add_jmap_account(
                &self.archive,
                json!({ "url": url, "username": user, "password": STALWART_PASSWORD }),
            )
            .await
            .ok();
        let account = id(&added);
        self.api.settle(&self.archive, &account).await;
        account
    }

    async fn sync(&self, account: &str) -> Vec<String> {
        let synced = self.api.sync_contacts(&self.archive, account).await;
        assert_eq!(
            synced["status"]["contacts_calendars"]["error"],
            Value::Null,
            "{synced}"
        );
        self.api.task_log("contacts_calendars", account).await
    }

    async fn items(&self, list: &str) -> BTreeMap<String, Value> {
        self.api
            .items(&self.archive, list)
            .await
            .into_iter()
            .map(|i| (i["title"].as_str().unwrap_or_default().to_owned(), i))
            .collect()
    }

    async fn collections(&self) -> BTreeMap<String, Value> {
        self.api
            .collections(&self.archive)
            .await
            .into_iter()
            .filter(|c| c["account_id"].is_string())
            .map(|c| (c["name"].as_str().unwrap_or_default().to_owned(), c))
            .collect()
    }
}

fn line<'a>(log: &'a [String], collection: &str) -> &'a str {
    log.iter()
        .find(|l| l.starts_with(collection))
        .unwrap_or_else(|| panic!("no line for {collection} in {log:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_contacts_and_calendars_through_jmap() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("pim"));
    let pim = Pim::new(&user).await;
    let book = pim.default_book().await;
    let calendar = pim.default_calendar().await;
    let friends = pim.create_book("Friends").await;
    let ada = pim
        .card(&book, "Ada Example", "ada.example@example.org")
        .await;
    let bob = pim
        .card(&book, "Bob Example", "bob.example@example.org")
        .await;
    pim.card(&friends, "Cleo Example", "cleo.example@example.org")
        .await;
    let meeting = pim
        .event(
            &calendar,
            json!({
                "title": "Planning meeting",
                "start": "2026-12-01T10:00:00",
                "timeZone": "Europe/Berlin",
                "duration": "PT1H",
            }),
        )
        .await;
    pim.event(
        &calendar,
        json!({
            "title": "Team holiday",
            "start": "2026-12-24T00:00:00",
            "showWithoutTime": true,
            "duration": "P1D",
        }),
    )
    .await;

    let mut run = Run::start(&user).await;
    run.secrets(
        "a contact",
        &[
            "Ada Example",
            "ada.example@example.org",
            "Bob Example",
            "bob.example@example.org",
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
        &[
            "Planning meeting",
            "Team holiday",
            "Review meeting",
            "Ada Sample",
        ],
    );
    let account = run.add(&stalwart::base_url(), &user).await;
    let enabled = run
        .api
        .update_account(
            &account,
            json!({ "contacts_calendars": { "enabled": true } }),
        )
        .await
        .ok();
    assert_eq!(
        enabled["contacts_calendars"]["protocol"], "jmap",
        "{enabled}"
    );

    let first = run.api.wait_for_contacts(&run.archive, &account, 1).await;
    assert_eq!(
        first["status"]["contacts_calendars"]["error"],
        Value::Null,
        "{first}"
    );
    let log = run.api.task_log("contacts_calendars", &account).await;
    let collections = run.collections().await;
    assert_eq!(collections.len(), 3, "{:?}", collections.keys());
    assert_eq!(collections["Friends"]["kind"], "contacts");
    assert_eq!(collections["Friends"]["items"], 1);
    let calendars: Vec<&Value> = collections
        .values()
        .filter(|c| c["kind"] == "calendar")
        .collect();
    assert_eq!(calendars.len(), 1);
    assert_eq!(calendars[0]["items"], 2);
    assert!(
        line(&log, "Friends (new)")
            .contains("1 new, 0 changed, 0 unchanged, 0 removed, 0 failed (read in full)"),
        "{log:?}"
    );
    let contacts = run.items("contacts").await;
    assert_eq!(contacts.len(), 3, "{:?}", contacts.keys());
    assert!(contacts.values().all(|c| c["readable"] == true));
    assert!(
        contacts["Ada Example"]["emails"]
            .as_array()
            .is_some_and(|e| e.contains(&json!("ada.example@example.org"))),
        "{}",
        contacts["Ada Example"]
    );
    let events = run.items("events").await;
    assert_eq!(events.len(), 2, "{:?}", events.keys());
    let planning = &events["Planning meeting"];
    assert_eq!(planning["time_zone"], "Europe/Berlin");
    assert_eq!(planning["starts_at"], 1_796_115_600_000_i64);
    assert_eq!(planning["ends_at"], 1_796_119_200_000_i64);
    assert_eq!(events["Team holiday"]["all_day"], true);

    let unchanged = run.sync(&account).await;
    assert!(
        unchanged
            .iter()
            .filter(|l| l.contains(": "))
            .all(|l| l.ends_with("0 removed, 0 failed (unchanged)")),
        "{unchanged:?}"
    );

    pim.update_card(&ada, json!({ "name": { "full": "Ada Sample" } }))
        .await;
    pim.destroy_card(&bob).await;
    pim.card(&book, "Dana Example", "dana.example@example.org")
        .await;
    pim.destroy_event(&meeting).await;
    pim.event(
        &calendar,
        json!({
            "title": "Review meeting",
            "start": "2026-12-02T15:30:00",
            "timeZone": "America/New_York",
            "duration": "PT30M",
        }),
    )
    .await;
    pim.destroy_book(&friends).await;
    let log = run.sync(&account).await;
    assert!(
        log.iter()
            .any(|l| l.contains("address books or calendars are gone from the server")),
        "{log:?}"
    );
    let contacts = run.items("contacts").await;
    assert!(contacts.contains_key("Ada Sample"), "{:?}", contacts.keys());
    assert_eq!(contacts["Ada Sample"]["versions"], 2);
    assert!(contacts["Bob Example"]["removed_at"].is_i64());
    assert!(contacts["Dana Example"]["removed_at"].is_null());
    let events = run.items("events").await;
    assert!(events["Planning meeting"]["removed_at"].is_i64());
    assert!(events["Review meeting"]["removed_at"].is_null());
    assert_eq!(events["Review meeting"]["time_zone"], "America/New_York");
    let collections = run.collections().await;
    assert!(collections["Friends"]["removed_at"].is_i64());
    assert_eq!(collections["Friends"]["removed_items"], 1);
    assert!(
        contacts["Cleo Example"]["removed_at"].is_i64(),
        "kept in the archive"
    );

    pim.card(&book, "Eve Example", "eve.example@example.org")
        .await;
    run.holtmail.rewrite_collection_tokens("szzzzzz").await;
    let log = run.sync(&account).await;
    let expired: Vec<&String> = log
        .iter()
        .filter(|l| l.ends_with("(read in full, the server forgot the last sync)"))
        .collect();
    assert_eq!(expired.len(), 2, "{log:?}");
    assert!(
        expired
            .iter()
            .any(|l| l.contains("1 new, 0 changed, 2 unchanged, 0 removed")),
        "{log:?}"
    );
    assert!(
        expired
            .iter()
            .any(|l| l.contains("0 new, 0 changed, 2 unchanged, 0 removed")),
        "{log:?}"
    );
    let contacts = run.items("contacts").await;
    assert!(contacts.contains_key("Eve Example"));
    assert_eq!(
        contacts["Ada Sample"]["versions"], 2,
        "an unchanged card read again is not a new version"
    );
    run.scanner.check(&run.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn uses_an_api_address_on_another_host_the_session_names() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("apiurl"));
    let pim = Pim::new(&user).await;
    let book = pim.default_book().await;
    pim.card(&book, "Fay Example", "fay.example@example.org")
        .await;
    let session = Redirect::json(pim.jmap.session()).await;
    let typed = format!("{}/jmap/session", session.url());

    let mut run = Run::start(&user).await;
    run.secrets("a contact", &["Fay Example", "fay.example@example.org"]);
    let account = run.add(&typed, &user).await;
    let listed = run.api.account(&run.archive, &account).await;
    assert_eq!(listed["status"]["error"], Value::Null, "{listed}");
    let enabled = run
        .api
        .update_account(
            &account,
            json!({ "contacts_calendars": { "enabled": true } }),
        )
        .await
        .ok();
    assert_eq!(
        enabled["contacts_calendars"]["protocol"], "jmap",
        "{enabled}"
    );
    let synced = run.api.wait_for_contacts(&run.archive, &account, 1).await;
    assert_eq!(
        synced["status"]["contacts_calendars"]["error"],
        Value::Null,
        "{synced}"
    );
    let contacts = run.items("contacts").await;
    assert!(
        contacts.contains_key("Fay Example"),
        "{:?}",
        contacts.keys()
    );
    let asked = session.asked();
    assert!(
        asked.iter().all(|a| a.path == "/jmap/session"),
        "only the session is asked where the typed address leads: {asked:?}"
    );
    run.scanner.check(&run.holtmail.logs());
}
