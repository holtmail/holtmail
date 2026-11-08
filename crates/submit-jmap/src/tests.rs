use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use holtmail_submit::testing::{Behavior, Deliveries, Delivery, Fake as SuiteFake, Receiver};

use super::*;

const PASSWORD: &str = "correct horse";
const TOKEN: &str = "fmu1-example-token";
const ADA: &str = "ada@example.com";
const BOB: &str = "bob@example.org";
const CAROL: &str = "carol@example.org";

struct Email {
    blob: String,
    mailboxes: BTreeSet<String>,
    keywords: BTreeSet<String>,
}

struct Fake {
    port: u16,
    behavior: Behavior,
    deliveries: Deliveries,
    blobs: BTreeMap<String, Vec<u8>>,
    emails: BTreeMap<String, Email>,
    next: u64,
    no_drafts: bool,
    no_submission: bool,
    server_fail: bool,
    status_refuse: Option<String>,
    identities: Vec<(&'static str, &'static str)>,
    calls: Vec<String>,
}

impl Fake {
    fn new(behavior: Behavior) -> Self {
        Self {
            port: 0,
            behavior,
            deliveries: Deliveries::default(),
            blobs: BTreeMap::new(),
            emails: BTreeMap::new(),
            next: 0,
            no_drafts: false,
            no_submission: false,
            server_fail: false,
            status_refuse: None,
            identities: vec![("i1", ADA), ("i2", "*@example.net")],
            calls: Vec::new(),
        }
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn limit(&self) -> u64 {
        match self.behavior {
            Behavior::SizeLimit(limit) => limit,
            _ => 50_000_000,
        }
    }
}

type Shared = Arc<Mutex<Fake>>;

fn authorized(headers: &HeaderMap) -> bool {
    let basic = format!("Basic {}", STANDARD.encode(format!("{ADA}:{PASSWORD}")));
    let bearer = format!("Bearer {TOKEN}");
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == basic || v == bearer)
}

async fn session(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let fake = fake.lock().unwrap();
    let base = format!("http://127.0.0.1:{}", fake.port);
    let mut capabilities = json!({
        CORE: { "maxSizeUpload": fake.limit(), "maxCallsInRequest": 16 },
        MAIL: {},
    });
    let mut account_capabilities = json!({ MAIL: {} });
    if !fake.no_submission {
        capabilities[SUBMISSION] = json!({});
        account_capabilities[SUBMISSION] =
            json!({ "maxDelayedSend": 0, "submissionExtensions": { "8BITMIME": [] } });
    }
    axum::Json(json!({
        "capabilities": capabilities,
        "accounts": { "u1": { "name": ADA, "accountCapabilities": account_capabilities } },
        "primaryAccounts": { MAIL: "u1", SUBMISSION: "u1" },
        "username": ADA,
        "apiUrl": format!("{base}/jmap/api/"),
        "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}"),
        "uploadUrl": "/jmap/upload/{accountId}/",
        "state": "1",
    }))
    .into_response()
}

