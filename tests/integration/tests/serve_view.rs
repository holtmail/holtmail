//! The change feed and served UIDs over a real Dovecot sync: the binary writes `changes` rows
//! as it archives, moves and flags, only when something changed, and builds the served
//! mailboxes of an archive once a device reads it over IMAP. A held move gets its served UID
//! in the target at once and keeps it after writeback sent the move and sync saw the message
//! under its new upstream UID. The suite opens the instance's database beside the running
//! binary to read the served tables, as an IMAP session would.

use std::time::{Duration, Instant};

use holtmail_core::{ArchiveId, FolderId, MessageId, Timestamp};
use holtmail_integration::api::id;
use holtmail_integration::data::Message;
use holtmail_integration::services::DOVECOT;
use holtmail_integration::{Api, Holtmail, Options, require_it, unique};
use holtmail_store::{FolderRole, ServeStore, ServedKey, ServedMember, ServedUidQuery};
use holtmail_store_sqlite::SqliteStore;
use reqwest::Method;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(60);

async fn rows(holtmail: &Holtmail, archive: &str) -> Vec<(u64, String, String)> {
    holtmail
        .query(
            "SELECT seq, kind, COALESCE(message_id, '') FROM changes WHERE archive_id = ? ORDER BY seq",
            &[archive],
        )
        .await
        .into_iter()
        .map(|r| (r[0].parse().unwrap(), r[1].clone(), r[2].clone()))
        .collect()
}

