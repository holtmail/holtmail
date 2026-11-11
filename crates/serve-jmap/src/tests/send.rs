use serde_json::json;

use super::support::*;

#[tokio::test]
async fn a_draft_is_sent_undone_and_leaves_drafts() {
    let h = harness();
    let account = h.account();
    let identities = h
        .call(&h.editor, "Identity/get", json!({"accountId": account}))
        .await;
    let identity = identities["list"][0].clone();
    assert_eq!(identity["email"], "ada@example.com");
    assert_eq!(identity["mayDelete"], false);
    let before = h
        .call(
            &h.editor,
            "EmailSubmission/get",
            json!({"accountId": account, "ids": []}),
        )
        .await["state"]
        .clone();

    let responses = h
        .calls(
            &h.editor,
            json!([
                ["Email/set", {
                    "accountId": account,
                    "create": {"draft": {
                        "mailboxIds": {"drafts": true},
                        "keywords": {"$draft": true, "$seen": true},
                        "from": [{"name": "Ada", "email": "ada@example.com"}],
                        "to": [{"name": "Bob", "email": "bob@example.org"}],
                        "bcc": [{"email": "carol@example.org"}],
                        "subject": "Plans",
                        "bodyValues": {"t": {"value": "See you at noon."}},
                        "textBody": [{"partId": "t", "type": "text/plain"}],
                    }},
                }, "0"],
                ["EmailSubmission/set", {
                    "accountId": account,
                    "create": {"send": {"identityId": identity["id"], "emailId": "#draft"}},
                    "onSuccessDestroyEmail": ["#send"],
                }, "1"],
            ]),
        )
        .await;
    let draft = responses[0][1]["created"]["draft"]["id"].clone();
    assert!(
        draft.as_str().unwrap().starts_with('D'),
        "{}",
        responses[0][1]
    );
    assert_eq!(responses[1][0], "EmailSubmission/set");
    let created = &responses[1][1]["created"]["send"];
    assert_eq!(created["undoStatus"], "pending", "{}", responses[1][1]);
    assert_eq!(responses[2][0], "Email/set");
    assert_eq!(responses[2][1]["destroyed"], json!([draft]));
    let submission = created["id"].clone();

    let got = h
        .call(
            &h.editor,
            "EmailSubmission/get",
            json!({"accountId": account, "ids": [submission]}),
        )
        .await;
    let item = &got["list"][0];
    assert_eq!(item["envelope"]["mailFrom"]["email"], "ada@example.com");
    let recipients: Vec<_> = item["envelope"]["rcptTo"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["email"].clone())
        .collect();
    assert_eq!(
        recipients,
        vec![json!("bob@example.org"), json!("carol@example.org")]
    );
    assert_eq!(
        item["deliveryStatus"]["bob@example.org"]["delivered"],
        "queued"
    );

    let undo = h
        .call(
            &h.editor,
            "EmailSubmission/set",
            json!({"accountId": account, "update": {submission.as_str().unwrap(): {"undoStatus": "canceled"}}}),
        )
        .await;
    assert_eq!(undo["updated"][submission.as_str().unwrap()], json!(null));
    let changes = h
        .call(
            &h.editor,
            "EmailSubmission/changes",
            json!({"accountId": account, "sinceState": before}),
        )
        .await;
    assert_eq!(changes["created"], json!([submission]));
    let query = h
        .call(
            &h.editor,
            "EmailSubmission/query",
            json!({"accountId": account, "filter": {"undoStatus": "canceled"}}),
        )
        .await;
    assert_eq!(query["ids"], json!([submission]));
    let late = h
        .call(
            &h.editor,
            "EmailSubmission/set",
            json!({"accountId": account, "update": {"Snope": {"undoStatus": "canceled"}}}),
        )
        .await;
    assert_eq!(late["notUpdated"]["Snope"]["type"], "notFound");
}

#[tokio::test]
async fn a_sent_message_cannot_be_undone() {
    let h = harness();
    let account = h.account();
    let identity = h
        .call(&h.editor, "Identity/get", json!({"accountId": account}))
        .await["list"][0]["id"]
        .clone();
    let responses = h
        .calls(
            &h.editor,
            json!([
                ["Email/set", {"accountId": account, "create": {"d": {
                    "mailboxIds": {"drafts": true},
                    "from": [{"email": "ada@example.com"}],
                    "to": [{"email": "bob@example.org"}],
                    "subject": "Now",
                    "bodyValues": {"t": {"value": "Right away."}},
                    "textBody": [{"partId": "t"}],
                }}}, "0"],
                ["EmailSubmission/set", {"accountId": account, "create": {"s": {"identityId": identity, "emailId": "#d"}}}, "1"],
            ]),
        )
        .await;
    let id = responses[1][1]["created"]["s"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let outbox = crate::ids::parse_submission(&id).unwrap();
    h.send.deliver(outbox).await;
    let undo = h
        .call(
            &h.editor,
            "EmailSubmission/set",
            json!({"accountId": account, "update": {id.clone(): {"undoStatus": "canceled"}}}),
        )
        .await;
    assert_eq!(undo["notUpdated"][&id]["type"], "cannotUnsend");
    let got = h
        .call(
            &h.editor,
            "EmailSubmission/get",
            json!({"accountId": account, "ids": [id]}),
        )
        .await;
    assert_eq!(got["list"][0]["undoStatus"], "final");
}

#[tokio::test]
async fn a_device_without_send_sees_no_identity() {
    let h = harness();
    let account = h.account();
    let identities = h
        .call(&h.reader, "Identity/get", json!({"accountId": account}))
        .await;
    assert_eq!(identities["list"], json!([]));
    let wrong = h
        .call(
            &h.reader,
            "EmailSubmission/set",
            json!({"accountId": account, "create": {"s": {"identityId": holtmail_core::IdentityId::new().to_string(), "emailId": h.email("welcome")}}}),
        )
        .await;
    assert_eq!(wrong["notCreated"]["s"]["type"], "forbiddenFrom");
    let set = h
        .call(
            &h.editor,
            "Identity/set",
            json!({"accountId": account, "create": {"i": {"email": "x@example.com"}}}),
        )
        .await;
    assert_eq!(set["notCreated"]["i"]["type"], "forbidden");
}
