use holtmail_core::{ArchiveId, MessageId};

use super::*;
use crate::{RuleLimits, Rules};

const FROM_CAROL: &[u8] = b"From: Carol <carol@example.org>\r\nTo: ada@example.com\r\nSubject: Lunch\r\nMessage-ID: <lunch-1@example.org>\r\nDate: Wed, 12 Mar 2025 08:00:00 +0000\r\n\r\nLunch on Friday?\r\n";
const FROM_CAROL_AGAIN: &[u8] = b"From: Carol <carol@example.org>\r\nTo: ada@example.com\r\nSubject: Lunch again\r\nMessage-ID: <lunch-2@example.org>\r\nDate: Wed, 12 Mar 2025 09:00:00 +0000\r\n\r\nOr Saturday?\r\n";
const FROM_LIST: &[u8] = b"From: Carol <carol@example.org>\r\nTo: friends@lists.example.org\r\nList-Id: <friends.lists.example.org>\r\nSubject: Picnic\r\nMessage-ID: <picnic-1@example.org>\r\nDate: Wed, 12 Mar 2025 10:00:00 +0000\r\n\r\nPicnic on Sunday.\r\n";
const FROM_DAVE: &[u8] = b"From: Dave <dave@example.org>\r\nTo: ada@example.com\r\nSubject: Invoice\r\nMessage-ID: <invoice-1@example.org>\r\nDate: Wed, 12 Mar 2025 11:00:00 +0000\r\n\r\nYour invoice.\r\n";
const FROM_ERIN: &[u8] = b"From: Erin <erin@example.org>\r\nTo: ada@example.com\r\nSubject: Hello\r\nMessage-ID: <hello-1@example.org>\r\nDate: Wed, 12 Mar 2025 12:00:00 +0000\r\n\r\nHello.\r\n";

fn away(identity: &str) -> Value {
    json!({
        "trigger": { "type": "message.archived", "next": "carol" },
        "nodes": [
            { "id": "carol", "type": "query", "params": { "query": "from:carol@example.org" },
              "next": { "yes": "reply" } },
            { "id": "reply", "type": "reply",
              "params": { "identity": identity, "body": "Away until Monday, {{ sender.name }}." } }
        ]
    })
}

impl Fixture {
    async fn arrive(&self, rules: &Rules, raw: &'static [u8], live: bool) -> MessageId {
        let archive: ArchiveId = self.archive.parse().unwrap();
        let done = self
            .ingest
            .ingest(
                archive,
                Bytes::from_static(raw),
                &Placement {
                    live,
                    ..Placement::default()
                },
            )
            .await
            .unwrap();
        self.ingest.commit().await.unwrap();
        let parsed = holtmail_mime::parse(raw).unwrap();
        rules.observe(archive, done.message_id, &parsed, live).await;
        rules.run_due().await;
        done.message_id
    }

    async fn send_queued(&self) {
        for _ in 0..3 {
            self.state
                .sending
                .clone()
                .unwrap()
                .outbox
                .send_due()
                .await
                .unwrap();
        }
    }

    async fn new_flow(&self, name: &str, flow: Value) -> String {
        let created = self
            .ok(
                "POST",
                &format!("/api/archives/{}/flows", self.archive),
                json!({ "name": name, "flow": flow }),
            )
            .await;
        created["id"].as_str().unwrap().to_owned()
    }

    async fn turn_on(&self, flow: &str) {
        self.ok(
            "PATCH",
            &format!("/api/flows/{flow}"),
            json!({ "mode": "on" }),
        )
        .await;
    }

    async fn runs(&self, flow: &str) -> Vec<Value> {
        self.json(&format!("/api/flows/{flow}/runs"))
            .await
            .as_array()
            .unwrap()
            .clone()
    }
}

