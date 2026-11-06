//! IMAP against Dovecot (CONDSTORE, special-use) and GreenMail (neither): first and incremental
//! syncs, folder names, removals and moves, renumbering, flags, minimum age, TLS and connection
//! changes, through the real binary and its API.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Duration as Age;
use holtmail_integration::api::id;
use holtmail_integration::data::{Message, now};
use holtmail_integration::services::{
    DOVECOT, DOVECOT_IMAP, DOVECOT_IMAPS, GREENMAIL, GREENMAIL_IMAPS, ImapServer, STALWART,
};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it};
use serde_json::{Value, json};

struct Run {
    holtmail: Holtmail,
    api: Api,
    archive: String,
    scanner: LogScanner,
}

impl Run {
    async fn start(options: Options, server: ImapServer, user: &str) -> Self {
        let holtmail = Holtmail::start(options).await;
        let api = Api::setup_owner(&holtmail).await;
        let archive = api.create_archive("IMAP").await;
        let scanner = LogScanner::new()
            .secret("the IMAP password", server.password)
            .secret("the IMAP user", user);
        Self {
            holtmail,
            api,
            archive,
            scanner,
        }
    }

    fn seeded(&mut self, messages: &[Message]) {
        let scanner = std::mem::take(&mut self.scanner);
        self.scanner = scanner
            .secrets("a subject", messages.iter().map(|m| m.subject.as_str()))
            .secrets("a sender", messages.iter().map(|m| m.from.as_str()));
    }

