use holtmail_core::{AccountId, ArchiveId};

use super::*;

impl Fixture {
    async fn account_id(&self) -> AccountId {
        let archive: ArchiveId = self.archive.parse().unwrap();
        self.store.list_accounts(archive).await.unwrap()[0].id
    }

    pub(super) async fn add_user(&self, email: &str, role: &str) -> (String, String) {
        let (status, _, body) = self
            .send(post_json(
                "/api/users",
                json!({ "email": email, "name": "Someone", "password": "difference engine", "role": role }),
            ))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
        let person: Value = serde_json::from_slice(&body).unwrap();
        let id = person["user"]["id"].as_str().unwrap().to_owned();
        (id, self.sign_in(email, "difference engine").await)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_account_capabilities_and_audits_them() {
    let f = fixture().await;
    let account = f.account_id().await;
    let uri = format!("/api/accounts/{account}/capabilities");
    let shown = f.json(&uri).await;
    assert_eq!(shown["capabilities"], json!([]));
    assert_eq!(
        shown["switches"],
        json!({ "enabled": true, "send": true, "rules": true })
    );

    let (status, _, body) = f
        .send(request(
            "PUT",
            &uri,
            json!({ "capabilities": ["mail.send", "mail.flags"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let shown = f.json(&uri).await;
    assert_eq!(shown["capabilities"], json!(["mail.flags", "mail.send"]));
    assert_eq!(shown["active"], json!(["mail.flags", "mail.send"]));

    for bad in [json!(["mail.everything"]), json!(["mail.expunge"])] {
        let (status, _, _) = f
            .send(request("PUT", &uri, json!({ "capabilities": bad })))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _, _) = f
        .send(request(
            "PUT",
            &format!("/api/accounts/{}/capabilities", AccountId::new()),
            json!({ "capabilities": [] }),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let audit = f.json(&format!("/api/audit?archive={}", f.archive)).await;
    let entries = audit.as_array().unwrap();
    assert_eq!(entries.len(), 1, "{audit}");
    assert_eq!(entries[0]["kind"], "capabilities_changed");
    assert_eq!(entries[0]["account"], account.to_string());
    assert_eq!(entries[0]["targets"], json!(["mail.flags", "mail.send"]));

    let (_, member) = f.add_user("grace@example.org", "member").await;
    let (status, _, _) = f
        .send_as(&member, Request::get(&uri).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, admin) = f.add_user("alan@example.org", "admin").await;
    let (status, _, _) = f
        .send_as(&admin, Request::get(&uri).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::OK, "admins manage capabilities");
    let (status, _, _) = f
        .send_as(
            &admin,
            Request::get("/api/audit").body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only the owner reads the audit log"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn grants_roles_and_only_the_owner_grants_writing() {
    let f = fixture().await;
    let (grace, _) = f.add_user("grace@example.org", "member").await;
    let (_, admin) = f.add_user("alan@example.org", "admin").await;
    let uri = format!("/api/users/{grace}/grants/{}", f.archive);

    let (status, _, _) = f
        .send_as(&admin, request("PUT", &uri, json!({ "role": "editor" })))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f
        .send_as(&admin, request("PUT", &uri, json!({ "role": "reader" })))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = f
        .send(request("PUT", &uri, json!({ "role": "sender" })))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = f
        .send(request("PUT", &uri, json!({ "role": "boss" })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let users = f.json("/api/users").await;
    let person = users
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["user"]["id"] == grace)
        .unwrap();
    assert_eq!(person["archives"], json!([f.archive]));
    assert_eq!(
        person["grants"],
        json!([{ "archive": f.archive, "role": "sender" }])
    );

    let (status, _, _) = f
        .send_as(&admin, Request::delete(&uri).body(Body::empty()).unwrap())
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an admin took a sender grant away"
    );
    for _ in 0..2 {
        let (status, _, _) = f
            .send(Request::delete(&uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let kinds: Vec<Value> = f
        .json("/api/audit")
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert_eq!(kinds, ["grant_changed"; 3]);
}

#[tokio::test(flavor = "multi_thread")]
async fn replays_a_request_with_the_same_idempotency_key() {
    let f = fixture().await;
    let create = |name: &str, key: Option<&str>| {
        let mut builder =
            Request::post("/api/archives").header(header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
        }
        builder
            .body(Body::from(json!({ "name": name }).to_string()))
            .unwrap()
    };
    let (status, headers, first) = f.send(create("Work", Some("key-1"))).await;
    assert_eq!(status, StatusCode::CREATED, "{}", body_text(&first));
    assert!(!headers.contains_key("idempotent-replayed"));
    let (status, headers, again) = f.send(create("Work", Some("key-1"))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(again, first);
    assert_eq!(headers["idempotent-replayed"], "true");
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");

    let (status, _, _) = f.send(create("Other", Some("key-1"))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, _) = f.send(create("Work", Some("has space"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = f.send(create("Work", Some("key-2"))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = f.send(create("Work", None)).await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, member) = f.add_user("grace@example.org", "admin").await;
    let (status, headers, _) = f.send_as(&member, create("Work", Some("key-1"))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        !headers.contains_key("idempotent-replayed"),
        "a key was shared between users"
    );

    let names: Vec<String> = f
        .store
        .list_archives()
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(
        names.iter().filter(|n| *n == "Work").count(),
        4,
        "{names:?}"
    );
}
