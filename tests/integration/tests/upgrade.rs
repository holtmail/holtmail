//! An upgrade of a real data volume (`IT_PROFILES=docker`): the image built from `fed635b`
//! (migrations 0001–0005) archives an uploaded mbox and a Dovecot account into a volume and
//! stops; the image built from this checkout starts on the same volume, applies migrations
//! 0006–0015, rebuilds the search index, reads invitations from the older mail and builds people
//! in the background, and then search, labels, people, the calendar and another sync answer
//! correctly with every count unchanged and the old session and password still working.

use std::time::Duration;

use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD, id};
use holtmail_integration::data::{Message, mbox};
use holtmail_integration::docker::{Container, DEV_IMAGE, OLD_IMAGE, Volume, image_present};
use holtmail_integration::services::{DOVECOT, DOVECOT_PASSWORD};
use holtmail_integration::{Api, LogScanner, profile, require_it, unique};
use serde_json::{Value, json};

fn invitation(to: &str, uid: &str) -> Message {
    let boundary = "upgrade-boundary";
    let event = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//Integration tests//EN\r\nMETHOD:REQUEST\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260415T090000Z\r\n\
         DTEND:20260415T100000Z\r\nSUMMARY:Upgrade planning\r\nSEQUENCE:0\r\n\
         ORGANIZER;CN=Olga Organizer:mailto:organizer@example.org\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{to}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let mut message = Message::new(to).body(&format!(
        "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nAn invented invitation.\r\n\
         --{boundary}\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\n{event}\
         --{boundary}--\r\n"
    ));
    message.from = "organizer@example.org".into();
    message.from_name = "Olga Organizer".into();
    message.content_type = format!("multipart/mixed; boundary=\"{boundary}\"");
    message
}

#[derive(Debug, PartialEq)]
struct Counts {
    stats: Value,
    folders: usize,
    accounts: usize,
    messages: usize,
    hits: u64,
}

