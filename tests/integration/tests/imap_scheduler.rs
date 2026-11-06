//! The scheduler and restarts: a short interval syncs on its own, tasks and last runs survive a
//! restart, and a sync cut short by a killed process shows as interrupted and resumes.

use std::time::{Duration, Instant};

use holtmail_integration::data::Message;
use holtmail_integration::relay::{Mode, Relay};
use holtmail_integration::services::{DOVECOT, local};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it};
use serde_json::{Value, json};

fn scanner(user: &str, messages: &[Message]) -> LogScanner {
    LogScanner::new()
        .secret("the IMAP password", DOVECOT.password)
        .secret("the IMAP user", user)
        .secrets("a subject", messages.iter().map(|m| m.subject.as_str()))
        .secrets("a sender", messages.iter().map(|m| m.from.as_str()))
}

async fn wait_for_messages(api: &Api, archive: &str, count: usize, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let listed = api.messages(archive).await.len();
        if listed == count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{listed} messages instead of {count} after {within:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn sync_tasks(tasks: &[Value], account: &str) -> Vec<Value> {
    let mut found: Vec<Value> = tasks
        .iter()
        .filter(|t| t["kind"] == "sync" && t["account_id"] == account)
        .cloned()
        .collect();
    found.sort_by_key(|t| t["started_at"].as_i64().unwrap_or(0));
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_on_its_own_and_keeps_runs_across_a_restart() {
    require_it!();
    let user = DOVECOT.user("schedule");
    let first = Message::new(&user);
    let later = Message::new(&user);
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &first.bytes()).await;

    let mut holtmail = Holtmail::start(Options {
        sync_interval_minutes: Some(1),
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Scheduled").await;
    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    let first_run = settled["status"]["finished_at"].as_i64().expect("a run");

    seeder.append("INBOX", &[], None, &later.bytes()).await;
    seeder.logout().await;
    wait_for_messages(&api, &archive, 2, Duration::from_secs(150)).await;
    let listed = api
        .wait_for_run(&archive, &account, first_run + 1, Duration::from_secs(60))
        .await;
    let second_run = listed["status"]["finished_at"].as_i64().expect("a run");
    assert!(
        listed["status"]["next_sync_at"].as_i64().is_some(),
        "{listed}"
    );
    let before = sync_tasks(&api.tasks().await, &account);
    assert!(before.len() >= 2, "{before:?}");
    assert!(before.iter().all(|t| t["state"] == "done"), "{before:?}");

    holtmail.restart().await;
    let api = Api::sign_in(
        &holtmail,
        holtmail_integration::api::OWNER_EMAIL,
        holtmail_integration::api::OWNER_PASSWORD,
    )
    .await;
    let after = sync_tasks(&api.tasks().await, &account);
    assert_eq!(
        after.iter().map(|t| &t["id"]).collect::<Vec<_>>(),
        before.iter().map(|t| &t["id"]).collect::<Vec<_>>()
    );
    assert!(after.iter().all(|t| t["state"] == "done"), "{after:?}");
    assert_eq!(
        after.last().map(|t| &t["log"]),
        before.last().map(|t| &t["log"])
    );
    let status = &api.account(&archive, &account).await["status"];
    assert!(
        status["finished_at"].as_i64() >= Some(second_run),
        "{status}"
    );
    assert_eq!(status["error"], Value::Null);
    scanner(&user, &[first, later]).check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_a_sync_the_killed_process_left_running() {
    require_it!();
    resume_after_kill(60).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_a_killed_sync_of_an_account_that_syncs_only_on_request() {
    require_it!();
    resume_after_kill(0).await;
}

async fn resume_after_kill(minutes: u64) {
    let user = DOVECOT.user("killed");
    let line = "Invented text that fills the message up to about ten kilobytes, line by line.\n";
    let messages: Vec<Message> = (0..120)
        .map(|_| Message::new(&user).body(&line.repeat(10 * 1024 / line.len())))
        .collect();
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder
        .append_many(
            "INBOX",
            &messages.iter().map(Message::bytes).collect::<Vec<_>>(),
        )
        .await;
    seeder.logout().await;
    let relay = Relay::start(&local(DOVECOT.port)).await;
    relay.set(Mode::StallAfter(800 * 1024));

    let mut holtmail = Holtmail::start(Options {
        sync_interval_minutes: Some(minutes),
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Killed").await;
    let mut account = DOVECOT.account(&user);
    account["port"] = json!(relay.port());
    let account = api.add_imap(&archive, account).await;
    wait_for_messages(&api, &archive, 50, Duration::from_secs(60)).await;
    let listed = api.account(&archive, &account).await;
    assert_eq!(listed["status"]["running"], true, "{listed}");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    holtmail.kill();

    relay.set(Mode::Pass);
    relay.cut_all();
    holtmail.start_again().await;
    let api = Api::sign_in(
        &holtmail,
        holtmail_integration::api::OWNER_EMAIL,
        holtmail_integration::api::OWNER_PASSWORD,
    )
    .await;
    wait_for_messages(&api, &archive, 120, Duration::from_secs(90)).await;
    let synced = api
        .wait_for_run(&archive, &account, 1, Duration::from_secs(60))
        .await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    let tasks = sync_tasks(&api.tasks().await, &account);
    assert_eq!(tasks.len(), 2, "{tasks:?}");
    assert_eq!(tasks[0]["state"], "interrupted", "{tasks:?}");
    assert_eq!(tasks[1]["state"], "done", "{tasks:?}");
    let resumed = tasks[1]["log"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["text"].as_str())
        .any(|l| l.starts_with("INBOX: 70 new, 0 known"));
    assert!(resumed, "{:?}", tasks[1]["log"]);
    if minutes == 0 {
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        loop {
            let listed = api.account(&archive, &account).await;
            if listed["status"]["next_sync_at"].is_null() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "still scheduled: {listed}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert_eq!(
            sync_tasks(&api.tasks().await, &account).len(),
            2,
            "synced once at start, then only on request"
        );
    }
    scanner(&user, &messages).check(&holtmail.logs());
}
