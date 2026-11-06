//! JMAP mail against Stalwart through the real binary and its API: password and token sign-in,
//! first and incremental syncs, an email in two mailboxes, moves, deletions, keywords, refused
//! credentials, session discovery through `/.well-known/jmap` and redirects.

use std::collections::{BTreeMap, BTreeSet};

use holtmail_integration::api::id;
use holtmail_integration::data::Message;
use holtmail_integration::jmap::Auth;
use holtmail_integration::redirect::Redirect;
use holtmail_integration::stalwart::{self, STALWART_PASSWORD, Seeder};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it};
use serde_json::{Value, json};

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
        let archive = api.create_archive("JMAP").await;
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

    fn seeded(&mut self, messages: &[&Message]) {
        let scanner = std::mem::take(&mut self.scanner);
        self.scanner = scanner
            .secrets("a subject", messages.iter().map(|m| m.subject.as_str()))
            .secrets("a sender", messages.iter().map(|m| m.from.as_str()));
    }

    fn secret(&mut self, what: &str, value: &str) {
        self.scanner = std::mem::take(&mut self.scanner).secret(what, value);
    }

    async fn add(&self, account: Value) -> String {
        let added = self.api.add_jmap_account(&self.archive, account).await.ok();
        let account = id(&added);
        let settled = self.api.settle(&self.archive, &account).await;
        assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
        account
    }

    async fn sync_clean(&self, account: &str) {
        let synced = self.api.sync(&self.archive, account).await;
        assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    }

    async fn by_subject(&self) -> BTreeMap<String, Value> {
        self.api
            .messages(&self.archive)
            .await
            .into_iter()
            .map(|m| (m["subject"].as_str().unwrap_or_default().to_owned(), m))
            .collect()
    }

    async fn folders(&self) -> BTreeMap<String, Value> {
        self.api
            .folders(&self.archive)
            .await
            .into_iter()
            .map(|f| (f["path"].as_str().unwrap_or_default().to_owned(), f))
            .collect()
    }

    async fn path_of_role(&self, role: &str) -> String {
        self.folders()
            .await
            .into_iter()
            .find(|(_, f)| f["role"] == role)
            .map(|(path, _)| path)
            .unwrap_or_else(|| panic!("no folder with the role {role}"))
    }

    async fn locations(&self, subject: &str) -> Vec<(String, bool)> {
        let paths: BTreeMap<String, String> = self
            .folders()
            .await
            .into_iter()
            .map(|(path, f)| (id(&f), path))
            .collect();
        let message = &self.by_subject().await[subject];
        let detail = self.api.message(&id(message)).await;
        let mut out: Vec<(String, bool)> = detail["locations"]
            .as_array()
            .expect("locations")
            .iter()
            .map(|l| {
                (
                    paths[l["folder_id"].as_str().expect("a folder id")].clone(),
                    l["removed_at"].is_null(),
                )
            })
            .collect();
        out.sort();
        out
    }

    fn check_logs(&self) {
        self.scanner.check(&self.holtmail.logs());
    }
}