async fn counts(api: &Api, archive: &str, word: &str) -> Counts {
    let stats = api.get(&format!("/api/archives/{archive}")).await.ok()["stats"].clone();
    Counts {
        stats,
        folders: api.folders(archive).await.len(),
        accounts: api.accounts(archive).await.len(),
        messages: api.messages(archive).await.len(),
        hits: api
            .get(&format!("/api/archives/{archive}/search?q={word}"))
            .await
            .ok()["total"]
            .as_u64()
            .expect("a total"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_current_image_upgrades_a_volume_of_the_old_one() {
    require_it!();
    if !profile("docker") {
        eprintln!("skipped: needs IT_PROFILES=docker");
        return;
    }
    assert!(
        image_present(OLD_IMAGE) && image_present(DEV_IMAGE),
        "it.sh builds both images"
    );
    let volume = Volume::new("holtmail-it-upgrade");
    let user = DOVECOT.user("upgrade");
    let to = user.clone();
    let token = unique("upgrade");
    let synced: Vec<Message> = (0..6)
        .map(|i| {
            Message::new(&to)
                .subject(&format!("Synced {token} {i}"))
                .body(&format!("Synced body {i} mentions keyword {token}."))
        })
        .collect();
    let mut imported: Vec<Message> = (0..8)
        .map(|i| {
            let mut m = Message::new(&to).subject(&format!("Imported {token} {i}"));
            m.from = "frequent-sender@example.org".into();
            m.from_name = "Frequent Sender".into();
            m
        })
        .collect();
    imported.push(invitation(
        &to,
        &format!("{}@example.org", unique("invite")),
    ));
    let mut seeder = DOVECOT.seeder(&user).await;
    for message in &synced {
        seeder.append("INBOX", &[], None, &message.bytes()).await;
    }
    seeder.create("Projects").await;
    seeder
        .append("Projects", &["\\Seen"], None, &synced[0].bytes())
        .await;
    seeder.logout().await;

    let old = Container::start(OLD_IMAGE, &volume, &[("HOLTMAIL_SYNC_INTERVAL", "0")]).await;
    let api = Api::for_url(&old.url);
    let setup_token = old.setup_token().expect("a setup link");
    api.post(
        "/api/setup",
        json!({ "token": setup_token, "email": OWNER_EMAIL, "name": "Owner Example", "password": OWNER_PASSWORD }),
    )
    .await
    .ok();
    let archive = api.create_archive("Before the upgrade").await;
    let (_, task) = api.import(&archive, "older.mbox", mbox(&imported)).await;
    assert_eq!(task["state"], "done", "{task}");
    let account = api
        .add_imap(
            &archive,
            json!({ "host": "dovecot", "port": 31143, "security": "plain", "username": user, "password": DOVECOT_PASSWORD }),
        )
        .await;
    let settled = api.settle(&archive, &account).await;
    assert_eq!(settled["status"]["error"], Value::Null, "{settled}");
    let before = counts(&api, &archive, &token).await;
    assert_eq!(before.messages, synced.len() + imported.len(), "{before:?}");
    assert_eq!(before.hits, (synced.len() + 8) as u64, "{before:?}");
    let session = api.get("/api/session").await.ok();
    old.stop();
    let old_logs = old.logs();
    drop(old);

    let new = Container::start(DEV_IMAGE, &volume, &[("HOLTMAIL_SYNC_INTERVAL", "0")]).await;
    assert!(
        new.setup_token().is_none(),
        "the new image asked for setup again"
    );
    for needle in ["search index rebuilt", "invitations read", "people built"] {
        new.wait_log(needle, Duration::from_secs(120)).await;
    }
    let api = Api::sign_in_url(&new.url, OWNER_EMAIL, OWNER_PASSWORD).await;
    assert_eq!(api.get("/api/session").await.ok()["user"], session["user"]);
    let after = counts(&api, &archive, &token).await;
    assert_eq!(after, before, "the counts changed across the upgrade");

    let labels = api
        .get(&format!("/api/archives/{archive}/labels"))
        .await
        .ok();
    assert_eq!(labels, json!([]), "labels of an archive without Gmail");
    let people = api.people(&archive).await;
    let frequent = people
        .iter()
        .find(|p| p["email"] == "frequent-sender@example.org")
        .unwrap_or_else(|| panic!("no person for the frequent sender: {people:?}"));
    let detail = api.get(&format!("/api/people/{}", id(frequent))).await.ok();
    assert_eq!(detail["correspondence"]["messages"], 8, "{detail}");

    let calendar = api
        .get(&format!(
            "/api/archives/{archive}/calendar?from=1775001600000&to=1777593600000"
        ))
        .await
        .ok();
    let occurrences = calendar["occurrences"].as_array().expect("occurrences");
    assert_eq!(occurrences.len(), 1, "{calendar}");
    assert_eq!(occurrences[0]["title"], "Upgrade planning");
    let event = api
        .get(&format!(
            "/api/events/{}",
            occurrences[0]["group_id"].as_str().expect("a group")
        ))
        .await
        .ok();
    assert_eq!(
        event["messages"].as_array().map(Vec::len),
        Some(1),
        "{event}"
    );

    let resynced = api.sync(&archive, &account).await;
    assert_eq!(resynced["status"]["error"], Value::Null, "{resynced}");
    assert_eq!(resynced["status"]["new_messages"], 0, "{resynced}");
    assert_eq!(counts(&api, &archive, &token).await, before);

    new.stop();
    let dir = tempfile::tempdir().expect("temp dir");
    for file in ["holtmail.db", "holtmail.db-wal", "holtmail.db-shm"] {
        let _ = holtmail_integration::docker::docker(
            &[
                "cp",
                &format!("{}:/data/{file}", new.name),
                dir.path().join(file).to_str().unwrap(),
            ],
            b"",
        );
    }
    let versions = migrations(&dir.path().join("holtmail.db")).await;
    assert_eq!(
        versions,
        (1..=15).collect::<Vec<i64>>(),
        "applied migrations"
    );

    let scanner = LogScanner::new()
        .secret("the IMAP password", DOVECOT_PASSWORD)
        .secret("the IMAP user", &user)
        .secret("the search word", &token)
        .secrets("a subject", imported.iter().map(|m| m.subject.as_str()));
    scanner.check(&old_logs);
    scanner.check(&new.logs());
}

async fn migrations(db: &std::path::Path) -> Vec<i64> {
    use sqlx::ConnectOptions;
    let mut db = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db)
        .read_only(true)
        .connect()
        .await
        .expect("the copied database opens");
    sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
        .fetch_all(&mut db)
        .await
        .expect("the migrations table")
}
