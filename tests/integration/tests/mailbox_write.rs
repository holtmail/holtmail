//! Changing mailboxes on real servers: through the API the binary sets flags, moves, deletes
//! (to the trash and for good), restores archived bytes and saves a sent copy on Dovecot, and
//! the suite reads the server itself over IMAP to see each change land; a flag another client
//! takes away after Holtmail set it stays away (the server wins), and a star asked for right
//! after a move, before any sync, follows the message to its new folder. On Stalwart over JMAP it sets
//! keywords, deletes to the trash and restores.

use std::time::{Duration, Instant};

use holtmail_integration::api::id;
use holtmail_integration::data::Message;
use holtmail_integration::imap::{Seeder, quote};
use holtmail_integration::services::{DOVECOT, GREENMAIL_SMTP, local};
use holtmail_integration::stalwart::{self, STALWART_PASSWORD};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, unique};
use reqwest::Method;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(60);

async fn start(name: &str) -> (Holtmail, Api, String) {
    let holtmail = Holtmail::start(Options {
        extra_config: "[write]\nundo_seconds = 1\n".into(),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive(&unique(name)).await;
    (holtmail, api, archive)
}

async fn allow(api: &Api, account: &str, capabilities: &[&str]) {
    api.request(
        Method::PUT,
        &format!("/api/accounts/{account}/capabilities"),
        Some(&json!({ "capabilities": capabilities })),
    )
    .await
    .ok();
}

async fn by_subject(api: &Api, archive: &str, subject: &str) -> String {
    api.messages(archive)
        .await
        .into_iter()
        .find(|m| m["subject"] == subject)
        .map(|m| id(&m))
        .unwrap_or_else(|| panic!("no archived message {subject}"))
}

async fn change(api: &Api, message: &str, op: Value) -> Value {
    let done = api
        .post(
            &format!("/api/messages/{message}/changes"),
            json!({ "op": op }),
        )
        .await
        .ok();
    assert_eq!(
        done["changes"].as_array().map(Vec::len),
        Some(1),
        "one change: {done}"
    );
    done["changes"][0].clone()
}

async fn change_state(api: &Api, account: &str, change: &str) -> Value {
    api.get(&format!("/api/accounts/{account}/mail-changes?limit=500"))
        .await
        .ok()
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == change)
        .cloned()
        .unwrap_or_else(|| panic!("no change {change}"))
}

/// Waits until the writeback has sent the change: applied, or sending with the server's
/// answer recorded (flags wait for a sync); fails on `failed`.
async fn sent(api: &Api, account: &str, change: &str) -> Value {
    let started = Instant::now();
    loop {
        let found = change_state(api, account, change).await;
        let waits_for_sync = found["kind"] == "flags" || found["kind"] == "labels";
        match found["state"].as_str() {
            Some("applied") => return found,
            Some("failed") => panic!("the change failed: {found}"),
            Some("superseded_by_server") if waits_for_sync => return found,
            Some("sending") if waits_for_sync && !found["sent_at"].is_null() => return found,
            _ => {}
        }
        assert!(started.elapsed() < WAIT, "not sent in time: {found}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The flags and UIDs of the messages in `folder` whose subject is `subject`, read by another
/// IMAP client.
async fn on_dovecot(user: &str, folder: &str, subject: &str) -> Vec<(u64, String)> {
    let mut imap = Seeder::login(&local(DOVECOT.port), user, DOVECOT.password).await;
    imap.ok(&format!("SELECT {}", quote(folder))).await;
    let lines = imap
        .ok(&format!("UID SEARCH HEADER SUBJECT {}", quote(subject)))
        .await;
    let uids: Vec<u64> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("* SEARCH"))
        .flat_map(|l| l.split_whitespace().filter_map(|n| n.parse().ok()))
        .collect();
    let mut out = Vec::new();
    for uid in uids {
        let lines = imap.ok(&format!("UID FETCH {uid} (FLAGS)")).await;
        let flags = lines
            .iter()
            .find_map(|l| {
                let start = l.find("FLAGS (")? + "FLAGS (".len();
                let end = l[start..].find(')')? + start;
                Some(l[start..end].to_owned())
            })
            .unwrap_or_default();
        out.push((uid, flags));
    }
    imap.logout().await;
    out
}

async fn until_dovecot(
    user: &str,
    folder: &str,
    subject: &str,
    done: impl Fn(&[(u64, String)]) -> bool,
) -> Vec<(u64, String)> {
    let started = Instant::now();
    loop {
        let found = on_dovecot(user, folder, subject).await;
        if done(&found) {
            return found;
        }
        assert!(
            started.elapsed() < WAIT,
            "{folder} on the server never matched: {found:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn body_on_dovecot(user: &str, folder: &str, uid: u64) -> Vec<u8> {
    let mut imap = Seeder::login(&local(DOVECOT.port), user, DOVECOT.password).await;
    let bodies = imap.bodies(folder).await;
    let lines = imap.ok("UID SEARCH ALL").await;
    imap.logout().await;
    let uids: Vec<u64> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("* SEARCH"))
        .flat_map(|l| l.split_whitespace().filter_map(|n| n.parse().ok()))
        .collect();
    let at = uids.iter().position(|u| *u == uid).expect("the UID");
    bodies[at].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_dovecot_mailboxes_and_the_server_wins() {
    require_it!();
    let user = DOVECOT.user("write");
    let messages: Vec<Message> = ["Flag", "Move", "Trash", "Expunge", "Contested"]
        .iter()
        .map(|what| Message::new(&user).subject(&unique(what)))
        .collect();
    let [flag, moving, trash, expunge, contested] = &messages[..] else {
        unreachable!()
    };
    let mut seeder = DOVECOT.seeder(&user).await;
    for message in &messages {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    seeder.create("Projects").await;
    seeder.logout().await;

    let (holtmail, api, archive) = start("write").await;
    let account = api.add_imap(&archive, DOVECOT.account(&user)).await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    let folders = api.folders(&archive).await;
    let folder = |path: &str| {
        folders
            .iter()
            .find(|f| f["path"] == path)
            .map(id)
            .unwrap_or_else(|| panic!("no folder {path}"))
    };
    let (inbox, projects, trash_folder) = (folder("INBOX"), folder("Projects"), folder("Trash"));
    assert_eq!(
        folders.iter().find(|f| f["path"] == "Trash").unwrap()["role"],
        "trash"
    );

    let flag_id = by_subject(&api, &archive, &flag.subject).await;
    let refused = api
        .post(
            &format!("/api/messages/{flag_id}/changes"),
            json!({ "op": { "kind": "flags", "add": ["\\Seen"] } }),
        )
        .await;
    assert_eq!(refused.status.as_u16(), 403, "no capability yet");
    allow(
        &api,
        &account,
        &[
            "mail.flags",
            "mail.move",
            "mail.delete",
            "mail.expunge",
            "mail.append",
            "mail.send",
        ],
    )
    .await;

    let flagged = change(
        &api,
        &flag_id,
        json!({ "kind": "flags", "add": ["\\Seen", "\\Flagged"] }),
    )
    .await;
    let detail = api.message(&flag_id).await;
    assert_eq!(detail["pending"][0]["id"], flagged["id"], "the held mark");
    sent(&api, &account, id(&flagged).as_str()).await;
    until_dovecot(&user, "INBOX", &flag.subject, |found| {
        found.len() == 1 && found[0].1.contains("\\Seen") && found[0].1.contains("\\Flagged")
    })
    .await;

    let moving_id = by_subject(&api, &archive, &moving.subject).await;
    let moved = change(
        &api,
        &moving_id,
        json!({ "kind": "move", "folder": projects }),
    )
    .await;
    sent(&api, &account, &id(&moved)).await;
    until_dovecot(&user, "Projects", &moving.subject, |f| f.len() == 1).await;
    assert!(on_dovecot(&user, "INBOX", &moving.subject).await.is_empty());
    let starred = change(
        &api,
        &moving_id,
        json!({ "kind": "flags", "add": ["\\Flagged"] }),
    )
    .await;
    assert_eq!(
        starred["folder_id"], inbox,
        "planned where sync last saw it"
    );
    let starred = sent(&api, &account, &id(&starred)).await;
    assert_eq!(starred["folder_id"], projects, "the star followed the move");
    until_dovecot(&user, "Projects", &moving.subject, |f| {
        f.len() == 1 && f[0].1.contains("\\Flagged")
    })
    .await;

    let trash_id = by_subject(&api, &archive, &trash.subject).await;
    let deleted = change(&api, &trash_id, json!({ "kind": "delete" })).await;
    assert_eq!(deleted["kind"], "move");
    assert_eq!(deleted["target_folder_id"], trash_folder);
    sent(&api, &account, &id(&deleted)).await;
    until_dovecot(&user, "Trash", &trash.subject, |f| f.len() == 1).await;

    let expunge_id = by_subject(&api, &archive, &expunge.subject).await;
    let gone = change(
        &api,
        &expunge_id,
        json!({ "kind": "delete", "expunge": true }),
    )
    .await;
    sent(&api, &account, &id(&gone)).await;
    until_dovecot(&user, "INBOX", &expunge.subject, |f| f.is_empty()).await;
    assert!(
        on_dovecot(&user, "Trash", &expunge.subject)
            .await
            .is_empty()
    );

    let contested_id = by_subject(&api, &archive, &contested.subject).await;
    let contest = change(
        &api,
        &contested_id,
        json!({ "kind": "flags", "add": ["\\Flagged"] }),
    )
    .await;
    sent(&api, &account, &id(&contest)).await;
    let on_server = until_dovecot(&user, "INBOX", &contested.subject, |f| {
        f.len() == 1 && f[0].1.contains("\\Flagged")
    })
    .await;
    let mut other = Seeder::login(&local(DOVECOT.port), &user, DOVECOT.password).await;
    other.ok("SELECT INBOX").await;
    other
        .ok(&format!("UID STORE {} -FLAGS (\\Flagged)", on_server[0].0))
        .await;
    other.logout().await;

    let synced = api.sync(&archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    assert_eq!(
        change_state(&api, &account, &id(&flagged)).await["state"],
        "applied",
        "sync confirmed the flags"
    );
    assert_eq!(
        change_state(&api, &account, &id(&contest)).await["state"],
        "superseded_by_server",
        "another client took the flag away after it was sent"
    );
    let contested_detail = api.message(&contested_id).await;
    assert!(
        contested_detail["pending"].as_array().unwrap().is_empty(),
        "{contested_detail}"
    );
    let inbox_flags = contested_detail["locations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["folder_id"] == inbox)
        .unwrap()["flags"]
        .clone();
    assert!(
        !inbox_flags
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "\\Flagged"),
        "{inbox_flags}"
    );
    let flag_detail = api.message(&flag_id).await;
    assert!(flag_detail["pending"].as_array().unwrap().is_empty());
    assert_eq!(
        change_state(&api, &account, &id(&starred)).await["state"],
        "applied",
        "sync confirmed the star in the folder the message moved to"
    );
    let moved_detail = api.message(&moving_id).await;
    let places: Vec<(&str, bool)> = moved_detail["locations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["folder_id"].as_str().unwrap(), l["removed_at"].is_null()))
        .collect();
    assert!(places.contains(&(projects.as_str(), true)), "{places:?}");
    assert!(places.contains(&(inbox.as_str(), false)), "{places:?}");

    let restored = api
        .post(
            &format!("/api/messages/{expunge_id}/restore"),
            json!({ "folder": inbox }),
        )
        .await
        .ok();
    assert_eq!(restored["kind"], "append");
    let restored = sent(&api, &account, &id(&restored)).await;
    let back = until_dovecot(&user, "INBOX", &expunge.subject, |f| f.len() == 1).await;
    assert!(back[0].1.contains("\\Seen"), "{back:?}");
    assert_eq!(
        restored["result_uid"].as_u64(),
        Some(back[0].0),
        "APPENDUID"
    );
    assert_eq!(
        body_on_dovecot(&user, "INBOX", back[0].0).await,
        api.raw(&expunge_id).await,
        "the archived bytes went back unchanged"
    );

    let transport = api
        .post(
            &format!("/api/archives/{archive}/transports"),
            json!({
                "name": "GreenMail",
                "kind": "smtp",
                "host": "127.0.0.1",
                "port": GREENMAIL_SMTP,
                "security": "none",
                "allowed_from": ["*@example.com"],
            }),
        )
        .await
        .ok();
    let identity = api
        .post(
            &format!("/api/archives/{archive}/identities"),
            json!({
                "address": user,
                "name": "Ada Example",
                "transport": id(&transport),
                "account": account,
                "sent_copy": "append:Sent",
                "active": true,
            }),
        )
        .await
        .ok();
    assert_eq!(identity["sent_copy"], "append:Sent", "{identity}");
    let subject = unique("Sent copy");
    let draft = api
        .post(
            &format!("/api/archives/{archive}/drafts"),
            json!({ "payload": {
                "to": [{ "address": format!("{}@example.org", unique("bob")) }],
                "subject": subject,
                "text": "The copy goes to Sent.",
            } }),
        )
        .await
        .ok();
    let queued = api
        .post(&format!("/api/drafts/{}/send", id(&draft)), json!({}))
        .await
        .ok();
    let outbox = id(&queued);
    let started = Instant::now();
    let entry = loop {
        let entry = api.get(&format!("/api/outbox/{outbox}")).await.ok();
        if entry["sent_copy"]["state"] == "applied" {
            break entry;
        }
        assert!(started.elapsed() < WAIT, "no sent copy: {entry}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(entry["state"], "sent");
    assert_eq!(entry["sent_copy"]["origin"], "sent_copy");
    let copy = until_dovecot(&user, "Sent", &subject, |f| f.len() == 1).await;
    assert!(copy[0].1.contains("\\Seen"), "{copy:?}");
    assert_eq!(
        body_on_dovecot(&user, "Sent", copy[0].0).await,
        api.raw(entry["message"].as_str().unwrap()).await
    );

    let audit = api.get("/api/audit").await.ok();
    let kinds: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    for kind in ["mail_flags", "mail_move", "mail_delete", "mail_append"] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
    LogScanner::new()
        .secret("the IMAP password", DOVECOT.password)
        .secret("the address", &user)
        .secrets(
            "a subject",
            messages
                .iter()
                .map(|m| m.subject.as_str())
                .chain([subject.as_str()]),
        )
        .check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_stalwart_mailboxes_over_jmap() {
    require_it!();
    let user = format!("{}@example.com", unique("write-jmap"));
    let seeder = stalwart::Seeder::new(&user).await;
    let inbox_box = seeder.role("inbox").await;
    let trash_box = seeder.role("trash").await;
    let flag = Message::new(&user).subject(&unique("Keyword"));
    let doomed = Message::new(&user).subject(&unique("Doomed"));
    seeder.import(&flag.bytes(), &[&inbox_box], &[]).await;
    let doomed_email = seeder.import(&doomed.bytes(), &[&inbox_box], &[]).await;

    let (holtmail, api, archive) = start("write-jmap").await;
    let added = api
        .add_jmap_account(
            &archive,
            json!({
                "url": stalwart::base_url(),
                "username": user,
                "password": STALWART_PASSWORD,
            }),
        )
        .await
        .ok();
    let account = id(&added);
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    allow(
        &api,
        &account,
        &["mail.flags", "mail.delete", "mail.append"],
    )
    .await;
    let inbox = api
        .folders(&archive)
        .await
        .into_iter()
        .find(|f| f["role"] == "inbox")
        .map(|f| id(&f))
        .expect("an inbox");

    let flag_id = by_subject(&api, &archive, &flag.subject).await;
    let flagged = change(
        &api,
        &flag_id,
        json!({ "kind": "flags", "add": ["\\Flagged", "\\Seen"] }),
    )
    .await;
    sent(&api, &account, &id(&flagged)).await;
    let started = Instant::now();
    loop {
        let emails = seeder.emails(&inbox_box).await;
        let email = emails
            .iter()
            .find(|e| e["subject"] == flag.subject)
            .unwrap();
        if email["keywords"]["$flagged"] == true && email["keywords"]["$seen"] == true {
            break;
        }
        assert!(started.elapsed() < WAIT, "keywords not set: {email}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    api.sync(&archive, &account).await;
    assert_eq!(
        change_state(&api, &account, &id(&flagged)).await["state"],
        "applied"
    );

    let doomed_id = by_subject(&api, &archive, &doomed.subject).await;
    let deleted = change(&api, &doomed_id, json!({ "kind": "delete" })).await;
    sent(&api, &account, &id(&deleted)).await;
    let in_trash = seeder.emails(&trash_box).await;
    assert!(
        in_trash.iter().any(|e| e["subject"] == doomed.subject),
        "{in_trash:?}"
    );
    seeder.destroy(&doomed_email).await;
    assert!(
        !seeder
            .emails(&trash_box)
            .await
            .iter()
            .any(|e| e["subject"] == doomed.subject)
    );

    let restored = api
        .post(
            &format!("/api/messages/{doomed_id}/restore"),
            json!({ "folder": inbox }),
        )
        .await
        .ok();
    sent(&api, &account, &id(&restored)).await;
    let back = seeder.emails(&inbox_box).await;
    let email = back
        .iter()
        .find(|e| e["subject"] == doomed.subject)
        .unwrap_or_else(|| panic!("not restored: {back:?}"));
    assert_eq!(email["keywords"]["$seen"], true);
    assert_eq!(
        email["size"].as_u64(),
        Some(api.raw(&doomed_id).await.len() as u64)
    );

    LogScanner::new()
        .secret("the JMAP password", STALWART_PASSWORD)
        .secret("the address", &user)
        .secrets(
            "a subject",
            [flag.subject.as_str(), doomed.subject.as_str()],
        )
        .check(&holtmail.logs());
}