fn replies_to(submitted: &[(holtmail_submit::Envelope, Vec<u8>)], to: &str) -> usize {
    submitted
        .iter()
        .filter(|(e, _)| e.recipients.iter().any(|r| r == to))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn flows_are_versioned_validated_and_dry_run() {
    let f = fixture().await;
    let catalogue = f.json("/api/rules/catalogue").await;
    let ids: Vec<&str> = catalogue
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"reply") && ids.contains(&"message.archived"));

    let (status, refused) = f
        .call(
            "POST",
            &format!("/api/archives/{}/flows", f.archive),
            json!({ "name": "Broken", "flow": { "trigger": { "type": "message.archived", "next": "x" },
                    "nodes": [{ "id": "x", "type": "nope" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["issues"][0]["node"], "x");

    let (_, identity) = f.sending_identity().await;
    let created = f
        .ok(
            "POST",
            &format!("/api/archives/{}/flows", f.archive),
            json!({ "name": "Away", "flow": away(&identity) }),
        )
        .await;
    assert_eq!(created["mode"], "test", "a new flow starts in test mode");
    assert_eq!(created["version"], 1);
    let flow = created["id"].as_str().unwrap();

    let (status, _) = f
        .call(
            "PUT",
            &format!("/api/flows/{flow}"),
            json!({ "version": 1, "flow": away("auto") }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, stale) = f
        .call(
            "PUT",
            &format!("/api/flows/{flow}"),
            json!({ "version": 1, "flow": away("auto") }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    let versions = f.json(&format!("/api/flows/{flow}/versions")).await;
    assert_eq!(versions.as_array().unwrap().len(), 2);
    assert_eq!(versions[0]["version"], 2);

    let bob = f.message_with_subject("Re: Weekly news").await;
    let tested = f
        .ok(
            "POST",
            &format!("/api/flows/{flow}/test"),
            json!({ "message": bob }),
        )
        .await;
    assert_eq!(tested["plan"]["ran"], true);
    assert_eq!(tested["plan"]["path"][0]["port"], "no", "{tested}");
    assert!(tested["plan"]["actions"].as_array().unwrap().is_empty());

    let rules = Rules::new(f.state.clone(), RuleLimits::default());
    let carol = f.arrive(&rules, FROM_CAROL, false).await;
    let tested = f
        .ok(
            "POST",
            &format!("/api/archives/{}/flows/test", f.archive),
            json!({ "flow": away("auto"), "message": carol.to_string() }),
        )
        .await;
    let action = &tested["plan"]["actions"][0];
    assert_eq!(action["action"]["type"], "reply", "{tested}");
    assert_eq!(action["action"]["body"], "Away until Monday, Carol.");
    assert_eq!(action["action"]["auto_submitted"], "auto-replied");
    assert!(
        f.runs(flow).await.is_empty(),
        "a dry run is not logged, and a backfill raises no event"
    );

    let changed = f
        .ok(
            "PATCH",
            &format!("/api/flows/{flow}"),
            json!({ "mode": "on", "name": "Away message" }),
        )
        .await;
    assert_eq!(changed["mode"], "on");
    assert_eq!(changed["name"], "Away message");
    assert!(f.audit_kinds().await.iter().any(|k| k == "rule_changed"));
    f.ok("DELETE", &format!("/api/flows/{flow}"), Value::Null)
        .await;
    let (status, _) = f
        .call("GET", &format!("/api/flows/{flow}"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_replies_once_per_sender_never_to_lists_and_only_when_on() {
    let f = fixture().await;
    f.sending_identity().await;
    let flow = f.new_flow("Away", away("auto")).await;
    let rules = Rules::new(f.state.clone(), RuleLimits::default());

    f.arrive(&rules, FROM_CAROL, true).await;
    f.send_queued().await;
    assert!(
        f.submitted.lock().unwrap().is_empty(),
        "a flow in test mode sends nothing"
    );
    let runs = f.runs(&flow).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["test"], true);
    assert_eq!(runs[0]["results"][0]["result"], "test");

    f.turn_on(&flow).await;
    f.arrive(&rules, FROM_CAROL_AGAIN, true).await;
    f.send_queued().await;
    let submitted = f.submitted.lock().unwrap().clone();
    assert_eq!(
        replies_to(&submitted, "carol@example.org"),
        1,
        "{submitted:?}"
    );
    let (_, bytes) = submitted
        .iter()
        .find(|(e, _)| e.recipients == ["carol@example.org"])
        .unwrap();
    let parsed = holtmail_mime::parse(bytes).unwrap();
    assert_eq!(parsed.from[0].email, "ada@example.com");
    assert_eq!(parsed.subject.as_deref(), Some("Re: Lunch again"));
    assert!(
        parsed
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("Auto-Submitted")
                && h.value.trim() == "auto-replied"),
        "RFC 3834: the reply says it is automatic"
    );
    assert_eq!(parsed.in_reply_to, vec!["lunch-2@example.org".to_owned()]);

    let rules_again = Rules::new(f.state.clone(), RuleLimits::default());
    f.arrive(&rules_again, FROM_LIST, true).await;
    f.send_queued().await;
    assert_eq!(
        replies_to(&f.submitted.lock().unwrap(), "carol@example.org"),
        1,
        "no second reply to the same sender within days, and none to a list"
    );
    let runs = f.runs(&flow).await;
    let skipped: Vec<&str> = runs
        .iter()
        .filter_map(|r| r["plan"]["path"].as_array())
        .flatten()
        .filter_map(|s| s["reason"]["code"].as_str())
        .collect();
    assert!(skipped.contains(&"mailing_list"), "{runs:?}");

    f.arrive(&rules, FROM_CAROL, true).await;
    assert_eq!(
        f.runs(&flow).await.len(),
        3,
        "the same event never runs twice for one flow version"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn held_actions_drafts_and_the_circuit_breaker() {
    let f = fixture().await;
    let (_, identity) = f.sending_identity().await;
    let confirm = f
        .new_flow(
            "Confirm",
            json!({
                "trigger": { "type": "message.archived", "next": "dave" },
                "nodes": [
                    { "id": "dave", "type": "query", "params": { "query": "from:dave@example.org" },
                      "next": { "yes": "fwd", "no": "flag" } },
                    { "id": "fwd", "type": "forward", "confirm": true,
                      "params": { "to": ["books@example.org"], "identity": identity } },
                    { "id": "flag", "type": "set_flags", "confirm": true,
                      "params": { "set": ["flagged"] } }
                ]
            }),
        )
        .await;
    f.turn_on(&confirm).await;
    let rules = Rules::new(
        f.state.clone(),
        RuleLimits {
            breaker_runs_per_hour: 1,
            ..RuleLimits::default()
        },
    );
    f.arrive(&rules, FROM_DAVE, true).await;
    f.send_queued().await;
    assert!(f.submitted.lock().unwrap().is_empty());
    let runs = f.runs(&confirm).await;
    assert_eq!(runs[0]["results"][0]["result"], "draft", "{runs:?}");
    let drafts = f.json(&format!("/api/archives/{}/drafts", f.archive)).await;
    assert_eq!(
        drafts.as_array().unwrap().len(),
        1,
        "the forward waits as a draft"
    );

    f.arrive(&rules, FROM_ERIN, true).await;
    let runs = f.runs(&confirm).await;
    let held = &runs[0];
    assert_eq!(held["results"][0]["result"], "pending", "{held}");
    let run = held["id"].as_str().unwrap();
    let decided = f
        .ok(
            "POST",
            &format!("/api/flow-runs/{run}/actions/0/dismiss"),
            Value::Null,
        )
        .await;
    assert_eq!(decided["results"][0]["result"], "dismissed");
    let (status, _) = f
        .call(
            "POST",
            &format!("/api/flow-runs/{run}/actions/0/confirm"),
            Value::Null,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a dismissed action stays dismissed"
    );

    let held = f.json(&format!("/api/flows/{confirm}")).await;
    assert_eq!(
        held["paused"],
        Value::Null,
        "held actions carry nothing out, so they never count for the breaker"
    );

    let busy = f
        .new_flow("Busy", labeller("from:example.org", "Seen"))
        .await;
    f.turn_on(&busy).await;
    f.arrive(&rules, FROM_CAROL, true).await;
    f.arrive(&rules, FROM_CAROL_AGAIN, true).await;
    let paused = f.json(&format!("/api/flows/{busy}")).await;
    assert_eq!(
        paused["paused"], "circuit_breaker",
        "two acting runs in an hour exceed a limit of one"
    );
    f.arrive(&rules, FROM_LIST, true).await;
    assert_eq!(f.runs(&busy).await.len(), 2, "a paused flow does not run");
    let resumed = f
        .ok(
            "PATCH",
            &format!("/api/flows/{busy}"),
            json!({ "mode": "on" }),
        )
        .await;
    assert_eq!(resumed["paused"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_rules_block_ask_and_add_copies() {
    let f = fixture().await;
    f.sending_identity().await;
    let flow = f
        .new_flow(
            "Guard",
            json!({
                "trigger": { "type": "message.sending", "next": "secret" },
                "nodes": [
                    { "id": "secret", "type": "query", "params": { "query": "subject:secret" },
                      "next": { "yes": "block", "no": "ext" } },
                    { "id": "block", "type": "block", "params": { "reason": "No secrets by mail." } },
                    { "id": "ext", "type": "query", "params": { "query": "to:example.org" },
                      "next": { "yes": "ask" } },
                    { "id": "ask", "type": "require_confirmation",
                      "params": { "reason": "This leaves the company." }, "next": { "next": "bcc" } },
                    { "id": "bcc", "type": "add_bcc", "params": { "address": "archive@example.com" } }
                ]
            }),
        )
        .await;
    f.turn_on(&flow).await;
    let drafts = format!("/api/archives/{}/drafts", f.archive);
    let secret = f
        .ok(
            "POST",
            &drafts,
            json!({ "payload": { "to": [{ "address": "bob@example.org" }], "subject": "The secret plan" } }),
        )
        .await;
    let (status, refused) = f
        .call(
            "POST",
            &format!("/api/drafts/{}/send", secret["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");

    let lunch = f
        .ok(
            "POST",
            &drafts,
            json!({ "payload": { "to": [{ "address": "bob@example.org" }], "subject": "Lunch" } }),
        )
        .await;
    let send = format!("/api/drafts/{}/send", lunch["id"].as_str().unwrap());
    let (status, asked) = f.call("POST", &send, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{asked}");
    assert_eq!(asked["reason"], "This leaves the company.");
    f.ok("POST", &send, json!({ "confirmed": true })).await;
    f.send_queued().await;
    let submitted = f.submitted.lock().unwrap().clone();
    let (envelope, _) = submitted
        .iter()
        .find(|(e, _)| e.recipients.contains(&"bob@example.org".to_owned()))
        .unwrap();
    assert!(
        envelope
            .recipients
            .contains(&"archive@example.com".to_owned()),
        "the rule's blind copy is in the envelope: {envelope:?}"
    );
    let runs = f.runs(&flow).await;
    assert!(runs.len() >= 2, "sending runs are logged: {runs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_runs_and_the_saver_rights() {
    let f = fixture().await;
    let flow = f
        .new_flow(
            "Label invoices",
            json!({
                "trigger": { "type": "manual", "next": "label" },
                "nodes": [{ "id": "label", "type": "label", "params": { "name": "Invoices" } }]
            }),
        )
        .await;
    f.turn_on(&flow).await;
    let bob = f.message_with_subject("Re: Weekly news").await;
    let runs = f
        .ok(
            "POST",
            &format!("/api/flows/{flow}/run"),
            json!({ "messages": [bob] }),
        )
        .await;
    assert_eq!(runs[0]["results"][0]["result"], "done", "{runs}");
    let message: MessageId = bob.parse().unwrap();
    assert!(
        f.store
            .labels(message)
            .await
            .unwrap()
            .contains(&"Invoices".to_owned())
    );

    let off = ApiState {
        write: WriteSwitches {
            rules: false,
            ..WriteSwitches::default()
        },
        ..f.state.clone()
    };
    let app = router(off);
    let mut request = request(
        "GET",
        &format!("/api/archives/{}/flows", f.archive),
        Value::Null,
    );
    request
        .headers_mut()
        .insert(header::HOST, "mail.example.com".parse().unwrap());
    request
        .headers_mut()
        .insert(header::COOKIE, f.cookie.parse().unwrap());
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "the rules switch is off"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn previews_a_template_against_a_message() {
    let f = fixture().await;
    let rules = Rules::new(f.state.clone(), RuleLimits::default());
    let carol = f.arrive(&rules, FROM_CAROL, false).await;
    let url = format!("/api/archives/{}/flows/preview", f.archive);
    let shown = f
        .ok(
            "POST",
            &url,
            json!({ "template": "Hi {{ sender.name }}: {{ subject }}", "message": carol.to_string(),
                    "trigger": "message.archived" }),
        )
        .await;
    assert_eq!(shown["text"], "Hi Carol: Lunch", "{shown}");
    assert_eq!(shown["context"]["sender"]["address"], "carol@example.org");

    let empty = f
        .ok("POST", &url, json!({ "template": "[{{ subject }}]" }))
        .await;
    assert_eq!(empty["text"], "[]");

    let broken = f.ok("POST", &url, json!({ "template": "{% if %}" })).await;
    assert!(broken["text"].is_null());
    assert!(
        broken["error"]
            .as_str()
            .unwrap()
            .starts_with("template syntax")
    );

    let (status, _) = f
        .call("POST", &url, json!({ "template": "x", "trigger": "nope" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

const REPLY_ELSEWHERE: &[u8] = b"From: Carol <carol@example.org>\r\nReply-To: someone-else@example.net\r\nTo: ada@example.com\r\nSubject: Tea\r\nMessage-ID: <tea-1@example.org>\r\nDate: Wed, 12 Mar 2025 13:00:00 +0000\r\n\r\nTea later?\r\n";
const INVITE: &[u8] = b"From: Bob Stone <bob@example.org>\r\nTo: Ada Lovelace <ada@example.com>\r\nSubject: Invitation: Harbour walk\r\nDate: Mon, 02 Nov 2026 09:00:00 +0000\r\nMessage-ID: <harbour-invite@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nA walk along the harbour.\r\n--alt\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:harbour@example.org\r\nSEQUENCE:1\r\nDTSTAMP:20261102T085900Z\r\nDTSTART:20261204T150000Z\r\nDTEND:20261204T160000Z\r\nSUMMARY:Harbour walk\r\nORGANIZER;CN=Bob Stone:mailto:bob@example.org\r\nATTENDEE;CN=Ada Lovelace;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ada@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--alt--\r\n";

#[derive(Default)]
struct Recorded(std::sync::Mutex<Vec<String>>);

#[async_trait::async_trait]
impl crate::RuleHooks for Recorded {
    async fn flow_paused(&self, _: ArchiveId, flow: holtmail_core::FlowId, reason: &str) {
        self.0
            .lock()
            .unwrap()
            .push(format!("paused {flow} {reason}"));
    }

    async fn notified(
        &self,
        _: ArchiveId,
        flow: holtmail_core::FlowId,
        _: holtmail_core::FlowRunId,
    ) {
        self.0.lock().unwrap().push(format!("notified {flow}"));
    }

    async fn webhook_allowed(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    async fn call(
        &self,
        hook: &str,
        _: ArchiveId,
        flow: holtmail_core::FlowId,
        _: holtmail_core::FlowRunId,
        _: Option<MessageId>,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push(format!("webhook {hook} {flow}"));
        Ok(())
    }
}

impl Recorded {
    fn count(&self, prefix: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }
}

fn recording(f: &Fixture, limits: RuleLimits) -> (std::sync::Arc<Rules>, Arc<Recorded>) {
    let hooks = Arc::new(Recorded::default());
    let state = ApiState {
        rule_hooks: hooks.clone(),
        ..f.state.clone()
    };
    (Rules::new(state, limits), hooks)
}

fn labeller(query: &str, label: &str) -> Value {
    json!({
        "trigger": { "type": "message.archived", "next": "q" },
        "nodes": [
            { "id": "q", "type": "query", "params": { "query": query }, "next": { "yes": "l" } },
            { "id": "l", "type": "label", "params": { "name": label } }
        ]
    })
}

fn as_member(cookie: &str, method: &str, uri: &str, body: Value) -> Request<Body> {
    let mut request = request(method, uri, body);
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    request
}

impl Fixture {
    async fn labels_of(&self, message: MessageId) -> Vec<String> {
        self.store.labels(message).await.unwrap()
    }

    async fn manual(&self, name: &str, nodes: Value) -> String {
        let first = nodes[0]["id"].as_str().unwrap().to_owned();
        let flow = self
            .new_flow(
                name,
                json!({ "trigger": { "type": "manual", "next": first }, "nodes": nodes }),
            )
            .await;
        self.turn_on(&flow).await;
        flow
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_test_flow_never_ends_the_chain_or_trips_the_breaker() {
    let f = fixture().await;
    let stopper = f
        .new_flow(
            "Stop everything",
            json!({
                "trigger": { "type": "message.archived", "next": "s" },
                "nodes": [{ "id": "s", "type": "stop" }]
            }),
        )
        .await;
    let busy = f
        .new_flow("Busy", labeller("from:example.org", "Seen by busy"))
        .await;
    let labeller_flow = f
        .new_flow("Friends", labeller("from:carol@example.org", "Friends"))
        .await;
    f.turn_on(&labeller_flow).await;
    let rules = Rules::new(
        f.state.clone(),
        RuleLimits {
            breaker_runs_per_hour: 1,
            ..RuleLimits::default()
        },
    );
    let carol = f.arrive(&rules, FROM_CAROL, true).await;
    assert_eq!(
        f.labels_of(carol).await,
        ["Friends"],
        "a stop in test mode only logs; later live flows still run"
    );
    f.arrive(&rules, FROM_DAVE, true).await;
    assert_eq!(f.runs(&busy).await.len(), 2);

    f.turn_on(&busy).await;
    let erin = f.arrive(&rules, FROM_ERIN, true).await;
    let flow = f.json(&format!("/api/flows/{busy}")).await;
    assert_eq!(
        flow["paused"],
        Value::Null,
        "test runs carry nothing out, so they never count for the breaker"
    );
    assert!(f.labels_of(erin).await.contains(&"Seen by busy".to_owned()));

    f.turn_on(&stopper).await;
    let again = f.arrive(&rules, FROM_CAROL_AGAIN, true).await;
    assert!(
        f.labels_of(again).await.is_empty(),
        "a live stop ends the chain"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn negated_words_decide_on_live_mail() {
    let f = fixture().await;
    let flow = f
        .new_flow(
            "Personal",
            labeller("-unsubscribe -\"weekly digest\"", "Personal"),
        )
        .await;
    f.turn_on(&flow).await;
    let rules = Rules::new(f.state.clone(), RuleLimits::default());
    let carol = f.arrive(&rules, FROM_CAROL, true).await;
    assert_eq!(
        f.labels_of(carol).await,
        ["Personal"],
        "a message without attachments has no attachment text to wait for"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_action_is_carried_out_once() {
    let f = fixture().await;
    let imap = f.imap().await;
    f.allow(imap.account, &["mail.flags"]).await;
    let flow = f
        .manual(
            "Flag",
            json!([{ "id": "flag", "type": "set_flags", "confirm": true, "params": { "set": ["seen"] } }]),
        )
        .await;
    let runs = f
        .ok(
            "POST",
            &format!("/api/flows/{flow}/run"),
            json!({ "messages": [imap.newsletter.to_string()] }),
        )
        .await;
    assert_eq!(runs[0]["results"][0]["result"], "pending", "{runs}");
    let run = runs[0]["id"].as_str().unwrap();
    let confirm = format!("/api/flow-runs/{run}/actions/0/confirm");
    let (a, b) = tokio::join!(
        f.call("POST", &confirm, Value::Null),
        f.call("POST", &confirm, Value::Null)
    );
    let statuses = [a.0, b.0];
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "two confirmations at once carry the action out once: {statuses:?}"
    );
    assert!(statuses.contains(&StatusCode::CONFLICT));
    let decided = f.json(&format!("/api/flow-runs/{run}")).await;
    assert_eq!(decided["results"][0]["result"], "confirmed", "{decided}");
    let (status, _) = f
        .call(
            "POST",
            &format!("/api/flow-runs/{run}/actions/0/dismiss"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread")]
async fn rule_replies_go_to_the_sender_checked_and_count_per_identity() {
    let f = fixture().await;
    f.sending_identity().await;
    let flow = f
        .new_flow(
            "Away",
            json!({
                "trigger": { "type": "message.archived", "next": "reply" },
                "nodes": [{ "id": "reply", "type": "reply",
                            "params": { "identity": "auto", "body": "Away." } }]
            }),
        )
        .await;
    f.turn_on(&flow).await;
    let rules = Rules::new(
        f.state.clone(),
        RuleLimits {
            sends_per_identity_hour: 1,
            ..RuleLimits::default()
        },
    );
    f.arrive(&rules, REPLY_ELSEWHERE, true).await;
    f.send_queued().await;
    let submitted = f.submitted.lock().unwrap().clone();
    assert_eq!(submitted.len(), 1, "{submitted:?}");
    assert_eq!(
        submitted[0].0.recipients,
        ["carol@example.org"],
        "never Reply-To, which no check ran on"
    );

    f.arrive(&rules, FROM_DAVE, true).await;
    f.send_queued().await;
    assert_eq!(f.submitted.lock().unwrap().len(), 1);
    let runs = f.runs(&flow).await;
    assert_eq!(runs[0]["results"][0]["result"], "skipped", "{runs:?}");
    assert!(
        runs[0]["results"][0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("rate_limited"),
        "an `auto` identity is limited once the route picked it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_rules_and_sending_flows_stay_with_their_saver() {
    let f = fixture().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let guard = f
        .new_flow(
            "Copy",
            json!({
                "trigger": { "type": "message.sending", "next": "bcc" },
                "nodes": [{ "id": "bcc", "type": "add_bcc", "params": { "address": "copies@example.com" } }]
            }),
        )
        .await;
    f.turn_on(&guard).await;
    let (grace, cookie) = f.add_member("grace@example.org").await;
    let grace: holtmail_core::UserId = grace.parse().unwrap();
    f.store.set_grants(grace, &[archive]).await.unwrap();
    f.ok(
        "PUT",
        &format!("/api/users/{grace}/grants/{}", f.archive),
        json!({ "role": "editor" }),
    )
    .await;
    let payload = serde_json::to_value(crate::drafts::DraftPayload {
        subject: "Hi".into(),
        ..crate::drafts::DraftPayload::default()
    })
    .unwrap();
    let decide = |user: holtmail_core::UserId| {
        let f = &f;
        let payload = payload.clone();
        async move {
            let user = f.store.get_user(user).await.unwrap().unwrap();
            let draft = f
                .store
                .create_draft(
                    archive,
                    user.id,
                    &payload,
                    &[],
                    holtmail_core::Timestamp::now(),
                )
                .await
                .unwrap();
            match crate::rules_run::sending_decision(&f.state, &user, &draft).await {
                Ok(overrides) => overrides,
                Err(error) => panic!("{}", error.1),
            }
        }
    };
    assert!(
        decide(grace).await.bcc.is_empty(),
        "another user's sending flow never touches this user's mail"
    );
    let owner = f
        .store
        .get_flow(guard.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decide(owner.saved_by).await.bcc, ["copies@example.com"]);
    assert!(f.audit_kinds().await.iter().any(|k| k == "mail_send"));

    let away = f
        .new_flow(
            "Reply",
            json!({
                "trigger": { "type": "manual", "next": "r" },
                "nodes": [{ "id": "r", "type": "reply", "params": { "identity": "auto", "body": "Hi." } }]
            }),
        )
        .await;
    let bob = f.message_with_subject("Re: Weekly news").await;
    let (status, _, _) = f
        .send(as_member(
            &cookie,
            "PATCH",
            &format!("/api/flows/{away}"),
            json!({ "mode": "on" }),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an editor cannot make another user's flow send as them"
    );
    f.turn_on(&away).await;
    let (status, _, _) = f
        .send(as_member(
            &cookie,
            "POST",
            &format!("/api/flows/{away}/run"),
            json!({ "messages": [bob] }),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_runs_take_a_search_and_move_or_delete() {
    let f = fixture().await;
    let imap = f.imap().await;
    f.allow(imap.account, &["mail.move", "mail.delete"]).await;
    let archive = f
        .manual(
            "Archive news",
            json!([{ "id": "m", "type": "move", "params": { "folder": "archive" } }]),
        )
        .await;
    let runs = f
        .ok(
            "POST",
            &format!("/api/flows/{archive}/run"),
            json!({ "query": "subject:\"weekly news\" from:news@example.com" }),
        )
        .await;
    let runs = runs.as_array().unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["message"], imap.newsletter.to_string());
    assert_eq!(runs[0]["results"][0]["result"], "done", "{runs:?}");

    let delete = f
        .manual("Bin", json!([{ "id": "d", "type": "delete" }]))
        .await;
    let runs = f
        .ok(
            "POST",
            &format!("/api/flows/{delete}/run"),
            json!({ "messages": [imap.newsletter.to_string()] }),
        )
        .await;
    assert_eq!(runs[0]["results"][0]["result"], "done", "{runs}");
    let (status, _) = f
        .call(
            "POST",
            &format!("/api/flows/{delete}/run"),
            json!({ "messages": [imap.newsletter.to_string()], "query": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a selection or a search");
}

#[tokio::test(flavor = "multi_thread")]
async fn schedules_and_upcoming_events_tick_once() {
    let f = fixture().await;
    super::pim_edit::pim(&f).await;
    let schedule = f
        .new_flow(
            "Every minute",
            json!({
                "trigger": { "type": "schedule", "params": { "cron": "* * * * *", "timezone": "Europe/Berlin" }, "next": "n" },
                "nodes": [{ "id": "n", "type": "notify", "params": { "title": "Tick" } }]
            }),
        )
        .await;
    f.turn_on(&schedule).await;
    let upcoming = f
        .new_flow(
            "Before standup",
            json!({
                "trigger": { "type": "event.upcoming", "params": { "minutes_before": 15 }, "next": "n" },
                "nodes": [{ "id": "n", "type": "notify", "params": { "title": "{{ event.summary }} soon" } }]
            }),
        )
        .await;
    f.turn_on(&upcoming).await;
    let (rules, hooks) = recording(&f, RuleLimits::default());
    let at = |hms: &str| {
        holtmail_core::Timestamp::from_millis(
            chrono::DateTime::parse_from_rfc3339(&format!("2026-01-05T{hms}Z"))
                .unwrap()
                .timestamp_millis(),
        )
    };
    rules.tick(at("08:42:30"), at("08:45:00")).await.unwrap();
    rules.run_due().await;
    rules.tick(at("08:42:30"), at("08:45:00")).await.unwrap();
    rules.run_due().await;
    let schedule_id: holtmail_core::FlowId = schedule.parse().unwrap();
    assert_eq!(
        hooks.count(&format!("notified {schedule_id}")),
        3,
        "08:43, 08:44 and 08:45, each once"
    );
    let runs = f.runs(&upcoming).await;
    assert_eq!(runs.len(), 1, "the 09:00 standup, 15 minutes before");
    assert_eq!(
        runs[0]["plan"]["actions"][0]["action"]["title"], "Standup soon",
        "{runs:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invitations_items_and_webhooks_run_through_the_owners_paths() {
    let f = fixture().await;
    f.sending_identity().await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let import = f
        .store
        .list_accounts(archive)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.kind == AccountKind::Import)
        .unwrap()
        .id;
    f.store
        .set_account_capabilities(
            import,
            &[holtmail_store::Capability::PimRespond]
                .into_iter()
                .collect(),
        )
        .await
        .unwrap();
    let rsvp = f
        .new_flow(
            "Accept walks",
            json!({
                "trigger": { "type": "invitation.received", "next": "a" },
                "nodes": [{ "id": "a", "type": "rsvp", "params": { "answer": "accepted" } }]
            }),
        )
        .await;
    f.turn_on(&rsvp).await;
    let (rules, hooks) = recording(&f, RuleLimits::default());
    let done = f
        .ingest
        .ingest(
            archive,
            Bytes::from_static(INVITE),
            &Placement {
                folder: Some(f.folder.parse().unwrap()),
                uid: Some(40),
                live: true,
                ..Placement::default()
            },
        )
        .await
        .unwrap();
    f.ingest.commit().await.unwrap();
    rules
        .observe(
            archive,
            done.message_id,
            &holtmail_mime::parse(INVITE).unwrap(),
            true,
        )
        .await;
    rules.run_due().await;
    let runs = f.runs(&rsvp).await;
    assert_eq!(runs.len(), 1, "invitation.received is raised: {runs:?}");
    assert_eq!(runs[0]["results"][0]["result"], "done", "{runs:?}");
    f.send_queued().await;
    assert_eq!(
        replies_to(&f.submitted.lock().unwrap(), "bob@example.org"),
        1
    );

    super::pim_edit::pim(&f).await;
    f.allow_pim().await;
    let items = f
        .manual(
            "Remember",
            json!([
                { "id": "e", "type": "create_event",
                  "params": { "title": "Call {{ sender.address }}", "starts": "2026-12-01T10:00:00Z" },
                  "next": { "next": "c" } },
                { "id": "c", "type": "create_contact", "next": { "next": "n" } },
                { "id": "n", "type": "notify", "params": { "title": "Done" } }
            ]),
        )
        .await;
    let message = f.message_with_subject("Re: Weekly news").await;
    let runs = f
        .ok(
            "POST",
            &format!("/api/flows/{items}/run"),
            json!({ "messages": [message] }),
        )
        .await;
    let results: Vec<&str> = runs[0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["result"].as_str().unwrap())
        .collect();
    assert_eq!(results, ["done", "done", "done"], "{runs}");

    let (status, refused) = f
        .call(
            "POST",
            &format!("/api/archives/{}/flows", f.archive),
            json!({ "name": "Hook", "flow": {
                "trigger": { "type": "manual", "next": "w" },
                "nodes": [{ "id": "w", "type": "webhook", "params": { "hook": "0190f4a1-0000-7000-8000-000000000000" } }]
            } }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a webhook rules may not call: {refused}"
    );
    let owner = f
        .store
        .get_flow(rsvp.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .saved_by;
    let hooked = f
        .store
        .create_flow(&holtmail_store::NewFlow {
            archive_id: archive,
            name: "Hook".into(),
            definition: json!({
                "trigger": { "type": "message.archived", "next": "w" },
                "nodes": [{ "id": "w", "type": "webhook", "params": { "hook": "hook-1" } }]
            })
            .to_string(),
            trigger: "message.archived".into(),
            saved_by: owner,
            at: holtmail_core::Timestamp::now(),
        })
        .await
        .unwrap();
    f.turn_on(&hooked.id.to_string()).await;
    f.arrive(&rules, FROM_ERIN, true).await;
    assert_eq!(hooks.count("webhook hook-1"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_event_keeps_an_earlier_stop() {
    let f = fixture().await;
    let stopper = f
        .new_flow(
            "Stop",
            json!({
                "trigger": { "type": "message.archived", "next": "s" },
                "nodes": [{ "id": "s", "type": "stop" }]
            }),
        )
        .await;
    f.turn_on(&stopper).await;
    let later = f
        .new_flow("Later", labeller("from:carol@example.org", "Later"))
        .await;
    f.turn_on(&later).await;
    let rules = Rules::new(f.state.clone(), RuleLimits::default());
    let carol = f.arrive(&rules, FROM_CAROL, false).await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    let event = format!("message.archived:{carol}");
    let stopper_flow = f
        .store
        .get_flow(stopper.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    f.store
        .claim_flow_run(&holtmail_store::NewFlowRun {
            flow_id: stopper_flow.id,
            archive_id: archive,
            version: stopper_flow.version,
            event_id: event.clone(),
            trigger: "message.archived".into(),
            test: false,
            message_id: Some(carol),
            sender: None,
            started_at: holtmail_core::Timestamp::now(),
        })
        .await
        .unwrap()
        .unwrap();
    f.store
        .put_rule_event(&holtmail_store::RuleEvent {
            id: event,
            archive_id: archive,
            trigger: "message.archived".into(),
            message_id: Some(carol),
            item_id: None,
            live: true,
            payload: json!({}),
            at: holtmail_core::Timestamp::now(),
        })
        .await
        .unwrap();
    rules.run_due().await;
    assert!(
        f.labels_of(carol).await.is_empty(),
        "a run cut short may have stopped the chain, so later flows stay out"
    );
    assert!(f.runs(&later).await.is_empty());
}