async fn built(holtmail: &Holtmail, archive: &str, key: &str) {
    let started = Instant::now();
    loop {
        let found = holtmail
            .query(
                "SELECT built FROM served_mailboxes WHERE archive_id = ? AND key = ?",
                &[archive, key],
            )
            .await;
        if found.first().is_some_and(|r| r[0] == "1") {
            return;
        }
        assert!(started.elapsed() < WAIT, "{key} was never built: {found:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn uid(store: &SqliteStore, archive: ArchiveId, key: ServedKey, m: MessageId) -> Option<u32> {
    store
        .catch_up_served_mailbox(archive, key, Timestamp::now())
        .await
        .unwrap();
    store
        .served_uids(
            archive,
            key,
            &ServedUidQuery {
                limit: 10_000,
                ..ServedUidQuery::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.member == ServedMember::Message(m))
        .map(|u| u.uid)
}

async fn sent(api: &Api, account: &str, change: &str) {
    let started = Instant::now();
    loop {
        let found = api
            .get(&format!("/api/accounts/{account}/mail-changes?limit=500"))
            .await
            .ok();
        let state = found
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == change)
            .map(|c| c["state"].clone())
            .unwrap_or(Value::Null);
        if state == "applied" {
            return;
        }
        assert_ne!(state, "failed", "the move failed");
        assert!(started.elapsed() < WAIT, "not sent in time: {state}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn change_rows_and_served_uids_survive_a_held_move_and_its_sync() {
    require_it!();
    let user = DOVECOT.user("served");
    let moving = Message::new(&user).subject(&unique("Moving"));
    let staying = Message::new(&user).subject(&unique("Staying"));
    let mut seeder = DOVECOT.seeder(&user).await;
    seeder.append("INBOX", &[], None, &moving.bytes()).await;
    seeder.append("INBOX", &[], None, &staying.bytes()).await;
    seeder.create("Projects").await;
    seeder.logout().await;

    let holtmail = Holtmail::start(Options::default()).await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive(&unique("served")).await;
    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    let messages = api.messages(&archive).await;
    let message_id = |subject: &str| {
        messages
            .iter()
            .find(|m| m["subject"] == subject)
            .map(id)
            .unwrap_or_else(|| panic!("no archived message {subject}"))
    };
    let (moving_id, staying_id) = (message_id(&moving.subject), message_id(&staying.subject));
    let folders = api.folders(&archive).await;
    let projects = folders
        .iter()
        .find(|f| f["path"] == "Projects")
        .map(id)
        .expect("Projects");

    let log = rows(&holtmail, &archive).await;
    for message in [&moving_id, &staying_id] {
        assert!(
            log.iter().any(|(_, k, m)| k == "message" && m == message),
            "ingest wrote a message row: {log:?}"
        );
    }
    assert!(log.iter().any(|(_, k, _)| k == "mailbox"), "folder rows");

    let before = rows(&holtmail, &archive).await.len();
    api.sync(&archive, &account).await;
    assert_eq!(
        rows(&holtmail, &archive).await.len(),
        before,
        "a resync of unchanged folders writes no rows"
    );

    let made = api
        .request(
            Method::POST,
            "/api/devices",
            Some(&json!({ "name": "Laptop", "protocols": ["imap"], "mail_archive": archive })),
        )
        .await;
    assert_eq!(made.status.as_u16(), 201, "{}", made.body);
    built(&holtmail, &archive, "role:inbox").await;
    built(&holtmail, &archive, &format!("folder:{projects}")).await;
    built(&holtmail, &archive, "all").await;

    let store = SqliteStore::open(holtmail.database()).await.unwrap();
    let archive_id: ArchiveId = archive.parse().unwrap();
    let projects_id: FolderId = projects.parse().unwrap();
    let inbox = ServedKey::Role(FolderRole::Inbox);
    let target = ServedKey::Folder(projects_id);
    let moving_m: MessageId = moving_id.parse().unwrap();
    let staying_m: MessageId = staying_id.parse().unwrap();
    let first = uid(&store, archive_id, inbox, moving_m)
        .await
        .expect("a UID in INBOX");
    let staying_uid = uid(&store, archive_id, inbox, staying_m).await.unwrap();

    api.request(
        Method::PUT,
        &format!("/api/accounts/{account}/capabilities"),
        Some(&json!({ "capabilities": ["mail.flags", "mail.move"] })),
    )
    .await
    .ok();
    let mark = rows(&holtmail, &archive).await.last().unwrap().0;
    let moved = api
        .post(
            &format!("/api/messages/{moving_id}/changes"),
            json!({ "op": { "kind": "move", "folder": projects } }),
        )
        .await
        .ok();
    let change = id(&moved["changes"][0]);
    let held = uid(&store, archive_id, target, moving_m)
        .await
        .expect("the held move has its target UID at once");
    let after: Vec<_> = rows(&holtmail, &archive)
        .await
        .into_iter()
        .filter(|(seq, _, _)| *seq > mark)
        .collect();
    assert!(
        after
            .iter()
            .any(|(_, k, m)| k == "place" && *m == moving_id),
        "the held move wrote a place row: {after:?}"
    );

    sent(&api, &account, &change).await;
    api.sync(&archive, &account).await;
    let detail = api.message(&moving_id).await;
    let live: Vec<&Value> = detail["locations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["removed_at"].is_null())
        .collect();
    assert_eq!(live.len(), 1, "{detail}");
    assert_eq!(live[0]["folder_id"], projects, "sync saw it in Projects");
    assert_eq!(
        uid(&store, archive_id, target, moving_m).await,
        Some(held),
        "the served UID is the message's, not the upstream one"
    );
    assert_eq!(uid(&store, archive_id, inbox, moving_m).await, None);
    let vanished = store
        .served_vanished(archive_id, inbox, 0, 0, 100)
        .await
        .unwrap();
    assert!(vanished.contains(&first), "a tombstone for the old UID");

    let before = store
        .served_uids(
            archive_id,
            inbox,
            &ServedUidQuery {
                limit: 10,
                ..ServedUidQuery::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.member == ServedMember::Message(staying_m))
        .unwrap()
        .modseq;
    let flagged = api
        .post(
            &format!("/api/messages/{staying_id}/changes"),
            json!({ "op": { "kind": "flags", "add": ["\\Seen"] } }),
        )
        .await
        .ok();
    assert_eq!(flagged["changes"].as_array().map(Vec::len), Some(1));
    store
        .catch_up_served_mailbox(archive_id, inbox, Timestamp::now())
        .await
        .unwrap();
    let after = store
        .served_uids(
            archive_id,
            inbox,
            &ServedUidQuery {
                limit: 10,
                ..ServedUidQuery::default()
            },
        )
        .await
        .unwrap();
    let staying_row = after
        .iter()
        .find(|u| u.member == ServedMember::Message(staying_m))
        .unwrap();
    assert_eq!(staying_row.uid, staying_uid, "a flag change keeps the UID");
    assert!(staying_row.modseq > before, "and raises its MODSEQ");
}