async fn upload(
    State(fake): State<Shared>,
    Path(account): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    assert_eq!(account, "u1");
    let mut fake = fake.lock().unwrap();
    if body.len() as u64 > fake.limit() {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let id = fake.id("b");
    fake.blobs.insert(id.clone(), body.to_vec());
    axum::Json(
        json!({ "accountId": "u1", "blobId": id, "type": "message/rfc822", "size": body.len() }),
    )
    .into_response()
}

fn reference(created: &BTreeMap<String, String>, raw: &str) -> Option<String> {
    match raw.strip_prefix('#') {
        Some(creation) => created.get(creation).cloned(),
        None => Some(raw.to_owned()),
    }
}

fn apply(email: &mut Email, patch: &Map<String, Value>) {
    for (path, value) in patch {
        let (set, key) = if let Some(key) = path.strip_prefix("mailboxIds/") {
            (&mut email.mailboxes, key)
        } else if let Some(key) = path.strip_prefix("keywords/") {
            (&mut email.keywords, key)
        } else {
            panic!("unexpected patch {path}");
        };
        let key = key.replace("~1", "/").replace("~0", "~");
        if value == &json!(true) {
            set.insert(key);
        } else {
            assert!(value.is_null());
            set.remove(&key);
        }
    }
}

fn submit(fake: &mut Fake, args: &Value, created: &mut BTreeMap<String, String>) -> (Value, bool) {
    let mut done = Map::new();
    let mut failed = Map::new();
    let mut drop = false;
    for (cid, spec) in args["create"].as_object().unwrap() {
        let Some(email_id) = reference(created, spec["emailId"].as_str().unwrap())
            .filter(|id| fake.emails.contains_key(id))
        else {
            failed.insert(
                cid.clone(),
                json!({ "type": "invalidProperties", "properties": ["emailId"] }),
            );
            continue;
        };
        let identity = spec["identityId"].as_str().unwrap();
        assert!(fake.identities.iter().any(|(id, _)| *id == identity));
        let mail_from = spec["envelope"]["mailFrom"]["email"]
            .as_str()
            .unwrap()
            .to_owned();
        let recipients: Vec<String> = spec["envelope"]["rcptTo"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["email"].as_str().unwrap().to_owned())
            .collect();
        let refusal = match &fake.behavior {
            Behavior::TemporaryFailure => Some(json!({ "type": "rateLimit" })),
            Behavior::PermanentFailure => {
                Some(json!({ "type": "forbiddenToSend", "description": "sending is turned off" }))
            }
            Behavior::RefuseRecipient(r) if recipients.contains(r) => {
                Some(json!({ "type": "invalidRecipients", "invalidRecipients": [r] }))
            }
            _ => None,
        };
        if let Some(refusal) = refusal {
            failed.insert(cid.clone(), refusal);
            continue;
        }
        let bytes = fake.blobs[&fake.emails[&email_id].blob].clone();
        let accepted: Vec<String> = recipients
            .iter()
            .filter(|r| fake.status_refuse.as_ref() != Some(*r))
            .cloned()
            .collect();
        fake.deliveries.push(Delivery {
            mail_from,
            recipients: accepted,
            bytes,
        });
        if fake.behavior == Behavior::DropAfterData {
            drop = true;
        }
        let status: Map<String, Value> = recipients
            .iter()
            .map(|r| {
                let refused = fake.status_refuse.as_ref() == Some(r);
                (
                    r.clone(),
                    json!({
                        "smtpReply": if refused { "550 5.1.1 No such user" } else { "250 2.1.5 OK" },
                        "delivered": if refused { "no" } else { "queued" },
                        "displayed": "unknown",
                    }),
                )
            })
            .collect();
        let id = fake.id("s");
        created.insert(cid.clone(), id.clone());
        done.insert(
            cid.clone(),
            json!({ "id": id, "undoStatus": "final", "deliveryStatus": status }),
        );
        if let Some(patch) = args["onSuccessUpdateEmail"]
            .get(format!("#{cid}"))
            .and_then(Value::as_object)
        {
            apply(fake.emails.get_mut(&email_id).unwrap(), patch);
        }
    }
    (
        json!({ "accountId": "u1", "newState": "2", "created": done, "notCreated": failed }),
        drop,
    )
}

async fn api(State(fake): State<Shared>, headers: HeaderMap, body: String) -> Response {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let request: Value = serde_json::from_str(&body).unwrap();
    let mut fake = fake.lock().unwrap();
    let mut created = BTreeMap::new();
    let mut results = Vec::new();
    let mut drop = false;
    for call in request["methodCalls"].as_array().unwrap() {
        let name = call[0].as_str().unwrap().to_owned();
        let args = &call[1];
        let id = call[2].clone();
        assert_eq!(args["accountId"], "u1");
        fake.calls.push(name.clone());
        let answer = match name.as_str() {
            "Mailbox/get" => {
                let mut list = vec![
                    json!({ "id": "inbox", "role": "inbox" }),
                    json!({ "id": "sent", "role": "sent" }),
                ];
                if !fake.no_drafts {
                    list.push(json!({ "id": "drafts", "role": "drafts" }));
                }
                json!(["Mailbox/get", { "accountId": "u1", "state": "1", "list": list, "notFound": [] }, id])
            }
            "Identity/get" => {
                let list: Vec<Value> = fake
                    .identities
                    .iter()
                    .map(|(id, email)| json!({ "id": id, "email": email, "name": "Ada", "mayDelete": false }))
                    .collect();
                json!(["Identity/get", { "accountId": "u1", "state": "1", "list": list, "notFound": [] }, id])
            }
            "Email/import" => {
                let mut done = Map::new();
                let mut failed = Map::new();
                for (cid, spec) in args["emails"].as_object().unwrap() {
                    let blob = spec["blobId"].as_str().unwrap().to_owned();
                    let mailboxes: BTreeSet<String> = spec["mailboxIds"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .cloned()
                        .collect();
                    let known = ["inbox", "sent", "drafts"];
                    if !fake.blobs.contains_key(&blob)
                        || mailboxes.iter().any(|m| !known.contains(&m.as_str()))
                    {
                        failed.insert(cid.clone(), json!({ "type": "invalidProperties" }));
                        continue;
                    }
                    let keywords = spec["keywords"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .cloned()
                        .collect();
                    let email = fake.id("e");
                    fake.emails.insert(
                        email.clone(),
                        Email {
                            blob: blob.clone(),
                            mailboxes,
                            keywords,
                        },
                    );
                    created.insert(cid.clone(), email.clone());
                    done.insert(
                        cid.clone(),
                        json!({ "id": email, "blobId": blob, "threadId": "t1", "size": 1 }),
                    );
                }
                json!(["Email/import", { "accountId": "u1", "newState": "2", "created": done, "notCreated": failed }, id])
            }
            "EmailSubmission/set" if fake.server_fail => {
                json!(["error", { "type": "serverFail" }, id])
            }
            "EmailSubmission/set" => {
                let (answer, dropped) = submit(&mut fake, args, &mut created);
                drop |= dropped;
                json!(["EmailSubmission/set", answer, id])
            }
            "Email/set" => {
                for destroyed in args["destroy"].as_array().unwrap() {
                    fake.emails.remove(destroyed.as_str().unwrap());
                }
                json!(["Email/set", { "accountId": "u1", "destroyed": args["destroy"] }, id])
            }
            other => json!(["error", { "type": "unknownMethod", "name": other }, id]),
        };
        results.push(answer);
    }
    if drop {
        let broken = futures::stream::once(async {
            Err::<Bytes, std::io::Error>(std::io::Error::other("dropped"))
        });
        return Body::from_stream(broken).into_response();
    }
    axum::Json(json!({ "methodResponses": results, "sessionState": "1" })).into_response()
}

async fn serve(fake: Fake) -> (Shared, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = Arc::new(Mutex::new(Fake { port, ..fake }));
    let app = Router::new()
        .route(
            "/.well-known/jmap",
            get(|| async { Redirect::temporary("/jmap/session") }),
        )
        .route("/jmap/session", get(session))
        .route("/jmap/api/", post(api))
        .route("/jmap/upload/{account}/", post(upload))
        .with_state(shared.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (shared, port)
}

fn config(port: u16) -> JmapSubmitConfig {
    JmapSubmitConfig::new(
        format!("http://127.0.0.1:{port}"),
        ADA,
        JmapSecret::Password(PASSWORD.into()),
    )
}

async fn start(fake: Fake) -> (Shared, JmapSubmitter) {
    let (shared, port) = serve(fake).await;
    (shared, JmapSubmitter::new(config(port)).unwrap())
}

struct JmapReceiver;

#[async_trait]
impl Receiver for JmapReceiver {
    async fn start(&self, behavior: Behavior) -> SuiteFake {
        let fake = Fake::new(behavior);
        let deliveries = fake.deliveries.clone();
        let (_, submitter) = start(fake).await;
        SuiteFake {
            submitter: Box::new(submitter),
            deliveries,
        }
    }
}

fn envelope(recipients: &[&str]) -> Envelope {
    Envelope {
        mail_from: ADA.into(),
        recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
    }
}

const MESSAGE: &[u8] =
    b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\nSubject: Hello\r\n\r\nHi Bob\r\n";

#[tokio::test]
async fn conformance() {
    holtmail_submit::testing::conformance(&JmapReceiver).await;
}

#[tokio::test]
async fn keeps_the_sent_copy_in_sent_without_draft() {
    let (fake, submitter) = start(Fake::new(Behavior::AcceptAll)).await;
    submitter.submit(&envelope(&[BOB]), MESSAGE).await.unwrap();
    let fake = fake.lock().unwrap();
    assert_eq!(fake.emails.len(), 1);
    let email = fake.emails.values().next().unwrap();
    assert_eq!(email.mailboxes, BTreeSet::from(["sent".to_owned()]));
    assert_eq!(email.keywords, BTreeSet::from(["$seen".to_owned()]));
    assert_eq!(fake.blobs[&email.blob], MESSAGE);
    assert_eq!(
        fake.calls,
        [
            "Mailbox/get",
            "Identity/get",
            "Email/import",
            "EmailSubmission/set"
        ]
    );
}

#[tokio::test]
async fn imports_into_sent_without_a_drafts_mailbox() {
    let mut fake = Fake::new(Behavior::AcceptAll);
    fake.no_drafts = true;
    let (fake, submitter) = start(fake).await;
    submitter.submit(&envelope(&[BOB]), MESSAGE).await.unwrap();
    let fake = fake.lock().unwrap();
    let email = fake.emails.values().next().unwrap();
    assert_eq!(email.mailboxes, BTreeSet::from(["sent".to_owned()]));
    assert!(!email.keywords.contains("$draft"));
}

#[tokio::test]
async fn removes_the_imported_copy_after_a_definite_failure() {
    let (fake, submitter) = start(Fake::new(Behavior::PermanentFailure)).await;
    let error = submitter
        .submit(&envelope(&[BOB]), MESSAGE)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SubmitError::Refused {
            permanent: true,
            code: 550,
            ..
        }
    ));
    let fake = fake.lock().unwrap();
    assert!(fake.emails.is_empty(), "the failed copy stayed");
    assert_eq!(fake.calls.last().unwrap(), "Email/set");
}

#[tokio::test]
async fn keeps_the_copy_when_the_outcome_is_unknown() {
    let mut fake = Fake::new(Behavior::AcceptAll);
    fake.server_fail = true;
    let (fake, submitter) = start(fake).await;
    let error = submitter
        .submit(&envelope(&[BOB]), MESSAGE)
        .await
        .unwrap_err();
    assert!(matches!(error, SubmitError::Uncertain(_)), "{error:?}");
    assert_eq!(fake.lock().unwrap().emails.len(), 1);
}

#[tokio::test]
async fn reads_refusals_from_the_delivery_status() {
    let mut fake = Fake::new(Behavior::AcceptAll);
    fake.status_refuse = Some(CAROL.into());
    let (_, submitter) = start(fake).await;
    let report = submitter
        .submit(&envelope(&[BOB, CAROL]), MESSAGE)
        .await
        .unwrap();
    assert_eq!(report.recipients[0].outcome, RecipientOutcome::Accepted);
    let refused: Vec<_> = report.refused().collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].0, CAROL);
    assert_eq!(refused[0].1.code, 550);
    assert!(refused[0].1.permanent);

    let all = submitter.submit(&envelope(&[CAROL]), MESSAGE).await;
    assert!(
        matches!(all, Err(SubmitError::RecipientsRefused(_))),
        "{all:?}"
    );
}

