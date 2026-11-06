//! Bulk deletion with review and the archive ledger against a real Dovecot: deleted mail stays
//! deleted across syncs, server-side copies and re-imports; the ledger names tampering.

use std::time::Duration;

use holtmail_integration::data::Message;
use holtmail_integration::imap::Seeder;
use holtmail_integration::services::{DOVECOT_IMAP, DOVECOT_IMAPS, DOVECOT_PASSWORD, local};
use holtmail_integration::{Api, Holtmail, require_it, unique};
use serde_json::{Value, json};

async fn setup(n: usize) -> (Holtmail, Api, String, String, String, Vec<Message>) {
    let user = format!("{}@example.com", unique("delete"));
    let messages: Vec<Message> = (0..n).map(|_| Message::new(&user)).collect();
    let mut seeder = Seeder::login(&local(DOVECOT_IMAP), &user, DOVECOT_PASSWORD).await;
    for message in &messages {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    seeder.logout().await;
    let holtmail = Holtmail::start_default().await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Deletions").await;
    let account = api
        .add_imap(
            &archive,
            json!({
                "host": "localhost",
                "port": DOVECOT_IMAPS,
                "security": "tls",
                "username": user,
                "password": DOVECOT_PASSWORD,
            }),
        )
        .await;
    api.settle(&archive, &account).await;
    (holtmail, api, archive, account, user, messages)
}

fn id_of(listed: &[Value], subject: &str) -> String {
    listed
        .iter()
        .find(|m| m["subject"] == subject)
        .and_then(|m| m["id"].as_str())
        .unwrap_or_else(|| panic!("{subject} not listed"))
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn deleted_mail_stays_deleted() {
    require_it!();
    let (_holtmail, api, archive, account, user, messages) = setup(3).await;
    let listed = api.messages(&archive).await;
    assert_eq!(listed.len(), 3);
    let gone = id_of(&listed, &messages[0].subject);

    let review = api
        .post(
            &format!("/api/archives/{archive}/deletions"),
            json!({ "message_ids": [gone] }),
        )
        .await
        .ok();
    assert_eq!(review["count"], 1);
    assert_eq!(review["sample"][0]["subject"], messages[0].subject.as_str());
    let review_id = review["id"].as_str().unwrap().to_owned();
    let wrong = api
        .post(
            &format!("/api/deletions/{review_id}/confirm"),
            json!({ "count": 2 }),
        )
        .await;
    assert_eq!(wrong.status.as_u16(), 400);
    let task = api
        .post(
            &format!("/api/deletions/{review_id}/confirm"),
            json!({ "count": 1 }),
        )
        .await
        .ok();
    let task = api
        .wait_task(task["id"].as_str().unwrap(), Duration::from_secs(30))
        .await;
    assert_eq!(task["state"], "done", "{task}");
    assert_eq!(task["kind"], "delete_messages");
    let again = api
        .post(
            &format!("/api/deletions/{review_id}/confirm"),
            json!({ "count": 1 }),
        )
        .await;
    assert_eq!(again.status.as_u16(), 400, "a review was used twice");

    assert_eq!(api.messages(&archive).await.len(), 2);
    assert_eq!(
        api.get(&format!("/api/messages/{gone}/raw"))
            .await
            .status
            .as_u16(),
        404
    );
    let ledger = api.get("/api/ledger").await.ok();
    assert_eq!(ledger["head"]["event"], "deleted");
    assert_eq!(ledger["head"]["message_id"], gone.as_str());

    api.sync(&archive, &account).await;
    assert_eq!(
        api.messages(&archive).await.len(),
        2,
        "a sync brought it back"
    );

    let mut seeder = Seeder::login(&local(DOVECOT_IMAP), &user, DOVECOT_PASSWORD).await;
    seeder.create("Copies").await;
    seeder
        .append("Copies", &[], None, &messages[0].bytes())
        .await;
    seeder.logout().await;
    api.sync(&archive, &account).await;
    assert_eq!(
        api.messages(&archive).await.len(),
        2,
        "a copy on the server came back"
    );
    let log = api.sync_log(&account).await.join("\n");
    assert!(log.contains("1 deleted earlier left out"), "{log}");

    let (_, imported) = api.import(&archive, "again.eml", messages[0].bytes()).await;
    assert!(
        imported["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("deleted earlier left out"),
        "{imported}"
    );
    assert_eq!(
        api.messages(&archive).await.len(),
        2,
        "a re-import brought it back"
    );

    let by_search = api
        .post(
            &format!("/api/archives/{archive}/deletions"),
            json!({ "query": format!("subject:\"{}\"", messages[1].subject) }),
        )
        .await
        .ok();
    assert_eq!(by_search["count"], 1, "{by_search}");
    let empty = api
        .post(
            &format!("/api/archives/{archive}/deletions"),
            json!({ "query": "" }),
        )
        .await;
    assert_eq!(empty.status.as_u16(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ledger_names_tampering() {
    require_it!();
    let (holtmail, api, _archive, _account, _user, _messages) = setup(2).await;
    let head = holtmail.command(&["ledger", "head"], "");
    assert!(head.status.success());
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    assert!(head.starts_with("2:") && head.len() == 2 + 64, "{head}");

    let clean = holtmail.command(&["ledger", "verify", "--blobs", "--anchor", &head], "");
    let out = String::from_utf8_lossy(&clean.stdout);
    assert!(clean.status.success(), "{out}");
    assert!(out.contains("2 entries checked, nothing wrong"), "{out}");

    let started = api
        .post("/api/ledger/verify", json!({ "anchor": head }))
        .await
        .ok();
    let task = api
        .wait_task(started["id"].as_str().unwrap(), Duration::from_secs(30))
        .await;
    assert_eq!(task["state"], "done", "{task}");
    assert_eq!(
        api.get("/api/ledger").await.ok()["last_verification"]["ok"],
        true
    );

    holtmail
        .execute("UPDATE ledger SET at = at + 1 WHERE seq = 1", &[])
        .await;
    let started = api.post("/api/ledger/verify", json!({})).await.ok();
    let task = api
        .wait_task(started["id"].as_str().unwrap(), Duration::from_secs(30))
        .await;
    assert_eq!(task["state"], "failed", "{task}");
    let last = api.get("/api/ledger").await.ok()["last_verification"].clone();
    assert_eq!(last["ok"], false);
    let problems = last["problems"].to_string();
    assert!(problems.contains("entry 1 was changed"), "{problems}");

    let blob = walk(&holtmail.data_dir().join("blobs"))
        .into_iter()
        .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .expect("a blob");
    std::fs::write(&blob, b"From: someone else\r\n\r\nchanged\r\n").unwrap();
    let damaged = holtmail.command(&["ledger", "verify", "--blobs"], "");
    let out = String::from_utf8_lossy(&damaged.stdout);
    assert_eq!(damaged.status.code(), Some(1), "{out}");
    assert!(out.contains("no longer match their hash"), "{out}");
}

fn walk(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    if path.is_dir() {
        std::fs::read_dir(path)
            .unwrap()
            .flat_map(|e| walk(&e.unwrap().path()))
            .collect()
    } else {
        vec![path.to_path_buf()]
    }
}
