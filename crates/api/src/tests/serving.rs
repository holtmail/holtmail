use holtmail_core::{ArchiveId, Timestamp};
use holtmail_store::{FolderRole, ServedKey};

use super::*;

impl Fixture {
    async fn owner(&self) -> holtmail_core::UserId {
        self.store
            .list_users()
            .await
            .unwrap()
            .into_iter()
            .find(|u| u.email == "ada@example.com")
            .unwrap()
            .id
    }
}

fn delete(uri: &str) -> Request<Body> {
    Request::delete(uri)
        .header("x-requested-with", "holtmail")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_and_drops_local_flags() {
    let f = fixture().await;
    let ada = f.owner().await;
    let news = f.message_id("Weekly news").await;
    let reply = f.message_id("Re: Weekly news").await;
    let now = Timestamp::now();
    f.store
        .set_local_flags(ada, news, &["\\Flagged".into()], &["\\Seen".into()], now)
        .await
        .unwrap();
    f.store
        .set_local_flags(ada, reply, &["\\Seen".into()], &[], now)
        .await
        .unwrap();

    let serving = f.json("/api/devices/serving").await;
    assert_eq!(serving["local_flags"], true);
    assert_eq!(serving["archives"][0]["archive_id"], f.archive.as_str());
    assert_eq!(serving["archives"][0]["local_flagged"], 2);
    assert_eq!(serving["archives"][0]["mailboxes"], 0);

    let uri = format!("/api/devices/local-flags?archive={}", f.archive);
    let listed = f.json(&uri).await;
    assert_eq!(listed["enabled"], true);
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["messages"][0]["subject"], "Re: Weekly news");
    assert_eq!(listed["messages"][1]["subject"], "Weekly news");
    assert_eq!(
        listed["messages"][1]["flags"],
        json!([{ "flag": "\\Flagged", "present": true }, { "flag": "\\Seen", "present": false }])
    );

    let (status, _, body) = f.send(delete(&format!("{uri}&message={news}"))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["dropped"],
        1
    );
    assert_eq!(f.json(&uri).await["total"], 1);

    let (status, _, body) = f.send(delete(&uri)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["dropped"],
        1
    );
    let (_, _, body) = f.send(delete(&uri)).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["dropped"],
        0
    );
    assert_eq!(f.json("/api/devices/serving").await["archives"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_flags_of_an_unreadable_archive_are_not_found() {
    let f = fixture().await;
    let other = ArchiveId::new();
    let (status, _, _) = f
        .get(&format!("/api/devices/local-flags?archive={other}"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = f
        .send(delete(&format!("/api/devices/local-flags?archive={other}")))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = f
        .send(delete(&format!(
            "/api/devices/local-flags?archive={}&message=nope",
            f.archive
        )))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_build_progress_of_served_mailboxes() {
    let f = fixture().await;
    let ada = f.owner().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let now = Timestamp::now();
    for key in [
        ServedKey::All,
        ServedKey::Role(FolderRole::Inbox),
        ServedKey::Drafts(ada),
    ] {
        f.store
            .open_served_mailbox(archive, key, now)
            .await
            .unwrap();
    }
    let inbox = f
        .store
        .build_served_mailbox(archive, ServedKey::Role(FolderRole::Inbox), 1000, now)
        .await
        .unwrap();
    assert!(inbox.built);
    let inbox = inbox.progress.total;
    assert!(inbox > 0);
    f.store
        .build_served_mailbox(archive, ServedKey::All, 1, now)
        .await
        .unwrap();

    let serving = f.json("/api/devices/serving").await;
    let row = &serving["archives"][0];
    assert_eq!(row["mailboxes"], 3);
    assert_eq!(row["built"], 1);
    assert_eq!(
        row["assigned"],
        1 + inbox,
        "a built mailbox counts in full, so the bar never falls back"
    );
    assert_eq!(row["total"], 2 + inbox);
    assert_eq!(row["local_flagged"], 0);
}