    async fn add(&self, server: ImapServer, user: &str) -> String {
        let account = self.api.add_imap(&self.archive, server.account(user)).await;
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

    async fn folder_ids(&self) -> BTreeMap<String, Value> {
        self.api
            .folders(&self.archive)
            .await
            .into_iter()
            .map(|f| (f["path"].as_str().unwrap_or_default().to_owned(), f))
            .collect()
    }

    async fn locations(&self, subject: &str) -> Vec<(String, bool)> {
        let folders: BTreeMap<String, String> = self
            .folder_ids()
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
                    folders[l["folder_id"].as_str().expect("a folder id")].clone(),
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

fn line<'a>(log: &'a [String], folder: &str) -> &'a str {
    log.iter()
        .find(|l| l.starts_with(&format!("{folder}: ")))
        .unwrap_or_else(|| panic!("no line for {folder} in {log:?}"))
}

async fn first_and_incremental(server: ImapServer) {
    let user = server.user("first");
    let inbox: Vec<Message> = (0..3).map(|_| Message::new(&user)).collect();
    let reports = Message::new(&user);
    let russian = Message::new(&user);
    let sent = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    for folder in ["Projects/Reports", "Projects/Отчёты", "Archive", "Sent"] {
        let _ = seeder
            .command(&format!(
                "CREATE {}",
                holtmail_integration::imap::quote(&server.folder(folder))
            ))
            .await;
    }
    for message in &inbox {
        seeder
            .append("INBOX", &["\\Seen"], None, &message.bytes())
            .await;
    }
    seeder.append("Archive", &[], None, &inbox[0].bytes()).await;
    seeder
        .append(
            &server.folder("Projects/Reports"),
            &[],
            None,
            &reports.bytes(),
        )
        .await;
    seeder
        .append(
            &server.folder("Projects/Отчёты"),
            &[],
            None,
            &russian.bytes(),
        )
        .await;
    seeder
        .append("Sent", &["\\Seen"], None, &sent.bytes())
        .await;

    let mut run = Run::start(Options::default(), server, &user).await;
    let all: Vec<Message> = inbox
        .iter()
        .chain([&reports, &russian, &sent])
        .cloned()
        .collect();
    run.seeded(&all);
    let account = run.add(server, &user).await;

    let messages = run.by_subject().await;
    assert_eq!(messages.len(), 6, "{:?}", messages.keys());
    let twice = &messages[&inbox[0].subject];
    assert_eq!(twice["folder_ids"].as_array().map(Vec::len), Some(2));
    let folders = run.folder_ids().await;
    for path in [
        "INBOX",
        "Archive",
        "Projects/Reports",
        "Projects/Отчёты",
        "Sent",
    ] {
        assert!(folders.contains_key(path), "{path} in {:?}", folders.keys());
    }
    assert_eq!(folders["INBOX"]["role"], "inbox");
    assert_eq!(folders["Sent"]["role"], "sent");
    assert_eq!(folders["Archive"]["role"], "archive");
    assert_eq!(folders["INBOX"]["messages"], 3);
    assert_eq!(folders["INBOX"]["unread"], 0);
    assert_eq!(folders["Projects/Отчёты"]["messages"], 1);
    assert_eq!(folders["Projects/Отчёты"]["name"], "Отчёты");

    let later: Vec<Message> = (0..2).map(|_| Message::new(&user)).collect();
    let nested = Message::new(&user);
    for message in &later {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    seeder
        .append(
            &server.folder("Projects/Reports"),
            &[],
            None,
            &nested.bytes(),
        )
        .await;
    seeder.logout().await;
    run.seeded(&later);
    run.seeded(std::slice::from_ref(&nested));
    run.sync_clean(&account).await;

    let log = run.api.sync_log(&account).await;
    assert!(
        line(&log, "INBOX").starts_with("INBOX: 2 new, 0 known, 0 removed"),
        "{log:?}"
    );
    assert!(
        line(&log, "Projects/Reports").starts_with("Projects/Reports: 1 new, 0 known"),
        "{log:?}"
    );
    assert!(
        line(&log, "Archive").starts_with("Archive: 0 new, 0 known"),
        "{log:?}"
    );
    assert_eq!(run.by_subject().await.len(), 9);
    assert_eq!(run.folder_ids().await["INBOX"]["unread"], 2);
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn first_and_incremental_sync_on_dovecot() {
    require_it!();
    first_and_incremental(DOVECOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_and_incremental_sync_on_greenmail() {
    require_it!();
    first_and_incremental(GREENMAIL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_and_incremental_sync_on_stalwart() {
    require_it!();
    first_and_incremental(STALWART).await;
}

async fn expunge_and_move(server: ImapServer) {
    let user = server.user("moves");
    let messages: Vec<Message> = (0..3).map(|_| Message::new(&user)).collect();
    let mut seeder = server.seeder(&user).await;
    seeder.create("Work").await;
    for message in &messages {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    let mut run = Run::start(Options::default(), server, &user).await;
    run.seeded(&messages);
    let account = run.add(server, &user).await;
    assert_eq!(run.by_subject().await.len(), 3);

    seeder.ok("SELECT INBOX").await;
    seeder.ok("STORE 1 +FLAGS (\\Deleted)").await;
    seeder.ok("EXPUNGE").await;
    seeder.ok("MOVE 1 Work").await;
    seeder.logout().await;
    run.sync_clean(&account).await;

    assert_eq!(run.by_subject().await.len(), 3);
    assert_eq!(
        run.locations(&messages[0].subject).await,
        vec![("INBOX".to_owned(), false)]
    );
    assert_eq!(
        run.locations(&messages[1].subject).await,
        vec![("INBOX".to_owned(), false), ("Work".to_owned(), true)]
    );
    assert_eq!(
        run.locations(&messages[2].subject).await,
        vec![("INBOX".to_owned(), true)]
    );
    let folders = run.folder_ids().await;
    assert_eq!(folders["INBOX"]["messages"], 1);
    assert_eq!(folders["Work"]["messages"], 1);
    let log = run.api.sync_log(&account).await;
    assert!(line(&log, "INBOX").contains("2 removed"), "{log:?}");
    assert!(
        line(&log, "Work").starts_with("Work: 0 new, 1 known"),
        "{log:?}"
    );
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_expunged_mail_and_follows_moves_on_dovecot() {
    require_it!();
    expunge_and_move(DOVECOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_expunged_mail_and_follows_moves_on_greenmail() {
    require_it!();
    expunge_and_move(GREENMAIL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_expunged_mail_and_follows_moves_on_stalwart() {
    require_it!();
    expunge_and_move(STALWART).await;
}

async fn renumbering(server: ImapServer) {
    let user = server.user("renumber");
    let messages: Vec<Message> = (0..3).map(|_| Message::new(&user)).collect();
    let extra = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    seeder.create("Renumber").await;
    for message in &messages {
        seeder.append("Renumber", &[], None, &message.bytes()).await;
    }
    let before = seeder.status("Renumber", "UIDVALIDITY").await;
    let mut run = Run::start(Options::default(), server, &user).await;
    run.seeded(&messages);
    run.seeded(std::slice::from_ref(&extra));
    let account = run.add(server, &user).await;

    seeder.ok("DELETE Renumber").await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    seeder.create("Renumber").await;
    seeder.append("Renumber", &[], None, &extra.bytes()).await;
    for message in &messages {
        seeder.append("Renumber", &[], None, &message.bytes()).await;
    }
    let after = seeder.status("Renumber", "UIDVALIDITY").await;
    seeder.logout().await;
    assert_ne!(before, after, "the server kept UIDVALIDITY");
    run.sync_clean(&account).await;

    let log = run.api.sync_log(&account).await;
    let renumbered = line(&log, "Renumber");
    assert!(
        renumbered.starts_with("Renumber: 1 new, 3 known, 0 removed")
            && renumbered.ends_with("renumbered by the server"),
        "{log:?}"
    );
    assert_eq!(run.by_subject().await.len(), 4);
    assert_eq!(run.folder_ids().await["Renumber"]["messages"], 4);
    run.sync_clean(&account).await;
    let log = run.api.sync_log(&account).await;
    assert!(
        line(&log, "Renumber").starts_with("Renumber: 0 new, 0 known, 0 removed, 0 changed"),
        "{log:?}"
    );
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn renumbers_without_duplicates_on_dovecot() {
    require_it!();
    renumbering(DOVECOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn renumbers_without_duplicates_on_greenmail() {
    require_it!();
    renumbering(GREENMAIL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn renumbers_without_duplicates_on_stalwart() {
    require_it!();
    renumbering(STALWART).await;
}

async fn flag_changes(run: &mut Run, server: ImapServer, user: &str) -> (String, Vec<Message>) {
    let messages: Vec<Message> = (0..3).map(|_| Message::new(user)).collect();
    run.seeded(&messages);
    let mut seeder = server.seeder(user).await;
    for message in &messages {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    let account = run.add(server, user).await;
    seeder.ok("SELECT INBOX").await;
    seeder.ok("STORE 1 +FLAGS (\\Seen)").await;
    seeder.ok("STORE 2 +FLAGS (\\Flagged)").await;
    seeder.logout().await;
    run.sync_clean(&account).await;
    (account, messages)
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

async fn condstore(server: ImapServer) {
    let user = server.user("condstore");
    let mut run = Run::start(Options::default(), server, &user).await;
    let (account, messages) = flag_changes(&mut run, server, &user).await;
    let log = run.api.sync_log(&account).await;
    assert!(
        line(&log, "INBOX").starts_with("INBOX: 0 new, 0 known, 0 removed, 2 changed"),
        "{log:?}"
    );
    let listed = run.by_subject().await;
    assert_eq!(flags(&listed[&messages[0].subject]), set(&["\\Seen"]));
    assert_eq!(flags(&listed[&messages[1].subject]), set(&["\\Flagged"]));
    assert_eq!(flags(&listed[&messages[2].subject]), set(&[]));
    assert_eq!(run.folder_ids().await["INBOX"]["unread"], 2);
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_flag_changes_since_a_modseq_on_dovecot() {
    require_it!();
    condstore(DOVECOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_flag_changes_since_a_modseq_on_stalwart() {
    require_it!();
    condstore(STALWART).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_all_flags_once_a_day_on_greenmail() {
    require_it!();
    let user = GREENMAIL.user("flagscan");
    let mut run = Run::start(Options::default(), GREENMAIL, &user).await;
    let (account, messages) = flag_changes(&mut run, GREENMAIL, &user).await;
    let log = run.api.sync_log(&account).await;
    assert!(
        line(&log, "INBOX").starts_with("INBOX: 0 new, 0 known, 0 removed, 0 changed"),
        "the flags were read before a day passed: {log:?}"
    );

    run.holtmail.age_flag_checks(25).await;
    run.sync_clean(&account).await;
    let log = run.api.sync_log(&account).await;
    assert!(
        line(&log, "INBOX").starts_with("INBOX: 0 new, 0 known, 0 removed, 2 changed"),
        "{log:?}"
    );
    let listed = run.by_subject().await;
    assert_eq!(flags(&listed[&messages[0].subject]), set(&["\\Seen"]));
    assert_eq!(flags(&listed[&messages[1].subject]), set(&["\\Flagged"]));
    run.check_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_for_young_mail_by_internal_date() {
    require_it!();
    let user = DOVECOT.user("minage");
    let old = Message::new(&user);
    let two_days = Message::new(&user);
    let ancient = Message::new(&user);
    let fresh = Message::new(&user);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder
        .append("INBOX", &[], Some(now() - Age::days(10)), &old.bytes())
        .await;
    let mut run = Run::start(Options::default(), DOVECOT, &user).await;
    run.seeded(&[
        old.clone(),
        two_days.clone(),
        ancient.clone(),
        fresh.clone(),
    ]);
    let account = run.add(DOVECOT, &user).await;
    run.api
        .update_account(&account, json!({ "min_age_days": 3 }))
        .await
        .ok();

    seeder
        .append("INBOX", &[], Some(now() - Age::days(2)), &two_days.bytes())
        .await;
    seeder
        .append("INBOX", &[], Some(now() - Age::days(20)), &ancient.bytes())
        .await;
    seeder.append("INBOX", &[], None, &fresh.bytes()).await;
    seeder.logout().await;

    run.sync_clean(&account).await;
    let log = run.api.sync_log(&account).await;
    assert!(
        line(&log, "INBOX").starts_with("INBOX: 0 new")
            && line(&log, "INBOX").contains("3 left until they are old enough"),
        "{log:?}"
    );
    assert_eq!(run.by_subject().await.len(), 1);

    run.api
        .update_account(&account, json!({ "min_age_days": 1 }))
        .await
        .ok();
    run.sync_clean(&account).await;
    let log = run.api.sync_log(&account).await;
    assert!(
        line(&log, "INBOX").starts_with("INBOX: 2 new")
            && line(&log, "INBOX").contains("1 left until they are old enough"),
        "{log:?}"
    );
    let listed = run.by_subject().await;
    assert!(listed.contains_key(&two_days.subject) && listed.contains_key(&ancient.subject));
    assert!(!listed.contains_key(&fresh.subject));

    run.api
        .update_account(&account, json!({ "min_age_days": null }))
        .await
        .ok();
    run.sync_clean(&account).await;
    assert!(run.by_subject().await.contains_key(&fresh.subject));
    run.check_logs();
}

fn connection(host: &str, port: u16, security: &str, user: &str) -> Value {
    json!({
        "host": host,
        "port": port,
        "security": security,
        "username": user,
        "password": DOVECOT.password,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn connects_over_tls_starttls_and_plain_text() {
    require_it!();
    let user = DOVECOT.user("tls");
    let message = Message::new(&user);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &message.bytes()).await;
    seeder.logout().await;
    let mut run = Run::start(Options::default(), DOVECOT, &user).await;
    run.seeded(std::slice::from_ref(&message));

    for (port, security) in [
        (DOVECOT_IMAPS, "tls"),
        (DOVECOT_IMAP, "starttls"),
        (DOVECOT_IMAP, "plain"),
    ] {
        let account = run
            .api
            .add_imap(&run.archive, connection("localhost", port, security, &user))
            .await;
        let settled = run.api.settle(&run.archive, &account).await;
        assert_eq!(
            settled["status"]["error"],
            Value::Null,
            "{security}: {settled}"
        );
    }
    assert_eq!(run.by_subject().await.len(), 1);

    for (host, port, security, expected) in [
        ("127.0.0.1", DOVECOT_IMAPS, "tls", "not valid for name"),
        ("127.0.0.1", DOVECOT_IMAP, "starttls", "not valid for name"),
        ("localhost", GREENMAIL_IMAPS, "tls", "UnknownIssuer"),
    ] {
        let refused = run
            .api
            .add_imap_account(&run.archive, connection(host, port, security, &user))
            .await;
        assert_eq!(
            refused.status, 400,
            "{host}:{port} {security}: {}",
            refused.body
        );
        let reason = refused.body["error"].as_str().unwrap_or_default();
        assert!(
            reason.contains(expected),
            "{host}:{port} {security}: {reason}"
        );
    }
    run.check_logs();

    let untrusting = Holtmail::start(Options {
        trust_stack_ca: false,
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&untrusting).await;
    let archive = api.create_archive("Untrusted").await;
    let refused = api
        .add_imap_account(
            &archive,
            connection("localhost", DOVECOT_IMAP, "starttls", &user),
        )
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(
        refused.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("UnknownIssuer"),
        "{}",
        refused.body
    );
    run.scanner.check(&untrusting.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_the_connection_only_with_the_password() {
    require_it!();
    let user = DOVECOT.user("move");
    let other = GREENMAIL.user("moved");
    let first = Message::new(&user);
    let second = Message::new(&other);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &first.bytes()).await;
    seeder.logout().await;
    let mut seeder = GREENMAIL.seeder(&other).await;
    seeder.append("INBOX", &[], None, &second.bytes()).await;
    seeder.logout().await;

    let mut run = Run::start(Options::default(), DOVECOT, &user).await;
    run.scanner = std::mem::take(&mut run.scanner)
        .secret("the other user", &other)
        .secret("the other password", GREENMAIL.password)
        .secret("a wrong password", "it-wrong-password");
    run.seeded(&[first.clone(), second.clone()]);
    let account = run.add(DOVECOT, &user).await;

    let to_greenmail = json!({
        "host": "127.0.0.1",
        "port": GREENMAIL.port,
        "security": "plain",
        "username": other,
    });
    let refused = run
        .api
        .update_account(&account, json!({ "connection": to_greenmail }))
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert_eq!(
        refused.body["error"],
        "Enter the password again when changing the server."
    );
    let wrong = run
        .api
        .update_account(
            &account,
            json!({
                "connection": connection("127.0.0.1", DOVECOT_IMAP, "plain", &user),
                "password": "it-wrong-password",
            }),
        )
        .await;
    assert_eq!(wrong.status, 400, "{}", wrong.body);
    assert_eq!(
        run.api.account(&run.archive, &account).await["connection"]["port"],
        DOVECOT_IMAP
    );

    let moved = run
        .api
        .update_account(
            &account,
            json!({ "connection": to_greenmail, "password": GREENMAIL.password }),
        )
        .await
        .ok();
    assert_eq!(moved["connection"]["port"], GREENMAIL.port);
    run.sync_clean(&account).await;
    let listed = run.by_subject().await;
    assert!(listed.contains_key(&first.subject) && listed.contains_key(&second.subject));

    let edit = |args: &[&str], stdin: &str| {
        let mut all = vec!["account", "edit", account.as_str()];
        all.extend_from_slice(args);
        run.holtmail.command(&all, stdin)
    };
    let port = DOVECOT_IMAP.to_string();
    let back = [
        "--host",
        "127.0.0.1",
        "--port",
        port.as_str(),
        "--security",
        "plain",
        "--username",
        user.as_str(),
    ];
    let refused = edit(&back, "");
    assert!(!refused.status.success());
    let mut with_password = back.to_vec();
    with_password.push("--password-stdin");
    let output = edit(&with_password, &format!("{}\n", DOVECOT.password));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        run.api.account(&run.archive, &account).await["connection"]["port"],
        DOVECOT_IMAP
    );
    run.sync_clean(&account).await;
    run.check_logs();
}
