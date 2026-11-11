use holtmail_core::{AccountId, ArchiveId, FolderId, MessageId};
use holtmail_store::{AccountKind, FolderRole, MailChangeState, NewAccount};

use super::*;

pub(super) struct Imap {
    pub(super) account: AccountId,
    inbox: FolderId,
    archive_folder: FolderId,
    trash: FolderId,
    pub(super) newsletter: MessageId,
    plain: MessageId,
}

impl Fixture {
    pub(super) async fn imap(&self) -> Imap {
        let archive: ArchiveId = self.archive.parse().unwrap();
        let account = self
            .store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Imap,
                name: "ada@example.com".into(),
                address: Some("ada@example.com".into()),
                settings: json!({}),
                sealed_secret: None,
                enabled: false,
            })
            .await
            .unwrap()
            .id;
        let folder = |path: &'static str, role| {
            let store = self.store.clone();
            async move { store.upsert_folder(account, path, role).await.unwrap().id }
        };
        let inbox = folder("INBOX", Some(FolderRole::Inbox)).await;
        let archive_folder = folder("Archive", Some(FolderRole::Archive)).await;
        let trash = folder("Trash", Some(FolderRole::Trash)).await;
        let page = self
            .json(&format!("/api/archives/{}/messages", self.archive))
            .await;
        let id = |subject: &str| -> MessageId {
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["subject"] == subject)
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap()
        };
        let newsletter = id("Weekly news");
        let plain = id("Re: Weekly news");
        self.store
            .record_location(
                newsletter,
                inbox,
                Some(10),
                &["\\Flagged".to_owned()],
                holtmail_core::Timestamp::now(),
            )
            .await
            .unwrap();
        Imap {
            account,
            inbox,
            archive_folder,
            trash,
            newsletter,
            plain,
        }
    }

    pub(super) async fn allow(&self, account: AccountId, capabilities: &[&str]) {
        let (status, _, body) = self
            .send(request(
                "PUT",
                &format!("/api/accounts/{account}/capabilities"),
                json!({ "capabilities": capabilities }),
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    }

    async fn change(&self, message: MessageId, op: Value) -> (StatusCode, Value) {
        let (status, _, body) = self
            .send(post_json(
                &format!("/api/messages/{message}/changes"),
                json!({ "op": op }),
            ))
            .await;
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn holds_flag_move_and_delete_changes_behind_every_lock() {
    let f = fixture().await;
    let m = f.imap().await;

    let (status, _) = f
        .change(m.newsletter, json!({ "kind": "flags", "add": ["\\Seen"] }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no capability yet");

    f.allow(m.account, &["mail.flags", "mail.move", "mail.delete"])
        .await;
    let (status, done) = f
        .change(m.newsletter, json!({ "kind": "flags", "add": ["\\Seen"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let changes = done["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "only the place on a server: {done}");
    assert_eq!(changes[0]["kind"], "flags");
    assert_eq!(changes[0]["state"], "held");
    assert_eq!(changes[0]["uid"], 10);
    assert_eq!(changes[0]["folder_id"], m.inbox.to_string());

    let detail = f.json(&format!("/api/messages/{}", m.newsletter)).await;
    assert_eq!(detail["pending"].as_array().unwrap().len(), 1);
    assert_eq!(detail["pending"][0]["add"], json!(["\\Seen"]));

    let (status, done) = f
        .change(
            m.newsletter,
            json!({ "kind": "flags", "add": ["\\Flagged"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(done["changes"].as_array().unwrap().is_empty());
    assert_eq!(done["skipped"][0]["reason"], "already");

    let (status, done) = f
        .change(m.newsletter, json!({ "kind": "flags", "add": ["\\Seen"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        done["skipped"][0]["reason"], "already",
        "a held change counts: {done}"
    );
    let (status, done) = f
        .change(
            m.newsletter,
            json!({ "kind": "flags", "remove": ["\\Seen"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        done["changes"].as_array().unwrap().len(),
        1,
        "undoing a held change is recorded although the server has no \\Seen: {done}"
    );

    for bad in [
        json!({ "kind": "flags", "add": ["\\Deleted"] }),
        json!({ "kind": "flags", "add": [] }),
        json!({ "kind": "flags", "add": ["two words"] }),
        json!({ "kind": "labels", "add": ["Trash"] }),
        json!({ "kind": "labels", "add": ["spam"] }),
        json!({ "kind": "labels", "remove": ["Inbox"] }),
        json!({ "kind": "labels", "add": ["\\Important"] }),
    ] {
        let (status, _) = f.change(m.newsletter, bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let (status, done) = f
        .change(
            m.newsletter,
            json!({ "kind": "move", "folder": m.archive_folder.to_string() }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["changes"][0]["kind"], "move");
    assert_eq!(
        done["changes"][0]["target_folder_id"],
        m.archive_folder.to_string()
    );

    let (status, _, body) = f
        .send(post_json(
            "/api/mail/changes",
            json!({
                "messages": [m.newsletter.to_string(), m.plain.to_string()],
                "op": { "kind": "delete" },
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let done: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(done["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        done["changes"][0]["kind"], "move",
        "a delete goes to the trash"
    );
    assert_eq!(done["changes"][0]["target_folder_id"], m.trash.to_string());
    assert_eq!(
        done["skipped"],
        json!([{ "message_id": m.plain.to_string(), "reason": "no_server" }])
    );

    let (status, _) = f
        .change(m.newsletter, json!({ "kind": "delete", "expunge": true }))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "expunging needs mail.expunge"
    );
    let (status, _, _) = f
        .send(post_json(
            "/api/mail/changes",
            json!({ "messages": [], "op": { "kind": "delete" } }),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let listed = f
        .json(&format!(
            "/api/accounts/{}/mail-changes?state=held",
            m.account
        ))
        .await;
    assert_eq!(listed.as_array().unwrap().len(), 4);
    let held = f.store.pending_mail_changes(m.newsletter).await.unwrap();
    assert!(held.iter().all(|c| c.state == MailChangeState::Held));

    let audit = f.json(&format!("/api/audit?archive={}", f.archive)).await;
    let kinds: Vec<&str> = audit
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    for kind in ["mail_flags", "mail_move", "mail_delete"] {
        assert!(kinds.contains(&kind), "{kind} audited: {kinds:?}");
    }
    assert!(
        audit
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "mail_flags")
            .all(|e| e["targets"] == json!([m.newsletter.to_string()]))
    );

    let (grace, member) = f.add_user("grace@example.org", "member").await;
    let (status, _, _) = f
        .send_as(
            &member,
            post_json(
                &format!("/api/messages/{}/changes", m.newsletter),
                json!({ "op": { "kind": "flags", "remove": ["\\Seen"] } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an archive not granted");
    let (status, _, _) = f
        .send(request(
            "PUT",
            &format!("/api/users/{grace}/grants/{}", f.archive),
            json!({ "role": "reader" }),
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = f
        .send_as(
            &member,
            post_json(
                &format!("/api/messages/{}/changes", m.newsletter),
                json!({ "op": { "kind": "flags", "remove": ["\\Seen"] } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a reader cannot write");
    let (status, _, _) = f
        .send(request(
            "PUT",
            &format!("/api/users/{grace}/grants/{}", f.archive),
            json!({ "role": "editor" }),
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = f
        .send_as(
            &member,
            post_json(
                &format!("/api/messages/{}/changes", m.newsletter),
                json!({ "op": { "kind": "flags", "remove": ["\\Flagged"] } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
}

#[tokio::test(flavor = "multi_thread")]
async fn restores_archived_bytes_into_a_folder() {
    let f = fixture().await;
    let m = f.imap().await;
    let uri = format!("/api/messages/{}/restore", m.plain);
    let body = json!({ "folder": m.inbox.to_string() });
    let (status, _, _) = f.send(post_json(&uri, body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    f.allow(m.account, &["mail.append"]).await;
    let (status, _, answer) = f.send(post_json(&uri, body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&answer));
    let change: Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(change["kind"], "append");
    assert_eq!(change["uid"], Value::Null);
    assert_eq!(change["message_id"], m.plain.to_string());
    assert_eq!(change["add"], json!(["\\Seen"]));

    let other = f.store.create_archive("Work").await.unwrap().id;
    let elsewhere = f
        .store
        .create_account(NewAccount {
            archive_id: other,
            kind: AccountKind::Imap,
            name: "work".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    let folder = f
        .store
        .upsert_folder(elsewhere, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    f.allow(elsewhere, &["mail.append"]).await;
    let (status, _, _) = f
        .send(post_json(&uri, json!({ "folder": folder.to_string() })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "another archive");
    let (status, _, _) = f
        .send(post_json(
            &uri,
            json!({ "folder": FolderId::new().to_string() }),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let audit = f.json(&format!("/api/audit?archive={}", f.archive)).await;
    assert!(
        audit
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "mail_append")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sets_retention_and_counts_what_it_would_delete() {
    let f = fixture().await;
    let m = f.imap().await;
    let uri = format!("/api/accounts/{}/retention", m.account);
    let shown = f.json(&uri).await;
    assert_eq!(shown["days"], Value::Null);
    assert_eq!(shown["due"], 0);
    assert_eq!(shown["active"], false);

    let (status, _, _) = f.send(request("PUT", &uri, json!({ "days": 30 }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "needs mail.delete");
    f.allow(m.account, &["mail.delete"]).await;
    let (status, _, body) = f.send(request("PUT", &uri, json!({ "days": 30 }))).await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let saved: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(saved["days"], 30);
    assert_eq!(saved["due"], 1, "the newsletter from March 2025");
    assert_eq!(saved["expunge"], false);
    assert_eq!(saved["active"], true);

    let dry = f.json(&format!("{uri}?days=36500")).await;
    assert_eq!(dry["days"], 30, "a dry run saves nothing");
    assert_eq!(dry["checked_days"], 36500);
    assert_eq!(dry["due"], 0);
    let (status, _, _) = f.get(&format!("{uri}?days=0")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, admin) = f.add_user("alan@example.org", "admin").await;
    let (status, _, _) = f
        .send_as(&admin, request("PUT", &uri, json!({ "days": 7 })))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "admins delete only with a grant"
    );
    let (_, member) = f.add_user("grace@example.org", "member").await;
    let (status, _, _) = f
        .send_as(&member, request("PUT", &uri, json!({ "days": 7 })))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, body) = f.send(request("PUT", &uri, json!({ "days": null }))).await;
    assert_eq!(status, StatusCode::OK);
    let off: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(off["days"], Value::Null);
    let audit = f.json(&format!("/api/audit?archive={}", f.archive)).await;
    let targets: Vec<&Value> = audit
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "capabilities_changed")
        .map(|e| &e["targets"])
        .collect();
    assert!(targets.contains(&&json!(["delete_after_days:30"])));
    assert!(targets.contains(&&json!(["delete_after_days:off"])));
    let _ = m.trash;
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_and_dismisses_failed_changes_and_lists_what_may_change() {
    let f = fixture().await;
    let m = f.imap().await;
    let accounts = f
        .json(&format!("/api/archives/{}/accounts", f.archive))
        .await;
    let writable = |accounts: &Value| {
        accounts
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == m.account.to_string())
            .unwrap()["writable"]
            .clone()
    };
    assert_eq!(writable(&accounts), json!([]));
    f.allow(m.account, &["mail.flags", "mail.delete", "mail.expunge"])
        .await;
    let accounts = f
        .json(&format!("/api/archives/{}/accounts", f.archive))
        .await;
    assert_eq!(
        writable(&accounts),
        json!(["mail.flags", "mail.delete", "mail.expunge"])
    );

    let (status, done) = f
        .change(m.newsletter, json!({ "kind": "flags", "add": ["\\Seen"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let id: holtmail_core::MailChangeId =
        done["changes"][0]["id"].as_str().unwrap().parse().unwrap();
    let retry = format!("/api/mail-changes/{id}/retry");
    let dismiss = format!("/api/mail-changes/{id}/dismiss");
    let (status, _, _) = f.send(post_json(&retry, json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "a held change is not failed");

    f.store
        .fail_mail_change(id, "connection", holtmail_core::Timestamp::now())
        .await
        .unwrap()
        .unwrap();
    let detail = f.json(&format!("/api/messages/{}", m.newsletter)).await;
    assert!(detail["pending"].as_array().unwrap().is_empty());
    assert_eq!(detail["failed"][0]["id"], id.to_string());
    assert_eq!(detail["failed"][0]["last_error"], "connection");

    let (status, _, body) = f.send(post_json(&retry, json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let again: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(again["state"], "held");
    assert_ne!(again["id"], id.to_string());
    assert_eq!(again["add"], json!(["\\Seen"]));
    let detail = f.json(&format!("/api/messages/{}", m.newsletter)).await;
    assert!(detail["failed"].as_array().unwrap().is_empty());
    assert_eq!(detail["pending"][0]["id"], again["id"]);
    let (status, _, _) = f.send(post_json(&retry, json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "retried once");

    let second: holtmail_core::MailChangeId = again["id"].as_str().unwrap().parse().unwrap();
    f.store
        .fail_mail_change(second, "unsupported", holtmail_core::Timestamp::now())
        .await
        .unwrap()
        .unwrap();
    let (_, member) = f.add_user("grace@example.org", "member").await;
    let (status, _, _) = f
        .send_as(
            &member,
            post_json(&format!("/api/mail-changes/{second}/dismiss"), json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an archive not granted");
    let (status, _, body) = f
        .send(post_json(
            &format!("/api/mail-changes/{second}/dismiss"),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let detail = f.json(&format!("/api/messages/{}", m.newsletter)).await;
    assert!(detail["failed"].as_array().unwrap().is_empty());
    let (status, _, _) = f.send(post_json(&dismiss, json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "dismissed by the retry");

    f.allow(m.account, &[]).await;
    let third = f
        .store
        .enqueue_mail_change(&holtmail_store::NewMailChange {
            account_id: m.account,
            folder_id: m.inbox,
            message_id: Some(m.newsletter),
            uid: Some(10),
            op: holtmail_store::MailOp::Flags {
                add: vec!["\\Answered".into()],
                remove: Vec::new(),
            },
            origin: holtmail_store::MailChangeOrigin::User,
            user_id: None,
            device_id: None,
            outbox_id: None,
            at: holtmail_core::Timestamp::now(),
        })
        .await
        .unwrap();
    f.store
        .fail_mail_change(third.id, "gone", holtmail_core::Timestamp::now())
        .await
        .unwrap();
    let (status, _, _) = f
        .send(post_json(
            &format!("/api/mail-changes/{}/retry", third.id),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the capability is off now");
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_accounts_without_the_write_consent_offer_no_changes() {
    let f = fixture().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let account = f
        .store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Graph,
            name: "ada@example.com".into(),
            address: Some("ada@example.com".into()),
            settings: json!({ "oauth": { "provider": "microsoft" } }),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    let inbox = f
        .store
        .upsert_folder(account, "Inbox", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    let page = f
        .json(&format!("/api/archives/{}/messages", f.archive))
        .await;
    let message: MessageId = page["items"][0]["id"].as_str().unwrap().parse().unwrap();
    f.store
        .record_location(
            message,
            inbox,
            Some(3),
            &[],
            holtmail_core::Timestamp::now(),
        )
        .await
        .unwrap();
    f.allow(account, &["mail.flags"]).await;

    let accounts = f
        .json(&format!("/api/archives/{}/accounts", f.archive))
        .await;
    let row = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == account.to_string())
        .unwrap();
    assert_eq!(row["writable"], json!([]));
    assert_eq!(row["needs_sign_in"], true);
    assert!(
        row["sign_in_reason"]
            .as_str()
            .unwrap()
            .contains("change mail")
    );
    let (status, _) = f
        .change(message, json!({ "kind": "flags", "add": ["\\Seen"] }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut granted = f.store.get_account(account).await.unwrap().unwrap();
    granted.settings = json!({ "oauth": { "provider": "microsoft", "write": true } });
    f.store.update_account(&granted).await.unwrap();
    let accounts = f
        .json(&format!("/api/archives/{}/accounts", f.archive))
        .await;
    let row = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == account.to_string())
        .unwrap();
    assert_eq!(row["writable"], json!(["mail.flags"]));
    assert_eq!(row["needs_sign_in"], false);
    let (status, _) = f
        .change(message, json!({ "kind": "flags", "add": ["\\Seen"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
}
