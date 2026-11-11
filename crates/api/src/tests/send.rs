use std::sync::Mutex;
use std::time::Duration;

use holtmail_blob::BlobId as BlobIdOf;
use holtmail_core::{ArchiveId, FolderId, MessageId};
use holtmail_outbox::{Archiver, Outbox, OutboxConfig, OutboxError, Transports};
use holtmail_store::{Capability, MessageAlias, Transport};
use holtmail_submit::{
    Envelope, Limits, RecipientOutcome, RecipientResult, Refusal, SubmitError, SubmitReport,
    Submitter,
};

use super::*;
use crate::Sending;

pub(super) type Submitted = Arc<Mutex<Vec<(Envelope, Vec<u8>)>>>;

struct FakeSubmitter(Submitted);

#[async_trait::async_trait]
impl Submitter for FakeSubmitter {
    async fn check(&self) -> Result<Limits, SubmitError> {
        Ok(Limits {
            max_size: Some(10_000_000),
            smtputf8: true,
        })
    }

    async fn submit(
        &self,
        envelope: &Envelope,
        message: &[u8],
    ) -> Result<SubmitReport, SubmitError> {
        self.0
            .lock()
            .unwrap()
            .push((envelope.clone(), message.to_vec()));
        Ok(SubmitReport {
            recipients: envelope
                .recipients
                .iter()
                .map(|address| RecipientResult {
                    address: address.clone(),
                    outcome: if address.starts_with("nobody@") {
                        RecipientOutcome::Refused(Refusal {
                            permanent: true,
                            code: 550,
                            text: "no such user".into(),
                        })
                    } else {
                        RecipientOutcome::Accepted
                    },
                })
                .collect(),
        })
    }

    fn limits(&self) -> Limits {
        Limits::default()
    }
}

struct FakeTransports(Arc<FakeSubmitter>);

#[async_trait::async_trait]
impl Transports for FakeTransports {
    async fn submitter(&self, _: &Transport) -> Result<Arc<dyn Submitter>, SubmitError> {
        Ok(self.0.clone())
    }

    async fn may_send(&self, _: ArchiveId, _: holtmail_core::UserId) -> Result<bool, OutboxError> {
        Ok(true)
    }
}

struct FakeArchiver(Ingest);

#[async_trait::async_trait]
impl Archiver for FakeArchiver {
    async fn archive(
        &self,
        archive: ArchiveId,
        raw: Bytes,
        folder: Option<FolderId>,
        flags: &[String],
    ) -> Result<MessageId, OutboxError> {
        let done = self
            .0
            .ingest(
                archive,
                raw,
                &Placement {
                    folder,
                    flags: flags.to_vec(),
                    ..Placement::default()
                },
            )
            .await
            .map_err(|e| OutboxError::Archive(e.to_string()))?;
        self.0
            .commit()
            .await
            .map_err(|e| OutboxError::Archive(e.to_string()))?;
        Ok(done.message_id)
    }
}

pub(super) fn fake_sending(
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    ingest: Ingest,
) -> (Sending, Submitted) {
    let submitted = Submitted::default();
    let transports = Arc::new(FakeTransports(Arc::new(FakeSubmitter(submitted.clone()))));
    let outbox = Arc::new(Outbox::new(
        store.clone(),
        blobs.clone(),
        transports.clone(),
        Arc::new(FakeArchiver(ingest)),
        OutboxConfig {
            undo_window: Duration::ZERO,
            ..OutboxConfig::default()
        },
    ));
    let submission = Arc::new(holtmail_submission_outbox::OutboxSubmission::new(
        store.clone(),
        blobs,
        holtmail_access::Access::new(store, holtmail_access::WriteSwitches::default()),
        outbox.clone(),
        holtmail_submission_outbox::Options::default(),
    ));
    let scheduler = Arc::new(holtmail_schedule_itip::ItipScheduler::new(
        submission.clone(),
        holtmail_schedule::system_clock(),
    ));
    (
        Sending {
            outbox,
            transports,
            secrets: Arc::new(PlainSecrets),
            submission,
            scheduler,
        },
        submitted,
    )
}

fn as_user(cookie: &str, method: &str, uri: &str, body: Value) -> Request<Body> {
    let mut request = request(method, uri, body);
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    request
}

impl Fixture {
    pub(super) async fn call(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, _, bytes) = self.send(request(method, uri, body)).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    pub(super) async fn ok(&self, method: &str, uri: &str, body: Value) -> Value {
        let (status, value) = self.call(method, uri, body).await;
        assert!(status.is_success(), "{method} {uri}: {status} {value}");
        value
    }

    pub(super) async fn smtp_transport(&self) -> Value {
        self.ok(
            "POST",
            &format!("/api/archives/{}/transports", self.archive),
            json!({
                "name": "Relay",
                "kind": "smtp",
                "host": "smtp.example.com",
                "username": "ada",
                "password": "relay secret 42",
                "allowed_from": ["*@example.com"],
            }),
        )
        .await
    }

    pub(super) async fn sending_identity(&self) -> (String, String) {
        let transport = self.smtp_transport().await;
        let identity = self
            .ok(
                "POST",
                &format!("/api/archives/{}/identities", self.archive),
                json!({
                    "address": "ada@example.com",
                    "name": "Ada Lovelace",
                    "transport": transport["id"],
                    "active": true,
                }),
            )
            .await;
        (
            transport["id"].as_str().unwrap().to_owned(),
            identity["id"].as_str().unwrap().to_owned(),
        )
    }

