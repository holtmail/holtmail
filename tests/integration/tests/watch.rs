//! Watching mail servers through the real binary: with syncs only on request, new mail reaches
//! the archive within seconds over IMAP IDLE (Dovecot, GreenMail, Stalwart), over NOTIFY for
//! several folders where the server has it, and over the JMAP event source (Stalwart). A watch
//! whose connection the server drops reconnects and catches up; a watch turned off stays off.

use std::time::{Duration, Instant};

use holtmail_integration::api::id;
use holtmail_integration::data::Message;
use holtmail_integration::services::{DOVECOT, GREENMAIL, ImapServer, STALWART, compose};
use holtmail_integration::stalwart::{STALWART_PASSWORD, Seeder};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it};
use serde_json::{Value, json};

fn watching() -> Options {
    Options {
        watch: true,
        ..Options::default()
    }
}

async fn wait_for(api: &Api, archive: &str, count: usize, within: Duration) -> Duration {
    let started = Instant::now();
    loop {
        let listed = api.messages(archive).await.len();
        if listed >= count {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < within,
            "{listed} messages instead of {count} after {within:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_watch(
    api: &Api,
    archive: &str,
    account: &str,
    field: &str,
    state: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let listed = api.account(archive, account).await;
        if listed["status"][field]["state"] == state {
            return listed;
        }
        assert!(
            Instant::now() < deadline,
            "{field} is not {state}: {}",
            listed["status"]
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn scanner(server: &ImapServer, user: &str, messages: &[&Message]) -> LogScanner {
    LogScanner::new()
        .secret("the IMAP password", server.password)
        .secret("the IMAP user", user)
        .secrets("a subject", messages.iter().map(|m| m.subject.as_str()))
        .secrets("a sender", messages.iter().map(|m| m.from.as_str()))
}

async fn idle_archives_new_mail_within_seconds(server: ImapServer) {
    let user = server.user("idle");
    let first = Message::new(&user);
    let pushed = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    seeder.append("INBOX", &[], None, &first.bytes()).await;
    seeder.logout().await;

    let holtmail = Holtmail::start(watching()).await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Watched").await;
    let account = api.add_imap(&archive, server.account(&user)).await;
    api.settle(&archive, &account).await;
    wait_for(&api, &archive, 1, Duration::from_secs(30)).await;
    let live = wait_for_watch(&api, &archive, &account, "watch", "live").await;
    assert_eq!(live["watch"]["mode"], "inbox", "{live}");
    assert_eq!(
        live["status"]["next_sync_at"],
        Value::Null,
        "only on request"
    );

    let mut seeder = server.seeder(&user).await;
    seeder.append("INBOX", &[], None, &pushed.bytes()).await;
    seeder.logout().await;
    let took = wait_for(&api, &archive, 2, Duration::from_secs(20)).await;
    println!(
        "{}: new mail archived {took:?} after it arrived",
        server.service
    );
    let log = api.sync_log(&account).await;
    assert!(
        log.iter()
            .any(|l| l.contains("reported changes in 1 folder")),
        "{log:?}"
    );
    scanner(&server, &user, &[&first, &pushed]).check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_on_dovecot() {
    require_it!();
    idle_archives_new_mail_within_seconds(DOVECOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_on_greenmail() {
    require_it!();
    idle_archives_new_mail_within_seconds(GREENMAIL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_on_stalwart() {
    require_it!();
    idle_archives_new_mail_within_seconds(STALWART).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chosen_folders_are_watched_and_others_wait() {
    require_it!();
    let server = DOVECOT;
    let user = server.user("folders");
    let mut seeder = server.seeder(&user).await;
    seeder.create("Work").await;
    seeder.create("Later").await;
    let start = Message::new(&user);
    seeder.append("INBOX", &[], None, &start.bytes()).await;
    seeder.logout().await;

    let holtmail = Holtmail::start(watching()).await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Folders").await;
    let account = api.add_imap(&archive, server.account(&user)).await;
    api.settle(&archive, &account).await;
    let changed = api
        .update_account(
            &account,
            json!({ "watch": { "mode": "folders", "folders": ["INBOX", "Work"] } }),
        )
        .await
        .ok();
    assert_eq!(changed["watch"]["folders"], json!(["INBOX", "Work"]));
    wait_for_watch(&api, &archive, &account, "watch", "live").await;

    let later = Message::new(&user);
    let work = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    seeder.append("Later", &[], None, &later.bytes()).await;
    seeder.append("Work", &[], None, &work.bytes()).await;
    seeder.logout().await;
    wait_for(&api, &archive, 2, Duration::from_secs(20)).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let subjects: Vec<String> = api
        .messages(&archive)
        .await
        .iter()
        .map(|m| m["subject"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(subjects.contains(&work.subject), "{subjects:?}");
    assert!(
        !subjects.contains(&later.subject),
        "an unwatched folder waits for a sync: {subjects:?}"
    );
    scanner(&server, &user, &[&start, &later, &work]).check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_connection_reconnects_and_catches_up() {
    require_it!();
    let server = DOVECOT;
    let user = server.user("kick");
    let first = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    seeder.append("INBOX", &[], None, &first.bytes()).await;
    seeder.logout().await;

    let holtmail = Holtmail::start(watching()).await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Kicked").await;
    let account = api.add_imap(&archive, server.account(&user)).await;
    api.settle(&archive, &account).await;
    wait_for_watch(&api, &archive, &account, "watch", "live").await;

    compose(&["exec", "-T", "dovecot", "doveadm", "kick", &user]);
    let gap = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    seeder.append("INBOX", &[], None, &gap.bytes()).await;
    seeder.logout().await;
    let took = wait_for(&api, &archive, 2, Duration::from_secs(60)).await;
    println!("archived {took:?} after the server dropped the watch");
    wait_for_watch(&api, &archive, &account, "watch", "live").await;
    let after = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    seeder.append("INBOX", &[], None, &after.bytes()).await;
    seeder.logout().await;
    wait_for(&api, &archive, 3, Duration::from_secs(20)).await;
    scanner(&server, &user, &[&first, &gap, &after]).check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watch_turned_off_leaves_mail_for_the_next_sync() {
    require_it!();
    let server = DOVECOT;
    let user = server.user("unwatched");
    let mut seeder = server.seeder(&user).await;
    let first = Message::new(&user);
    seeder.append("INBOX", &[], None, &first.bytes()).await;
    seeder.logout().await;

    let holtmail = Holtmail::start(watching()).await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Off").await;
    let account = api.add_imap(&archive, server.account(&user)).await;
    api.settle(&archive, &account).await;
    api.update_account(&account, json!({ "watch": { "mode": "off" } }))
        .await
        .ok();
    wait_for_watch(&api, &archive, &account, "watch", "off").await;
    let waiting = Message::new(&user);
    let mut seeder = server.seeder(&user).await;
    seeder.append("INBOX", &[], None, &waiting.bytes()).await;
    seeder.logout().await;
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(api.messages(&archive).await.len(), 1);
    api.sync(&archive, &account).await;
    assert_eq!(api.messages(&archive).await.len(), 2);
    scanner(&server, &user, &[&first, &waiting]).check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_jmap_event_source_archives_new_mail_within_seconds() {
    require_it!();
    let user = format!("{}@example.com", holtmail_integration::unique("jmappush"));
    let seeder = Seeder::new(&user).await;
    let inbox = seeder.role("inbox").await;
    let first = Message::new(&user);
    seeder.import(&first.bytes(), &[&inbox], &[]).await;

    let holtmail = Holtmail::start(watching()).await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("JMAP push").await;
    let added = api
        .add_jmap_account(
            &archive,
            json!({
                "url": holtmail_integration::stalwart::session_url(),
                "username": user,
                "password": STALWART_PASSWORD,
            }),
        )
        .await
        .ok();
    let account = id(&added);
    api.settle(&archive, &account).await;
    wait_for(&api, &archive, 1, Duration::from_secs(30)).await;
    wait_for_watch(&api, &archive, &account, "watch", "live").await;

    let pushed = Message::new(&user);
    seeder.import(&pushed.bytes(), &[&inbox], &[]).await;
    let took = wait_for(&api, &archive, 2, Duration::from_secs(20)).await;
    println!("JMAP: new mail archived {took:?} after it arrived");

    let synced_before = api.sync_log(&account).await;
    api.sync(&archive, &account).await;
    let log = api.sync_log(&account).await;
    assert_ne!(synced_before, log);
    LogScanner::new()
        .secret("the JMAP password", STALWART_PASSWORD)
        .secret("the JMAP user", &user)
        .secrets(
            "a subject",
            [first.subject.as_str(), pushed.subject.as_str()],
        )
        .check(&holtmail.logs());
}