#[tokio::test]
async fn signs_in_with_a_token_and_refuses_wrong_credentials() {
    let (_, port) = serve(Fake::new(Behavior::AcceptAll)).await;
    let mut token = config(port);
    token.secret = JmapSecret::Token(TOKEN.into());
    token.username = String::new();
    JmapSubmitter::new(token).unwrap().check().await.unwrap();

    let mut wrong = config(port);
    wrong.secret = JmapSecret::Password("wrong".into());
    let submitter = JmapSubmitter::new(wrong).unwrap();
    assert!(matches!(submitter.check().await, Err(SubmitError::Auth(_))));
    assert!(matches!(
        submitter.submit(&envelope(&[BOB]), MESSAGE).await,
        Err(SubmitError::Auth(_))
    ));
}

#[tokio::test]
async fn check_needs_submission_and_a_matching_identity() {
    let mut fake = Fake::new(Behavior::AcceptAll);
    fake.no_submission = true;
    let (_, submitter) = start(fake).await;
    assert!(matches!(
        submitter.check().await,
        Err(SubmitError::Invalid(_))
    ));

    let (_, port) = serve(Fake::new(Behavior::AcceptAll)).await;
    for (address, ok) in [
        ("ada@example.com", true),
        ("ADA@example.com", true),
        ("anyone@example.net", true),
        ("eve@example.org", false),
    ] {
        let mut config = config(port);
        config.address = Some(address.into());
        let result = JmapSubmitter::new(config).unwrap().check().await;
        assert_eq!(result.is_ok(), ok, "{address}: {result:?}");
    }
}