    pub(super) async fn message_with_subject(&self, subject: &str) -> String {
        let page = self
            .json(&format!("/api/archives/{}/messages", self.archive))
            .await;
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["subject"] == subject)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    pub(super) async fn audit_kinds(&self) -> Vec<String> {
        let audit = self.json("/api/audit").await;
        audit
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sets_up_transports_and_identities_without_showing_secrets() {
    let f = fixture().await;
    let transport = f.smtp_transport().await;
    let id = transport["id"].as_str().unwrap();
    assert_eq!(transport["has_password"], true);
    assert_eq!(transport["port"], 587);
    assert_eq!(transport["security"], "starttls");
    let listed = f
        .json(&format!("/api/archives/{}/transports", f.archive))
        .await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert!(!listed.to_string().contains("relay secret 42"));
    let stored = f
        .store
        .get_transport(id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.sealed_secret.as_deref(),
        Some(&b"relay secret 42"[..])
    );

    let check = f
        .ok("POST", &format!("/api/transports/{id}/check"), json!({}))
        .await;
    assert_eq!(check["ok"], true, "{check}");
    assert_eq!(check["max_size"], 10_000_000);

    for bad in [
        json!({ "name": "x", "kind": "smtp", "host": "h.example.com", "allowed_from": ["*"] }),
        json!({ "name": "x", "kind": "smtp", "allowed_from": ["*@example.com"] }),
        json!({ "name": "x", "kind": "smtp", "host": "h.example.com", "username": "ada" }),
        json!({ "name": "x", "kind": "account" }),
    ] {
        let (status, body) = f
            .call(
                "POST",
                &format!("/api/archives/{}/transports", f.archive),
                bad.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }

    let uri = format!("/api/archives/{}/identities", f.archive);
    let draft = f
        .ok(
            "POST",
            &uri,
            json!({ "address": "ADA@example.com", "transport": id }),
        )
        .await;
    assert_eq!(draft["active"], false);
    assert_eq!(draft["address"], "ada@example.com");
    assert_eq!(draft["sent_copy"], "none");
    assert_eq!(draft["users"].as_array().unwrap().len(), 1);
    let (status, body) = f
        .call(
            "POST",
            &uri,
            json!({ "address": "ada@example.org", "transport": id, "active": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let identity = draft["id"].as_str().unwrap();
    let active = f
        .ok(
            "PATCH",
            &format!("/api/identities/{identity}"),
            json!({ "active": true, "signature_text": "-- \nAda" }),
        )
        .await;
    assert_eq!(active["active"], true);
    f.ok(
        "PUT",
        &format!("/api/archives/{}/default-identity", f.archive),
        json!({ "identity": identity }),
    )
    .await;
    let listed = f.json(&uri).await;
    assert_eq!(listed[0]["default"], true);

    let (status, _) = f
        .call("DELETE", &format!("/api/transports/{id}"), json!({}))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "removed a transport in use"
    );
    let suggestions = f.json(&format!("{uri}/suggestions")).await;
    assert!(suggestions.is_array());

    let kinds = f.audit_kinds().await;
    assert!(kinds.iter().any(|k| k == "transport_changed"), "{kinds:?}");
    assert!(kinds.iter().any(|k| k == "identity_changed"), "{kinds:?}");
    assert!(
        !f.json("/api/audit")
            .await
            .to_string()
            .contains("ada@example.com")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_a_draft_through_the_outbox_and_undoes_another() {
    let f = fixture().await;
    let (_, identity) = f.sending_identity().await;
    let drafts = format!("/api/archives/{}/drafts", f.archive);
    let draft = f
        .ok(
            "POST",
            &drafts,
            json!({ "payload": {
                "to": [{ "name": "Bob", "address": "bob@example.org" }],
                "bcc": [{ "address": "nobody@example.org" }],
                "subject": "Plans for Thursday",
                "text": "Shall we meet at noon?",
            } }),
        )
        .await;
    assert_eq!(draft["version"], 1);
    assert_eq!(draft["route"]["identity"], identity);
    assert_eq!(draft["route"]["reasons"][0]["code"], "default");
    let id = draft["id"].as_str().unwrap();
    let saved = f
        .ok(
            "PUT",
            &format!("/api/drafts/{id}"),
            json!({ "version": 1, "payload": {
                "to": [{ "name": "Bob", "address": "bob@example.org" }],
                "bcc": [{ "address": "nobody@example.org" }],
                "subject": "Plans for Thursday",
                "text": "Shall we meet at noon? Or at one?",
            } }),
        )
        .await;
    assert_eq!(saved["version"], 2);
    let (status, stale) = f
        .call(
            "PUT",
            &format!("/api/drafts/{id}"),
            json!({ "version": 1, "payload": {} }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["version"], 2);
    let versions = f.json(&format!("/api/drafts/{id}/versions")).await;
    assert_eq!(versions.as_array().unwrap().len(), 2);

    let queued = f
        .ok("POST", &format!("/api/drafts/{id}/send"), json!({}))
        .await;
    assert_eq!(queued["state"], "queued", "{queued}");
    assert_eq!(queued["mail_from"], "ada@example.com");
    let (status, _) = f.call("GET", &format!("/api/drafts/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the sent draft stayed");

    let sending = f.state.sending.clone().unwrap();
    assert_eq!(sending.outbox.send_due().await.unwrap(), 1);
    let (envelope, bytes) = f.submitted.lock().unwrap()[0].clone();
    assert_eq!(
        envelope.recipients,
        vec!["bob@example.org", "nobody@example.org"]
    );
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Subject: Plans for Thursday"));
    assert!(!text.contains("nobody@example.org"), "Bcc in the bytes");

    let sent = f
        .json(&format!("/api/outbox/{}", queued["id"].as_str().unwrap()))
        .await;
    assert_eq!(sent["state"], "sent", "{sent}");
    assert_eq!(sent["recipients"][0]["state"], "accepted");
    assert_eq!(sent["recipients"][1]["state"], "refused");
    assert_eq!(sent["recipients"][1]["code"], 550);
    let message = sent["message"].as_str().unwrap();
    let (status, _, raw) = f.get(&format!("/api/messages/{message}/raw")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&raw[..], &bytes[..], "the archived copy differs");

    let resent = f
        .ok(
            "POST",
            &format!("/api/outbox/{}/resend", queued["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(resent["recipients"].as_array().unwrap().len(), 1);
    assert_eq!(resent["recipients"][0]["address"], "nobody@example.org");
    f.ok(
        "POST",
        &format!("/api/outbox/{}/cancel", resent["id"].as_str().unwrap()),
        json!({}),
    )
    .await;

    let second = f
        .ok(
            "POST",
            &drafts,
            json!({ "payload": {
                "to": [{ "address": "carol@example.org" }],
                "bcc": [{ "address": "dave@example.org" }],
                "subject": "Never mind",
                "text": "Sent too early.",
            } }),
        )
        .await;
    let queued = f
        .ok(
            "POST",
            &format!("/api/drafts/{}/send", second["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    let undone = f
        .ok(
            "POST",
            &format!("/api/outbox/{}/undo", queued["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(undone["outbox"]["state"], "cancelled");
    assert_eq!(undone["draft"]["payload"]["subject"], "Never mind");
    assert_eq!(
        undone["draft"]["payload"]["to"][0]["address"],
        "carol@example.org"
    );
    assert_eq!(
        undone["draft"]["payload"]["bcc"][0]["address"],
        "dave@example.org"
    );
    assert_eq!(sending.outbox.send_due().await.unwrap(), 0);
    assert_eq!(
        f.submitted.lock().unwrap().len(),
        1,
        "the undone message left"
    );
    let (status, _) = f
        .call(
            "POST",
            &format!("/api/outbox/{}/undo", queued["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let outbox = f
        .json(&format!("/api/archives/{}/outbox?state=sent", f.archive))
        .await;
    assert_eq!(outbox.as_array().unwrap().len(), 1);
    let kinds = f.audit_kinds().await;
    for kind in ["outbox_changed", "mail_send"] {
        assert!(kinds.iter().any(|k| k == kind), "{kind}: {kinds:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_and_forwards_with_the_route_explained() {
    let f = fixture().await;
    let (_, identity) = f.sending_identity().await;
    let plain = f.message_with_subject("Re: Weekly news").await;
    let reply = f
        .ok(
            "POST",
            &format!("/api/messages/{plain}/reply"),
            json!({ "kind": "sender" }),
        )
        .await;
    assert_eq!(reply["payload"]["to"][0]["address"], "bob@example.org");
    assert_eq!(reply["payload"]["subject"], "Re: Weekly news");
    assert_eq!(reply["payload"]["context"]["kind"], "reply");
    assert_eq!(reply["payload"]["identity"], Value::Null);
    assert_eq!(reply["route"]["identity"], identity);
    assert_eq!(reply["route"]["from"], "ada@example.com");
    assert_eq!(
        reply["route"]["reasons"][0],
        json!({ "code": "original_recipient", "address": "ada@example.com", "alias": null, "account": null })
    );
    assert!(
        reply["payload"]["text"]
            .as_str()
            .unwrap()
            .contains("> Thanks <3")
    );
    let (status, _) = f
        .call(
            "POST",
            &format!("/api/messages/{plain}/reply"),
            json!({ "kind": "list" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let news = f.message_with_subject("Weekly news").await;
    let forward = f
        .ok(
            "POST",
            &format!("/api/messages/{news}/forward"),
            json!({ "kind": "attachment" }),
        )
        .await;
    assert_eq!(forward["payload"]["subject"], "Fwd: Weekly news");
    let attachments = forward["payload"]["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["content_type"], "message/rfc822");

    let draft = forward["id"].as_str().unwrap();
    let with_file = f
        .send(
            Request::post(format!(
                "/api/drafts/{draft}/attachments?name=notes.txt&content_type=text/plain"
            ))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from("Bring the plans."))
            .unwrap(),
        )
        .await;
    assert_eq!(with_file.0, StatusCode::OK, "{}", body_text(&with_file.2));
    let with_file: Value = serde_json::from_slice(&with_file.2).unwrap();
    assert_eq!(with_file["payload"]["attachments"][1]["name"], "notes.txt");
    let mut payload = with_file["payload"].clone();
    payload["to"] = json!([{ "address": "carol@example.org" }]);
    payload["attachments"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "message": news, "ordinal": 0 }));
    let saved = f
        .ok(
            "PUT",
            &format!("/api/drafts/{draft}"),
            json!({ "version": with_file["version"], "payload": payload }),
        )
        .await;
    assert_eq!(
        saved["payload"]["attachments"][2]["content_type"],
        "image/png"
    );
    let mut forged = saved["payload"].clone();
    forged["attachments"] = json!([{ "blob": BlobIdOf::of(b"elsewhere").to_string() }]);
    let (status, _) = f
        .call(
            "PUT",
            &format!("/api/drafts/{draft}"),
            json!({ "version": saved["version"], "payload": forged }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "attached a foreign blob");

    f.ok("POST", &format!("/api/drafts/{draft}/send"), json!({}))
        .await;
    f.state
        .sending
        .clone()
        .unwrap()
        .outbox
        .send_due()
        .await
        .unwrap();
    let (_, bytes) = f.submitted.lock().unwrap()[0].clone();
    let parsed = holtmail_mime::parse(&bytes).unwrap();
    assert_eq!(parsed.attachments.len(), 3);
    assert_eq!(parsed.subject.as_deref(), Some("Fwd: Weekly news"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_only_with_a_sender_grant_the_switch_and_the_capability() {
    let f = fixture().await;
    let (transport, identity) = f.sending_identity().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let drafts = format!("/api/archives/{}/drafts", f.archive);

    let (grace, cookie) = f.add_member("grace@example.org").await;
    f.store
        .set_grants(grace.parse().unwrap(), &[archive])
        .await
        .unwrap();
    let (status, _, _) = f
        .send(as_user(&cookie, "POST", &drafts, json!({ "payload": {} })))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a reader wrote a draft");
    f.ok(
        "PUT",
        &format!("/api/users/{grace}/grants/{}", f.archive),
        json!({ "role": "sender" }),
    )
    .await;
    let (status, _, body) = f
        .send(as_user(
            &cookie,
            "POST",
            &drafts,
            json!({ "payload": { "to": [{ "address": "bob@example.org" }], "subject": "Hi" } }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
    let draft: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(draft["route_error"]["code"], "no_identity");
    let (status, _, _) = f
        .send(as_user(
            &cookie,
            "POST",
            &format!("/api/drafts/{}/send", draft["id"].as_str().unwrap()),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "sent without an identity");
    let (status, _, _) = f
        .send(as_user(
            &cookie,
            "POST",
            &format!("/api/archives/{}/transports", f.archive),
            json!({ "name": "Mine", "kind": "smtp", "host": "h.example.com", "allowed_from": ["*@example.org"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a member added a transport");
    let (status, _) = f
        .call(
            "GET",
            &format!("/api/drafts/{}", draft["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the owner read a member's draft"
    );

    let mine = f
        .ok(
            "POST",
            &drafts,
            json!({ "payload": { "to": [{ "address": "bob@example.org" }], "subject": "Hi" } }),
        )
        .await;
    let off = ApiState {
        write: WriteSwitches {
            send: false,
            ..WriteSwitches::default()
        },
        ..f.state.clone()
    };
    let app = router(off);
    let mut request = request(
        "POST",
        &format!("/api/drafts/{}/send", mine["id"].as_str().unwrap()),
        json!({}),
    );
    let headers = request.headers_mut();
    headers.insert(header::HOST, "mail.example.com".parse().unwrap());
    headers.insert(header::COOKIE, f.cookie.parse().unwrap());
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "the send switch is off"
    );

    let account = f.store.list_accounts(archive).await.unwrap()[0].id;
    let through_account = f
        .ok(
            "POST",
            &format!("/api/archives/{}/transports", f.archive),
            json!({ "name": "Account", "kind": "account", "account": account.to_string(), "allowed_from": ["ada@example.com"] }),
        )
        .await;
    f.ok(
        "PATCH",
        &format!("/api/identities/{identity}"),
        json!({ "transport": through_account["id"] }),
    )
    .await;
    let (status, body) = f
        .call(
            "POST",
            &format!("/api/drafts/{}/send", mine["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    f.store
        .set_account_capabilities(account, &[Capability::MailSend].into_iter().collect())
        .await
        .unwrap();
    f.ok(
        "POST",
        &format!("/api/drafts/{}/send", mine["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let _ = transport;
}

impl Fixture {
    pub(super) async fn add_member(&self, email: &str) -> (String, String) {
        let (status, _, body) = self
            .send(post_json(
                "/api/users",
                json!({ "email": email, "name": "Someone", "password": "difference engine", "role": "member" }),
            ))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
        let person: Value = serde_json::from_slice(&body).unwrap();
        let id = person["user"]["id"].as_str().unwrap().to_owned();
        (id, self.sign_in(email, "difference engine").await)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_carry_rights_and_the_web_sees_lists_and_subjects() {
    let f = fixture().await;
    let archives = f.json("/api/archives").await;
    assert_eq!(
        archives[0]["rights"],
        json!({ "role": "sender", "manage": true, "compose": false }),
        "composing without an identity"
    );
    let (_, identity) = f.sending_identity().await;
    let archives = f.json("/api/archives").await;
    assert_eq!(archives[0]["rights"]["compose"], true);

    let archive: ArchiveId = f.archive.parse().unwrap();
    let (grace, cookie) = f.add_member("grace@example.org").await;
    f.store
        .set_grants(grace.parse().unwrap(), &[archive])
        .await
        .unwrap();
    let (_, _, body) = f
        .send(as_user(&cookie, "GET", "/api/archives", json!({})))
        .await;
    let seen: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        seen[0]["rights"],
        json!({ "role": "reader", "manage": false, "compose": false })
    );
    f.ok(
        "PUT",
        &format!("/api/users/{grace}/grants/{}", f.archive),
        json!({ "role": "sender" }),
    )
    .await;
    let (_, _, body) = f
        .send(as_user(
            &cookie,
            "GET",
            &format!("/api/archives/{}", f.archive),
            json!({}),
        ))
        .await;
    let seen: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(seen["rights"]["compose"], false, "no identity to send as");
    let owner = f.json("/api/users").await[0]["user"]["id"].clone();
    f.ok(
        "PUT",
        &format!("/api/identities/{identity}/users"),
        json!({ "users": [owner, grace] }),
    )
    .await;
    let (_, _, body) = f
        .send(as_user(
            &cookie,
            "GET",
            &format!("/api/archives/{}", f.archive),
            json!({}),
        ))
        .await;
    let seen: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(seen["rights"]["compose"], true);

    let (_, admin) = f.add_member("hopper@example.org").await;
    let hopper = f
        .json("/api/users")
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["user"]["email"] == "hopper@example.org")
        .unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.ok(
        "PATCH",
        &format!("/api/users/{hopper}"),
        json!({ "role": "admin" }),
    )
    .await;
    let (status, _, _) = f
        .send(as_user(
            &admin,
            "PUT",
            &format!("/api/users/{grace}/grants/{}", f.archive),
            json!({ "role": "reader" }),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an admin took away sending");

    f.ingest
        .ingest(
            archive,
            Bytes::from_static(
                b"From: Bob <bob@example.org>\r\nTo: dev@lists.example.org\r\nSubject: Release notes\r\nList-Post: <mailto:dev@lists.example.org>\r\nMessage-ID: <l1@example.org>\r\nDate: Wed, 12 Mar 2025 08:00:00 +0000\r\n\r\nShipped.\r\n",
            ),
            &Placement::default(),
        )
        .await
        .unwrap();
    f.ingest.commit().await.unwrap();
    let list = f.message_with_subject("Release notes").await;
    let detail = f.json(&format!("/api/messages/{list}")).await;
    assert_eq!(detail["list_post"], "dev@lists.example.org");
    let plain = f.message_with_subject("Re: Weekly news").await;
    let detail = f.json(&format!("/api/messages/{plain}")).await;
    assert_eq!(detail["list_post"], Value::Null);

    let draft = f
        .ok(
            "POST",
            &format!("/api/archives/{}/drafts", f.archive),
            json!({ "payload": { "to": [{ "address": "bob@example.org" }], "subject": "Café plans", "text": "Noon?" } }),
        )
        .await;
    let queued = f
        .ok(
            "POST",
            &format!("/api/drafts/{}/send", draft["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(queued["subject"], "Café plans");
    let outbox = f.json(&format!("/api/archives/{}/outbox", f.archive)).await;
    assert_eq!(outbox[0]["subject"], "Café plans");
}

#[tokio::test(flavor = "multi_thread")]
async fn reply_all_leaves_out_own_aliases() {
    let f = fixture().await;
    f.sending_identity().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    for alias in ["shop@example.net", "news-alias@example.net"] {
        f.store
            .put_alias(
                archive,
                &holtmail_store::KnownAlias {
                    address: alias.into(),
                    provider: "custom".into(),
                    service_id: None,
                    remote_id: None,
                    enabled: None,
                    note: None,
                    name: None,
                    for_domain: None,
                    mailboxes: vec![],
                    created_at: None,
                    forwarded: None,
                    blocked: None,
                    replied: None,
                },
            )
            .await
            .unwrap();
    }
    f.ingest
        .ingest(
            archive,
            Bytes::from_static(
                b"From: Bob <bob@example.org>\r\nTo: shop@example.net\r\nCc: Carol <carol@example.org>, news-alias@example.net\r\nSubject: Your order\r\nMessage-ID: <order-1@example.org>\r\nDate: Wed, 12 Mar 2025 08:00:00 +0000\r\n\r\nShipped.\r\n",
            ),
            &Placement::default(),
        )
        .await
        .unwrap();
    f.ingest.commit().await.unwrap();
    let order = f.message_with_subject("Your order").await;
    let reply = f
        .ok(
            "POST",
            &format!("/api/messages/{order}/reply"),
            json!({ "kind": "all" }),
        )
        .await;
    let addresses: Vec<&str> = reply["payload"]["to"]
        .as_array()
        .unwrap()
        .iter()
        .chain(reply["payload"]["cc"].as_array().unwrap())
        .map(|m| m["address"].as_str().unwrap())
        .collect();
    assert!(addresses.contains(&"bob@example.org"), "{addresses:?}");
    assert!(addresses.contains(&"carol@example.org"), "{addresses:?}");
    assert!(
        !addresses.iter().any(|a| a.ends_with("@example.net")),
        "replied to an own alias: {addresses:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn admins_cannot_take_away_or_take_over_writing_grants() {
    let f = fixture().await;
    let (status, _, body) = f
        .send(post_json(
            "/api/users",
            json!({ "email": "root@example.com", "name": "Root", "password": "difference engine", "role": "admin" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
    let admin = f.sign_in("root@example.com", "difference engine").await;
    let (member, _) = f.add_member("grace@example.com").await;
    f.ok(
        "PUT",
        &format!("/api/users/{member}/grants/{}", f.archive),
        json!({ "role": "sender" }),
    )
    .await;
    let as_admin =
        |method: &str, body: Value| as_user(&admin, method, &format!("/api/users/{member}"), body);
    let (status, _, _) = f.send(as_admin("PATCH", json!({ "archives": [] }))).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an admin took away a sender grant"
    );
    let (status, _, _) = f
        .send(as_admin(
            "PATCH",
            json!({ "password": "another engine 42" }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an admin reset a sender's password"
    );
    let (status, _, _) = f
        .send(as_admin("PATCH", json!({ "name": "Grace H." })))
        .await;
    assert_eq!(status, StatusCode::OK);
    let grants = f.json("/api/users").await;
    let person = grants
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["user"]["id"] == member.as_str())
        .unwrap();
    assert_eq!(person["grants"][0]["role"], "sender");
    f.ok(
        "PATCH",
        &format!("/api/users/{member}"),
        json!({ "archives": [] }),
    )
    .await;
    assert!(f.audit_kinds().await.iter().any(|k| k == "grant_changed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_secret_never_follows_a_new_host() {
    let f = fixture().await;
    let transport = f.smtp_transport().await;
    let uri = format!("/api/transports/{}", transport["id"].as_str().unwrap());
    let (status, _) = f
        .call("PATCH", &uri, json!({ "host": "smtp.example.net" }))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the password followed a new host"
    );
    f.ok(
        "PATCH",
        &uri,
        json!({ "host": "smtp.example.net", "password": "relay secret 43" }),
    )
    .await;
    f.ok("PATCH", &uri, json!({ "name": "Relay 2" })).await;

    let archive: ArchiveId = f.archive.parse().unwrap();
    let account = f.store.list_accounts(archive).await.unwrap()[0].id;
    let (status, _) = f
        .call(
            "POST",
            &format!("/api/archives/{}/transports", f.archive),
            json!({ "name": "Own", "kind": "account", "account": account.to_string(), "host": "smtp.example.net", "allowed_from": ["*@example.com"] }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an account's own sign-in was pointed at another host"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn jmap_and_graph_transports_and_what_sync_learns_after_sending() {
    let f = fixture().await;
    let transports = format!("/api/archives/{}/transports", f.archive);
    let jmap = f
        .ok(
            "POST",
            &transports,
            json!({ "name": "Stalwart", "kind": "jmap", "url": "jmap.example.com", "username": "ada", "password": "jmap secret 7", "allowed_from": ["*@example.com"] }),
        )
        .await;
    assert_eq!(jmap["kind"], "jmap");
    assert_eq!(jmap["url"], "https://jmap.example.com/.well-known/jmap");
    assert_eq!(jmap["auth"], "password");
    assert_eq!(jmap["has_password"], true);
    assert!(!jmap.to_string().contains("jmap secret 7"));
    let graph = f
        .ok(
            "POST",
            &transports,
            json!({ "name": "Graph", "kind": "graph", "password": "graph token", "mailbox": "ada@example.com", "allowed_from": ["*@example.com"] }),
        )
        .await;
    assert_eq!(graph["url"], Value::Null);
    assert_eq!(graph["mailbox"], "ada@example.com");
    for bad in [
        json!({ "name": "x", "kind": "jmap", "username": "ada", "password": "p" }),
        json!({ "name": "x", "kind": "jmap", "url": "http://jmap.example.com", "username": "ada", "password": "p" }),
        json!({ "name": "x", "kind": "jmap", "url": "jmap.example.com", "password": "p" }),
        json!({ "name": "x", "kind": "jmap", "url": "jmap.example.com", "auth": "magic", "password": "p" }),
        json!({ "name": "x", "kind": "graph" }),
        json!({ "name": "x", "kind": "graph", "password": "t", "account": "x" }),
    ] {
        let (status, body) = f.call("POST", &transports, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let token = f
        .ok(
            "POST",
            &transports,
            json!({ "name": "Token", "kind": "jmap", "url": "http://localhost:8080", "auth": "token", "password": "t" }),
        )
        .await;
    assert_eq!(token["url"], "http://localhost:8080/.well-known/jmap");
    let uri = format!("/api/transports/{}", jmap["id"].as_str().unwrap());
    let (status, _) = f
        .call("PATCH", &uri, json!({ "url": "jmap.example.net" }))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the password followed a new address"
    );
    let (status, _) = f
        .call("PATCH", &uri, json!({ "host": "smtp.example.net" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let moved = f
        .ok(
            "PATCH",
            &uri,
            json!({ "url": "jmap.example.net", "password": "jmap secret 8" }),
        )
        .await;
    assert_eq!(moved["url"], "https://jmap.example.net/.well-known/jmap");
    let (status, _) = f
        .call("PATCH", &uri, json!({ "auth": "token", "username": "" }))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the stored password became a token"
    );
    f.ok(
        "PATCH",
        &uri,
        json!({ "auth": "password", "username": "ada" }),
    )
    .await;
    let smtp = f.smtp_transport().await;
    let (status, _) = f
        .call(
            "PATCH",
            &format!("/api/transports/{}", smtp["id"].as_str().unwrap()),
            json!({ "url": "jmap.example.net" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let identities = format!("/api/archives/{}/identities", f.archive);
    let identity = f
        .ok(
            "POST",
            &identities,
            json!({ "address": "ada@example.com", "transport": jmap["id"], "active": true }),
        )
        .await;
    assert_eq!(identity["sent_copy"], "provider");
    assert_eq!(identity["rewritten_from"], Value::Null);
    let id: holtmail_core::IdentityId = identity["id"].as_str().unwrap().parse().unwrap();
    f.store
        .set_identity_rewritten_from(
            id,
            Some(&holtmail_store::RewrittenFrom {
                seen: "ada.example@mail.example.net".into(),
                at: holtmail_core::Timestamp::from_secs(1_800_000_000),
            }),
        )
        .await
        .unwrap();
    let listed = f.json(&identities).await;
    let warned = &listed[0]["rewritten_from"];
    assert_eq!(warned["seen"], "ada.example@mail.example.net");
    assert!(
        warned["explanation"]
            .as_str()
            .unwrap()
            .contains("Send mail as"),
        "{warned}"
    );

    let drafts = format!("/api/archives/{}/drafts", f.archive);
    let draft = f
        .ok(
            "POST",
            &drafts,
            json!({ "payload": {
                "to": [{ "address": "bob@example.org" }],
                "subject": "Plans for Friday",
                "text": "Lunch?",
            } }),
        )
        .await;
    let queued = f
        .ok(
            "POST",
            &format!("/api/drafts/{}/send", draft["id"].as_str().unwrap()),
            json!({ "identity": identity["id"] }),
        )
        .await;
    let sending = f.state.sending.clone().unwrap();
    assert_eq!(sending.outbox.send_due().await.unwrap(), 1);
    let outbox: holtmail_core::OutboxId = queued["id"].as_str().unwrap().parse().unwrap();
    let change = holtmail_store::OutboxChange {
        recipients: Some(vec![holtmail_store::OutboxRecipient {
            address: "bob@example.org".into(),
            state: holtmail_store::RecipientState::Bounced {
                status: Some("5.1.1".into()),
                text: Some("550 5.1.1 no such user".into()),
            },
        }]),
        ..holtmail_store::OutboxChange::to(
            holtmail_store::OutboxState::Bounced,
            holtmail_core::Timestamp::now(),
        )
    };
    f.store
        .change_outbox(outbox, &[holtmail_store::OutboxState::Sent], &change)
        .await
        .unwrap()
        .expect("sent first");
    f.store
        .set_outbox_note(outbox, Some("checked"), holtmail_core::Timestamp::now())
        .await
        .unwrap();
    let shown = f.json(&format!("/api/outbox/{outbox}")).await;
    assert_eq!(shown["state"], "bounced");
    assert_eq!(shown["recipients"][0]["state"], "bounced");
    assert_eq!(shown["recipients"][0]["status"], "5.1.1");
    assert_eq!(shown["recipients"][0]["text"], "550 5.1.1 no such user");
    assert_eq!(shown["note"], "checked");
    assert_eq!(shown["synced_copy"], Value::Null);
}

fn known_alias(
    address: &str,
    provider: &str,
    service: Option<holtmail_core::AliasServiceId>,
    remote: Option<&str>,
) -> holtmail_store::KnownAlias {
    holtmail_store::KnownAlias {
        address: address.into(),
        provider: provider.into(),
        service_id: service,
        remote_id: remote.map(str::to_owned),
        enabled: Some(true),
        note: None,
        name: None,
        for_domain: None,
        mailboxes: vec!["ada@example.com".into()],
        created_at: None,
        forwarded: None,
        blocked: None,
        replied: None,
    }
}

impl Fixture {
    async fn alias_mail(&self, raw: &'static [u8], subject: &str, hit: MessageAlias) -> String {
        let archive: ArchiveId = self.archive.parse().unwrap();
        self.ingest
            .ingest(archive, Bytes::from_static(raw), &Placement::default())
            .await
            .unwrap();
        self.ingest.commit().await.unwrap();
        let id = self.message_with_subject(subject).await;
        self.store
            .set_message_alias(id.parse().unwrap(), Some(&hit))
            .await
            .unwrap();
        id
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_leaves_from_an_alias_its_identity_sends_as() {
    let f = fixture().await;
    let transport = f
        .ok(
            "POST",
            &format!("/api/archives/{}/transports", f.archive),
            json!({
                "name": "Relay",
                "kind": "smtp",
                "host": "smtp.example.com",
                "allowed_from": ["*@example.com", "*@sl.example.org"],
            }),
        )
        .await;
    let identity = f
        .ok(
            "POST",
            &format!("/api/archives/{}/identities", f.archive),
            json!({
                "address": "ada@example.com",
                "name": "Ada Lovelace",
                "alias": "shop@sl.example.org",
                "transport": transport["id"],
                "active": true,
            }),
        )
        .await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    f.store
        .put_alias(
            archive,
            &known_alias("shop@sl.example.org", "simplelogin", None, None),
        )
        .await
        .unwrap();
    f.ok(
        "PUT",
        &format!("/api/archives/{}/alias-strategies", f.archive),
        json!({ "scope": "alias", "key": "shop@sl.example.org", "strategy": "direct" }),
    )
    .await;
    let order = f
        .alias_mail(
            b"From: Bob <bob@example.org>\r\nTo: shop@sl.example.org\r\nSubject: Your parcel\r\nDate: Sun, 08 Nov 2026 09:00:00 +0000\r\nMessage-ID: <parcel@example.org>\r\n\r\nHello\r\n",
            "Your parcel",
            MessageAlias {
                provider: "simplelogin".into(),
                alias: "shop@sl.example.org".into(),
                sender: None,
                reverse_alias: None,
                signed: true,
            },
        )
        .await;
    let reply = f
        .ok("POST", &format!("/api/messages/{order}/reply"), json!({}))
        .await;
    assert_eq!(reply["route"]["identity"], identity["id"], "{reply}");
    assert_eq!(reply["route"]["strategy"], "direct");
    assert_eq!(reply["route"]["from"], "shop@sl.example.org");
    let draft = reply["id"].as_str().unwrap();
    f.ok("POST", &format!("/api/drafts/{draft}/send"), json!({}))
        .await;
    for _ in 0..2 {
        f.state
            .sending
            .clone()
            .unwrap()
            .outbox
            .send_due()
            .await
            .unwrap();
    }
    let submitted = f.submitted.lock().unwrap().clone();
    let (envelope, bytes) = submitted.first().expect("the reply left");
    assert_eq!(envelope.mail_from, "shop@sl.example.org");
    let parsed = holtmail_mime::parse(bytes).unwrap();
    assert_eq!(parsed.from[0].email, "shop@sl.example.org");
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_through_a_reverse_alias_and_guards_against_leaks() {
    let f = fixture().await;
    let (_, identity) = f.sending_identity().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    f.store
        .put_alias(
            archive,
            &known_alias("shop@sl.example.org", "simplelogin", None, None),
        )
        .await
        .unwrap();
    let order = f
        .alias_mail(
            b"From: \"Bob - bob at example.org\" <ra_bob@sl.example.org>\r\nTo: shop@sl.example.org\r\nCc: Carol <carol@example.org>\r\nSubject: Your order\r\nMessage-ID: <order-2@example.org>\r\nDate: Wed, 12 Mar 2025 08:00:00 +0000\r\n\r\nShipped.\r\n",
            "Your order",
            MessageAlias {
                provider: "simplelogin".into(),
                alias: "shop@sl.example.org".into(),
                sender: Some(holtmail_core::Address {
                    name: Some("Bob".into()),
                    email: "bob@example.org".into(),
                }),
                reverse_alias: Some("ra_bob@sl.example.org".into()),
                signed: true,
            },
        )
        .await;

    let reply = f
        .ok(
            "POST",
            &format!("/api/messages/{order}/reply"),
            json!({ "kind": "sender" }),
        )
        .await;
    assert_eq!(reply["route"]["identity"], identity);
    assert_eq!(reply["route"]["strategy"], "reverse_alias");
    assert_eq!(reply["route"]["from"], "ada@example.com");
    assert_eq!(reply["route"]["to"], "ra_bob@sl.example.org");
    assert_eq!(reply["route"]["alias"], "shop@sl.example.org");
    assert_eq!(reply["route"]["strategy_source"], "default");
    assert_eq!(reply["route"]["provider"], "simplelogin");
    assert_eq!(reply["route"]["contact"], "bob@example.org");
    let codes: Vec<&str> = reply["route"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["alias_mailbox", "reverse_alias"]);
    assert_eq!(
        reply["payload"]["to"],
        json!([{ "name": "Bob - bob at example.org", "address": "ra_bob@sl.example.org" }])
    );
    assert_eq!(reply["leaks"], json!([]));

    let all = f
        .ok(
            "POST",
            &format!("/api/messages/{order}/reply"),
            json!({ "kind": "all" }),
        )
        .await;
    assert_eq!(
        all["leaks"],
        json!([{ "recipient": "carol@example.org", "field": "cc", "reason": "cc_not_reverse_alias", "fix": "remove_recipient" }])
    );
    let draft = all["id"].as_str().unwrap();
    let (status, refused) = f
        .call("POST", &format!("/api/drafts/{draft}/send"), json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["leaks"][0]["recipient"], "carol@example.org");
    assert!(f.submitted.lock().unwrap().is_empty());
    f.ok(
        "POST",
        &format!("/api/drafts/{draft}/send"),
        json!({ "allow_leaks": true }),
    )
    .await;
    let audit = f.json("/api/audit").await;
    assert!(
        audit.as_array().unwrap().iter().any(|e| e["targets"]
            .as_array()
            .unwrap()
            .contains(&json!("queued_despite_leaks"))),
        "{audit}"
    );

    let sender = reply["id"].as_str().unwrap();
    f.ok("POST", &format!("/api/drafts/{sender}/send"), json!({}))
        .await;
    for _ in 0..2 {
        f.state
            .sending
            .clone()
            .unwrap()
            .outbox
            .send_due()
            .await
            .unwrap();
    }
    let submitted = f.submitted.lock().unwrap().clone();
    let (envelope, bytes) = submitted
        .iter()
        .find(|(e, _)| e.recipients == ["ra_bob@sl.example.org"])
        .unwrap_or_else(|| panic!("the reply went to the reverse alias only: {submitted:?}"));
    assert_eq!(envelope.mail_from, "ada@example.com");
    let parsed = holtmail_mime::parse(bytes).unwrap();
    assert_eq!(parsed.from[0].email, "ada@example.com");

    let set = f
        .ok(
            "PUT",
            &format!("/api/archives/{}/alias-strategies", f.archive),
            json!({ "scope": "alias", "key": "Shop@SL.example.org", "strategy": "direct" }),
        )
        .await;
    assert_eq!(
        set["overrides"],
        json!([{ "scope": "alias", "key": "shop@sl.example.org", "strategy": "direct" }])
    );
    assert!(
        set["defaults"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["strategy"] == "direct"),
        "the fake provider has no detector"
    );
    let listed = f
        .json(&format!("/api/archives/{}/aliases", f.archive))
        .await;
    let shop = listed["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["address"] == "shop@sl.example.org")
        .unwrap();
    assert_eq!(shop["reply_strategy"], "direct");
    assert_eq!(shop["reply_strategy_source"], "alias");
    let direct = f
        .ok("POST", &format!("/api/messages/{order}/reply"), json!({}))
        .await;
    assert_eq!(
        direct["route_error"]["code"], "no_alias_identity",
        "direct needs an identity that sends as the alias: {direct}"
    );
    assert_eq!(direct["route_error"]["alias"], "shop@sl.example.org");
    for bad in [
        json!({ "scope": "alias", "key": "not an address", "strategy": "direct" }),
        json!({ "scope": "domain", "key": "a@b.example", "strategy": "direct" }),
        json!({ "scope": "planet", "key": "x", "strategy": "direct" }),
        json!({ "scope": "service", "key": "simplelogin", "strategy": "teleport" }),
    ] {
        let (status, _) = f
            .call(
                "PUT",
                &format!("/api/archives/{}/alias-strategies", f.archive),
                bad.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let cleared = f
        .ok(
            "PUT",
            &format!("/api/archives/{}/alias-strategies", f.archive),
            json!({ "scope": "alias", "key": "shop@sl.example.org", "strategy": null }),
        )
        .await;
    assert_eq!(cleared["overrides"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn makes_a_reverse_alias_when_the_original_has_none() {
    let f = fixture().await;
    f.sending_identity().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let service = f
        .ok(
            "POST",
            &format!("/api/archives/{}/alias-services", f.archive),
            json!({ "provider": "fake", "key": "good-key" }),
        )
        .await;
    let service_id: holtmail_core::AliasServiceId =
        service["id"].as_str().unwrap().parse().unwrap();
    let made = f
        .ok(
            "POST",
            &format!("/api/alias-services/{service_id}/aliases"),
            json!({ "prefix": "shop" }),
        )
        .await;
    assert_eq!(made["address"], "shop@alias.example");
    let remote = f
        .store
        .known_aliases_among(archive, &["shop@alias.example".into()])
        .await
        .unwrap()
        .remove(0)
        .remote_id;
    f.store
        .put_alias(
            archive,
            &known_alias(
                "shop@alias.example",
                "fake",
                Some(service_id),
                remote.as_deref(),
            ),
        )
        .await
        .unwrap();
    f.ok(
        "PUT",
        &format!("/api/archives/{}/alias-strategies", f.archive),
        json!({ "scope": "service", "key": "fake", "strategy": "reverse_alias" }),
    )
    .await;
    let order = f
        .alias_mail(
            b"From: Bob <bob@example.org>\r\nTo: shop@alias.example\r\nSubject: Parcel\r\nMessage-ID: <parcel-1@example.org>\r\nDate: Wed, 12 Mar 2025 08:00:00 +0000\r\n\r\nOn its way.\r\n",
            "Parcel",
            MessageAlias {
                provider: "fake".into(),
                alias: "shop@alias.example".into(),
                sender: None,
                reverse_alias: None,
                signed: false,
            },
        )
        .await;
    let reply = f
        .ok("POST", &format!("/api/messages/{order}/reply"), json!({}))
        .await;
    assert_eq!(reply["route"]["strategy"], "reverse_alias", "{reply}");
    assert_eq!(reply["route"]["strategy_source"], "service");
    let first = reply["route"]["to"].as_str().unwrap().to_owned();
    assert!(first.starts_with("reverse"), "{first}");
    assert_eq!(
        reply["route"]["reasons"][1]["code"],
        "computed_reverse_alias"
    );
    assert_eq!(reply["payload"]["to"][0]["address"], first.as_str());
    let draft = reply["id"].as_str().unwrap();
    let again = f.json(&format!("/api/drafts/{draft}")).await;
    assert_eq!(again["route"]["to"], first.as_str());
    assert_eq!(again["leaks"], json!([]));

    let mut payload = again["payload"].clone();
    payload["cc"] = json!([{ "address": "carol@example.org" }]);
    let saved = f
        .ok(
            "PUT",
            &format!("/api/drafts/{draft}"),
            json!({ "version": again["version"], "payload": payload }),
        )
        .await;
    assert_eq!(saved["leaks"][0]["fix"], "use_reverse_alias");
    let fixed = f
        .ok(
            "POST",
            &format!("/api/drafts/{draft}/reverse-alias"),
            json!({ "version": saved["version"], "recipient": "Carol <carol@example.org>" }),
        )
        .await;
    let second = fixed["payload"]["cc"][0]["address"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(second.starts_with("reverse") && second != first, "{second}");
    assert_eq!(fixed["leaks"], json!([]));
    let (status, _) = f
        .call(
            "POST",
            &format!("/api/drafts/{draft}/reverse-alias"),
            json!({ "version": fixed["version"], "recipient": "dave@example.org" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "not a recipient");

    let mut forged = fixed["payload"].clone();
    forged["cc"] = json!([{ "address": "carol@example.org" }]);
    forged["context"]["reverse_contacts"] =
        json!([{ "contact": "carol@example.org", "address": "carol@example.org" }]);
    let forged = f
        .ok(
            "PUT",
            &format!("/api/drafts/{draft}"),
            json!({ "version": fixed["version"], "payload": forged }),
        )
        .await;
    assert_eq!(
        forged["leaks"][0]["recipient"], "carol@example.org",
        "a saved pair never counts, so the draft shows what send will refuse: {forged}"
    );
    let (status, refused) = f
        .call("POST", &format!("/api/drafts/{draft}/send"), json!({}))
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a pair the service does not confirm is no reverse alias: {refused}"
    );
    let mut payload = forged["payload"].clone();
    payload["cc"] = json!([{ "address": second }]);
    payload["context"]["reverse_contacts"] =
        json!([{ "contact": "carol@example.org", "address": second }]);
    f.ok(
        "PUT",
        &format!("/api/drafts/{draft}"),
        json!({ "version": forged["version"], "payload": payload }),
    )
    .await;
    f.ok("POST", &format!("/api/drafts/{draft}/send"), json!({}))
        .await;
    f.state
        .sending
        .clone()
        .unwrap()
        .outbox
        .send_due()
        .await
        .unwrap();
    let submitted = f.submitted.lock().unwrap().clone();
    assert_eq!(submitted[0].0.recipients, [first, second.clone()]);
    assert_eq!(
        f.store
            .reverse_alias_contact(archive, &second)
            .await
            .unwrap()
            .as_deref(),
        Some("carol@example.org"),
        "People can show whom the made reverse alias stands for"
    );
}

impl Fixture {
    async fn fake_service_alias(&self, strategy: &str) {
        let archive: ArchiveId = self.archive.parse().unwrap();
        let service = self
            .ok(
                "POST",
                &format!("/api/archives/{}/alias-services", self.archive),
                json!({ "provider": "fake", "key": "good-key" }),
            )
            .await;
        let service_id: holtmail_core::AliasServiceId =
            service["id"].as_str().unwrap().parse().unwrap();
        self.ok(
            "POST",
            &format!("/api/alias-services/{service_id}/aliases"),
            json!({ "prefix": "shop" }),
        )
        .await;
        let remote = self
            .store
            .known_aliases_among(archive, &["shop@alias.example".into()])
            .await
            .unwrap()
            .remove(0)
            .remote_id;
        self.store
            .put_alias(
                archive,
                &known_alias(
                    "shop@alias.example",
                    "fake",
                    Some(service_id),
                    remote.as_deref(),
                ),
            )
            .await
            .unwrap();
        self.ok(
            "PUT",
            &format!("/api/archives/{}/alias-strategies", self.archive),
            json!({ "scope": "service", "key": "fake", "strategy": strategy }),
        )
        .await;
    }

    async fn send_with(&self, state: ApiState, draft: &str) -> StatusCode {
        let mut request = request("POST", &format!("/api/drafts/{draft}/send"), json!({}));
        let headers = request.headers_mut();
        headers.insert(header::HOST, "mail.example.com".parse().unwrap());
        headers.insert(header::COOKIE, self.cookie.parse().unwrap());
        router(state).oneshot(request).await.unwrap().status()
    }
}

fn shop_hit() -> MessageAlias {
    MessageAlias {
        provider: "fake".into(),
        alias: "shop@alias.example".into(),
        sender: None,
        reverse_alias: None,
        signed: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn alias_sends_check_the_locks_first_and_keep_what_the_user_removed() {
    let f = fixture().await;
    f.sending_identity().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    f.fake_service_alias("direct").await;
    let order = f
        .alias_mail(
            b"From: Bob <bob@example.org>\r\nTo: shop@alias.example\r\nSubject: Crate\r\nMessage-ID: <crate-1@example.org>\r\nDate: Wed, 12 Mar 2025 08:00:00 +0000\r\n\r\nPacked.\r\n",
            "Crate",
            shop_hit(),
        )
        .await;
    let reply = f
        .ok("POST", &format!("/api/messages/{order}/reply"), json!({}))
        .await;
    assert_eq!(reply["route_error"]["code"], "no_alias_identity", "{reply}");
    assert_eq!(f.directory.reverse_aliases_made().await, 0);
    f.ok(
        "PUT",
        &format!("/api/archives/{}/alias-strategies", f.archive),
        json!({ "scope": "service", "key": "fake", "strategy": "reverse_alias" }),
    )
    .await;
    let draft = reply["id"].as_str().unwrap();

    let mut payload = reply["payload"].clone();
    payload["to"] = json!([]);
    payload["cc"] = json!([{ "address": "carol@example.org" }]);
    payload["context"]["reverse_contacts"] =
        json!([{ "contact": "dave@example.org", "address": "dave@alias.example" }]);
    let saved = f
        .ok(
            "PUT",
            &format!("/api/drafts/{draft}"),
            json!({ "version": reply["version"], "payload": payload }),
        )
        .await;
    assert!(
        saved["payload"]["context"]
            .get("reverse_contacts")
            .is_none()
            || saved["payload"]["context"]["reverse_contacts"] == json!([]),
        "a save never sets reverse contacts: {saved}"
    );

    let off = ApiState {
        write: WriteSwitches {
            send: false,
            ..WriteSwitches::default()
        },
        ..f.state.clone()
    };
    assert_eq!(f.send_with(off, draft).await, StatusCode::FORBIDDEN);
    assert_eq!(
        f.directory.reverse_aliases_made().await,
        0,
        "a refused send never reaches the alias service"
    );

    let (status, refused) = f
        .call("POST", &format!("/api/drafts/{draft}/send"), json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(
        refused["leaks"],
        json!([{ "recipient": "carol@example.org", "field": "cc", "reason": "cc_not_reverse_alias", "fix": "use_reverse_alias" }])
    );
    assert_eq!(f.directory.reverse_aliases_made().await, 1);
    f.ok(
        "POST",
        &format!("/api/drafts/{draft}/send"),
        json!({ "allow_leaks": true }),
    )
    .await;
    f.state
        .sending
        .clone()
        .unwrap()
        .outbox
        .send_due()
        .await
        .unwrap();
    let submitted = f.submitted.lock().unwrap().clone();
    assert_eq!(
        submitted[0].0.recipients,
        ["carol@example.org"],
        "the sender removed from To stays removed"
    );

    let listed = f
        .alias_mail(
            b"From: Bob <bob@example.org>\r\nReply-To: List <list@example.org>\r\nTo: shop@alias.example\r\nSubject: Club\r\nMessage-ID: <club-1@example.org>\r\nDate: Wed, 12 Mar 2025 09:00:00 +0000\r\n\r\nJoin.\r\n",
            "Club",
            shop_hit(),
        )
        .await;
    let club = f
        .ok("POST", &format!("/api/messages/{listed}/reply"), json!({}))
        .await;
    assert_eq!(
        club["payload"]["to"][0]["address"], "list@example.org",
        "an unsigned Reply-To is not the sender: {club}"
    );
    assert_eq!(club["leaks"][0]["recipient"], "list@example.org");

    let follow_up = f
        .alias_mail(
            b"From: Bob <bob@example.org>\r\nTo: ada@example.com\r\nSubject: Arrival\r\nIn-Reply-To: <crate-1@example.org>\r\nReferences: <crate-1@example.org>\r\nMessage-ID: <crate-2@example.org>\r\nDate: Wed, 12 Mar 2025 10:00:00 +0000\r\n\r\nArrived?\r\n",
            "Arrival",
            shop_hit(),
        )
        .await;
    f.store
        .set_message_alias(follow_up.parse().unwrap(), None)
        .await
        .unwrap();
    let plain = f
        .ok(
            "POST",
            &format!("/api/messages/{follow_up}/reply"),
            json!({}),
        )
        .await;
    assert_eq!(
        plain["route"]["alias"], "shop@alias.example",
        "the thread's alias: {plain}"
    );
    assert_eq!(plain["leaks"][0]["fix"], "use_alias_identity", "{plain}");
    let alias_transport = f
        .ok(
            "POST",
            &format!("/api/archives/{}/transports", f.archive),
            json!({
                "name": "Alias relay",
                "kind": "smtp",
                "host": "smtp.example.com",
                "username": "shop",
                "password": "alias secret 42",
                "allowed_from": ["*@alias.example"],
            }),
        )
        .await;
    let shop = f
        .ok(
            "POST",
            &format!("/api/archives/{}/identities", f.archive),
            json!({
                "address": "shop@alias.example",
                "transport": alias_transport["id"],
                "active": true,
            }),
        )
        .await;
    let mut payload = plain["payload"].clone();
    payload["identity"] = shop["id"].clone();
    let picked = f
        .ok(
            "PUT",
            &format!("/api/drafts/{}", plain["id"].as_str().unwrap()),
            json!({ "version": plain["version"], "payload": payload }),
        )
        .await;
    assert_eq!(picked["route"]["from"], "shop@alias.example");
    assert_eq!(picked["leaks"], json!([]), "{picked}");
    let _ = archive;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_raw_draft_opens_like_edit_as_new() {
    let f = fixture().await;
    f.sending_identity().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let owner = f.store.list_users().await.unwrap()[0].id;
    let raw = "From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\n\
               Subject: Saved in a mail client\r\nIn-Reply-To: <earlier@example.org>\r\n\
               MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
               --b\r\nContent-Type: text/plain\r\n\r\nSee the notes.\r\n\
               --b\r\nContent-Type: text/plain; name=\"notes.txt\"\r\n\
               Content-Disposition: attachment; filename=\"notes.txt\"\r\n\r\nnotes\r\n--b--\r\n";
    let blob = f
        .blobs
        .put(bytes::Bytes::from_static(raw.as_bytes()))
        .await
        .unwrap();
    let draft = f
        .store
        .create_raw_draft(archive, owner, &blob, holtmail_core::Timestamp::now())
        .await
        .unwrap();
    let shown = f.json(&format!("/api/drafts/{}", draft.id)).await;
    let payload = &shown["payload"];
    assert_eq!(payload["subject"], "Saved in a mail client");
    assert_eq!(payload["to"][0]["address"], "bob@example.org");
    assert_eq!(payload["text"].as_str().unwrap().trim(), "See the notes.");
    assert_eq!(payload["in_reply_to"], "earlier@example.org");
    assert_eq!(payload["attachments"][0]["name"], "notes.txt");
    assert!(payload["attachments"][0]["blob"].is_string());
    assert_eq!(payload["context"]["kind"], "new");
    let versions = f.json(&format!("/api/drafts/{}/versions", draft.id)).await;
    assert_eq!(versions[0]["payload"]["subject"], "Saved in a mail client");
    let saved = f
        .ok(
            "PUT",
            &format!("/api/drafts/{}", draft.id),
            json!({ "version": 1, "payload": payload }),
        )
        .await;
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["payload"]["subject"], "Saved in a mail client");
}