fn flags(message: &Value) -> BTreeSet<String> {
    message["flags"]
        .as_array()
        .map(|f| {
            f.iter()
                .filter_map(|f| f.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn set(flags: &[&str]) -> BTreeSet<String> {
    flags.iter().map(|f| (*f).to_owned()).collect()
}

fn password_account(url: &str, user: &str) -> Value {
    json!({ "url": url, "username": user, "password": STALWART_PASSWORD })
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_mail_with_a_password_found_through_well_known() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("jmap"));
    let seeder = Seeder::new(&user).await;
    let inbox = seeder.role("inbox").await;
    let projects = seeder.create_mailbox("Projects", None).await;
    let reports = seeder.create_mailbox("Reports", Some(&projects)).await;
    let read = Message::new(&user);
    let twice = Message::new(&user);
    let flagged = Message::new(&user);
    let read_id = seeder.import(&read.bytes(), &[&inbox], &["$seen"]).await;
    let twice_id = seeder
        .import(&twice.bytes(), &[&inbox, &projects], &[])
        .await;
    let flagged_id = seeder
        .import(&flagged.bytes(), &[&reports], &["$flagged", "$forwarded"])
        .await;

    let mut run = Run::start(&user).await;
    run.seeded(&[&read, &twice, &flagged]);
    let account = run
        .add(password_account(&stalwart::base_url(), &user))
        .await;
    let listed = run.api.account(&run.archive, &account).await;
    assert_eq!(
        listed["jmap"]["url"],
        format!("{}/.well-known/jmap", stalwart::base_url())
    );
    assert_eq!(listed["kind"], "jmap");

    let messages = run.by_subject().await;
    assert_eq!(messages.len(), 3, "{:?}", messages.keys());
    assert_eq!(
        messages[&twice.subject]["folder_ids"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(flags(&messages[&read.subject]), set(&["\\Seen"]));
    assert_eq!(
        flags(&messages[&flagged.subject]),
        set(&["\\Flagged", "$forwarded"])
    );
    assert_eq!(flags(&messages[&twice.subject]), set(&[]));
    assert_eq!(
        run.api.raw(&id(&messages[&flagged.subject])).await,
        flagged.bytes()
    );
    let inbox_path = run.path_of_role("inbox").await;
    let folders = run.folders().await;
    assert!(
        folders.contains_key("Projects/Reports"),
        "{:?}",
        folders.keys()
    );
    assert_eq!(folders[&inbox_path]["messages"], 2);
    assert_eq!(folders[&inbox_path]["unread"], 1);
    assert_eq!(folders["Projects"]["messages"], 1);

    let later: Vec<Message> = (0..2).map(|_| Message::new(&user)).collect();
    for message in &later {
        seeder.import(&message.bytes(), &[&inbox], &[]).await;
    }
    run.seeded(&later.iter().collect::<Vec<_>>());
    seeder
        .update(
            &read_id,
            json!({ "mailboxIds": { projects.clone(): true } }),
        )
        .await;
    seeder.destroy(&flagged_id).await;
    seeder
        .update(
            &twice_id,
            json!({ "keywords/$seen": true, "keywords/$flagged": true }),
        )
        .await;
    run.sync_clean(&account).await;

    let log = run.api.sync_log(&account).await;
    let inbox_line = log
        .iter()
        .find(|l| l.starts_with(&format!("{inbox_path}: ")))
        .unwrap_or_else(|| panic!("no line for {inbox_path} in {log:?}"));
    assert!(
        inbox_line.starts_with(&format!("{inbox_path}: 2 new, 0 known, 1 removed")),
        "{log:?}"
    );
    let messages = run.by_subject().await;
    assert_eq!(messages.len(), 5, "deleted mail stays in the archive");
    assert_eq!(
        run.locations(&read.subject).await,
        vec![(inbox_path.clone(), false), ("Projects".to_owned(), true)]
    );
    assert_eq!(
        run.locations(&flagged.subject).await,
        vec![("Projects/Reports".to_owned(), false)]
    );
    assert_eq!(
        flags(&messages[&twice.subject]),
        set(&[]),
        "keywords are read once a day"
    );

    run.holtmail.age_flag_checks(25).await;
    run.sync_clean(&account).await;
    let messages = run.by_subject().await;
    assert_eq!(
        flags(&messages[&twice.subject]),
        set(&["\\Seen", "\\Flagged"])
    );
    assert_eq!(flags(&messages[&read.subject]), set(&["\\Seen"]));
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_mail_with_an_api_token() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("token"));
    let seeder = Seeder::new(&user).await;
    let inbox = seeder.role("inbox").await;
    let message = Message::new(&user);
    seeder.import(&message.bytes(), &[&inbox], &[]).await;
    let token = stalwart::api_key(&user).await;
    let by_token = Seeder::with(Auth::Bearer(token.clone())).await;
    assert_eq!(by_token.role("inbox").await, inbox);

    let mut run = Run::start(&user).await;
    run.seeded(&[&message]);
    run.secret("the API token", &token);
    let account = run
        .add(json!({ "url": stalwart::session_url(), "auth": "token", "password": token }))
        .await;
    let listed = run.api.account(&run.archive, &account).await;
    assert_eq!(listed["jmap"]["auth"], "token");
    assert_eq!(listed["jmap"]["username"], "");
    assert!(run.by_subject().await.contains_key(&message.subject));

    let later = Message::new(&user);
    seeder.import(&later.bytes(), &[&inbox], &[]).await;
    run.seeded(&[&later]);
    run.sync_clean(&account).await;
    assert_eq!(run.by_subject().await.len(), 2);
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_wrong_credentials() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("wrong"));
    Seeder::new(&user).await;
    let mut run = Run::start(&user).await;
    run.secret("a wrong password", "it-wrong-password");
    run.secret("a wrong token", "API_it-wrong-token");

    let wrong_password = run
        .api
        .add_jmap_account(
            &run.archive,
            json!({ "url": stalwart::base_url(), "username": user, "password": "it-wrong-password" }),
        )
        .await;
    assert_eq!(wrong_password.status, 400, "{}", wrong_password.body);
    assert!(
        wrong_password.body["error"]
            .as_str()
            .unwrap_or_default()
            .starts_with("The server refused this user name and password."),
        "{}",
        wrong_password.body
    );
    let unknown = run
        .api
        .add_jmap_account(
            &run.archive,
            password_account(&stalwart::base_url(), "nobody-here@example.com"),
        )
        .await;
    assert_eq!(unknown.status, 400, "{}", unknown.body);
    let wrong_token = run
        .api
        .add_jmap_account(
            &run.archive,
            json!({ "url": stalwart::session_url(), "auth": "token", "password": "API_it-wrong-token" }),
        )
        .await;
    assert_eq!(wrong_token.status, 400, "{}", wrong_token.body);
    assert!(
        wrong_token.body["error"]
            .as_str()
            .unwrap_or_default()
            .starts_with("The server refused this token."),
        "{}",
        wrong_token.body
    );
    assert!(run.api.accounts(&run.archive).await.is_empty());

    let account = run
        .add(password_account(&stalwart::base_url(), &user))
        .await;
    let refused = run
        .api
        .update_account(&account, json!({ "password": "it-wrong-password" }))
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    run.sync_clean(&account).await;
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_a_well_known_redirect_to_another_host_without_credentials() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("redirect"));
    let seeder = Seeder::new(&user).await;
    let inbox = seeder.role("inbox").await;
    let message = Message::new(&user);
    seeder.import(&message.bytes(), &[&inbox], &[]).await;
    let second = Redirect::start(&stalwart::session_url()).await;
    let first = Redirect::start(&format!("{}/jmap/session", second.url())).await;

    let mut run = Run::start(&user).await;
    run.seeded(&[&message]);
    let account = run.add(password_account(&first.url(), &user)).await;
    assert!(run.by_subject().await.contains_key(&message.subject));
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
    let synced = run.api.sync_contacts(&run.archive, &account).await;
    assert_eq!(
        synced["status"]["contacts_calendars"]["error"],
        Value::Null,
        "{synced}"
    );
    for (name, server) in [("first", &first), ("second", &second)] {
        let asked = server.asked();
        assert!(!asked.is_empty(), "the {name} host was not asked");
        assert!(
            asked.iter().all(|a| !a.authorized),
            "credentials went to the {name} host: {asked:?}"
        );
    }
    assert!(first.asked().iter().all(|a| a.path == "/.well-known/jmap"));
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_follow_a_session_redirect_to_another_host_with_credentials() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("offorigin"));
    Seeder::new(&user).await;
    let elsewhere = Redirect::start(&stalwart::session_url()).await;
    let typed = Redirect::start(&format!("{}/jmap/session", elsewhere.url())).await;

    let run = Run::start(&user).await;
    let refused = run
        .api
        .add_jmap_account(
            &run.archive,
            password_account(&format!("{}/jmap/session", typed.url()), &user),
        )
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(
        refused.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("redirect"),
        "{}",
        refused.body
    );
    assert!(
        typed.asked().iter().all(|a| a.path == "/jmap/session"),
        "{:?}",
        typed.asked()
    );
    assert!(
        elsewhere.asked().iter().all(|a| !a.authorized),
        "credentials followed the redirect: {:?}",
        elsewhere.asked()
    );
    assert!(run.api.accounts(&run.archive).await.is_empty());
    run.check_logs();
}