#[tokio::test]
async fn refuses_a_return_path_without_an_identity_before_uploading() {
    let (fake, submitter) = start(Fake::new(Behavior::AcceptAll)).await;
    let other = Envelope {
        mail_from: "eve@example.org".into(),
        recipients: vec![BOB.into()],
    };
    let error = submitter.submit(&other, MESSAGE).await.unwrap_err();
    assert!(matches!(error, SubmitError::Invalid(_)));
    assert!(fake.lock().unwrap().blobs.is_empty());
}

#[tokio::test]
async fn unreachable_servers_are_unavailable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let submitter = JmapSubmitter::new(config(port)).unwrap();
    let error = submitter
        .submit(&envelope(&[BOB]), MESSAGE)
        .await
        .unwrap_err();
    assert!(matches!(error, SubmitError::Unavailable(_)), "{error:?}");
    assert!(error.is_retryable());
}

#[test]
fn refuses_unusable_configurations() {
    assert!(session_url("http://mail.example.com").is_err());
    assert!(session_url("https://ada:pw@mail.example.com").is_err());
    assert_eq!(
        session_url("mail.example.com").unwrap(),
        "https://mail.example.com/.well-known/jmap"
    );
    let no_user = JmapSubmitConfig::new(
        "https://mail.example.com",
        " ",
        JmapSecret::Password(PASSWORD.into()),
    );
    assert!(matches!(
        JmapSubmitter::new(no_user),
        Err(SubmitError::Invalid(_))
    ));
    assert_eq!(
        format!("{:?}", JmapSecret::Token(TOKEN.into())),
        "Token(..)"
    );
}

#[test]
fn maps_set_errors() {
    let limit = Some(10);
    assert!(matches!(
        set_error(&json!({ "type": "forbiddenFrom" }), limit),
        SubmitError::Refused {
            permanent: true,
            code: 550,
            ..
        }
    ));
    assert_eq!(
        set_error(&json!({ "type": "tooLarge" }), limit),
        SubmitError::TooLarge { limit }
    );
    assert_eq!(
        set_error(&json!({ "type": "tooLarge", "maxSize": 5 }), limit),
        SubmitError::TooLarge { limit: Some(5) }
    );
    assert!(matches!(
        set_error(&json!({ "type": "rateLimit" }), limit),
        SubmitError::Temporary(_)
    ));
    assert!(matches!(
        method_error("serverFail"),
        SubmitError::Uncertain(_)
    ));
    assert!(method_error("serverUnavailable").is_retryable());
}
