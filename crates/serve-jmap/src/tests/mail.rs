use axum::http::StatusCode;
use holtmail_blob::BlobId;
use holtmail_mailview::{FolderRole, MailboxId};
use serde_json::{Value, json};

use super::support::*;
use crate::ids;

fn inbox() -> String {
    ids::mailbox(&MailboxId::Role(FolderRole::Inbox))
}

fn trash() -> String {
    ids::mailbox(&MailboxId::Role(FolderRole::Trash))
}

#[tokio::test]
async fn mailboxes_follow_the_view_and_the_device() {
    let h = harness();
    let account = h.account();
    let got = h
        .call(&h.editor, "Mailbox/get", json!({"accountId": account}))
        .await;
    let list = got["list"].as_array().unwrap();
    let inbox_box = list.iter().find(|m| m["role"] == "inbox").unwrap();
    assert_eq!(inbox_box["id"], json!(inbox()));
    assert_eq!(inbox_box["name"], "INBOX");
    assert_eq!(inbox_box["parentId"], Value::Null);
    let status = h
        .world
        .view
        .status(&h.world.editor, &MailboxId::Role(FolderRole::Inbox))
        .await
        .unwrap();
    assert_eq!(inbox_box["totalEmails"], json!(status.messages));
    assert_eq!(inbox_box["unreadEmails"], json!(status.unseen));
    assert_eq!(inbox_box["myRights"]["mayAddItems"], true);
    assert_eq!(inbox_box["myRights"]["mayDelete"], false);
    let work = list
        .iter()
        .find(|m| m["name"] == "Work" && m["parentId"].is_string())
        .expect("Accounts/Work");
    let accounts = list.iter().find(|m| m["id"] == work["parentId"]).unwrap();
    assert_eq!(accounts["name"], "Accounts");

    let read_only = h
        .call(
            &h.reader,
            "Mailbox/get",
            json!({"accountId": account, "ids": [inbox()], "properties": ["myRights"]}),
        )
        .await;
    let rights = &read_only["list"][0]["myRights"];
    assert_eq!(rights["mayAddItems"], false);
    assert_eq!(rights["maySetSeen"], true, "local flags");
    assert!(read_only["list"][0].get("name").is_none());

    let query = h
        .call(
            &h.editor,
            "Mailbox/query",
            json!({"accountId": account, "filter": {"role": "trash"}}),
        )
        .await;
    assert_eq!(query["ids"], json!([trash()]));
}

#[tokio::test]
async fn subscriptions_change_and_mailboxes_stay_put() {
    let h = harness();
    let account = h.account();
    let set = h
        .call(
            &h.editor,
            "Mailbox/set",
            json!({
                "accountId": account,
                "create": {"new": {"name": "Holiday"}},
                "update": {"all": {"isSubscribed": true}, "kept": {"name": "Other"}},
                "destroy": [inbox()],
            }),
        )
        .await;
    assert_eq!(set["updated"], json!({"all": null}));
    assert_eq!(set["notCreated"]["new"]["type"], "forbidden");
    assert_eq!(set["notUpdated"]["kept"]["type"], "invalidProperties");
    assert_eq!(set["notDestroyed"][inbox()]["type"], "forbidden");
    let all = h
        .call(
            &h.editor,
            "Mailbox/get",
            json!({"accountId": account, "ids": ["all"]}),
        )
        .await;
    assert_eq!(all["list"][0]["isSubscribed"], true);
    let changes = h
        .call(
            &h.editor,
            "Mailbox/changes",
            json!({"accountId": account, "sinceState": set["oldState"]}),
        )
        .await;
    assert!(
        changes["updated"]
            .as_array()
            .unwrap()
            .contains(&json!("all"))
    );
}

#[tokio::test]
async fn query_then_get_by_reference() {
    let h = harness();
    let account = h.account();
    let responses = h
        .calls(
            &h.editor,
            json!([
                ["Email/query", {
                    "accountId": account,
                    "filter": {"inMailbox": inbox()},
                    "sort": [{"property": "receivedAt", "isAscending": false}],
                    "calculateTotal": true,
                }, "q"],
                ["Email/get", {
                    "accountId": account,
                    "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"},
                    "properties": ["subject", "keywords", "mailboxIds", "from", "receivedAt", "threadId"],
                }, "g"],
            ]),
        )
        .await;
    let query = &responses[0][1];
    assert_eq!(query["total"], 3);
    assert_eq!(
        query["ids"],
        json!([h.email("reply"), h.email("shared"), h.email("welcome")])
    );
    let list = responses[1][1]["list"].as_array().unwrap();
    assert_eq!(list[0]["subject"], "Re: Welcome aboard");
    assert_eq!(
        list[1]["keywords"],
        json!({"$seen": true, "$flagged": true})
    );
    assert_eq!(
        list[2]["from"],
        json!([{"name": "Ada Lovelace", "email": "ada@example.com"}])
    );
    assert_eq!(list[2]["receivedAt"], "2026-11-02T09:00:00Z");
    assert!(list[2]["mailboxIds"][inbox()].as_bool().unwrap());
    assert_eq!(
        list[0]["threadId"], list[2]["threadId"],
        "a reply joins the thread"
    );
    assert!(list[0].get("preview").is_none());

    let thread = h
        .call(
            &h.editor,
            "Thread/get",
            json!({"accountId": account, "ids": [list[0]["threadId"]]}),
        )
        .await;
    assert_eq!(
        thread["list"][0]["emailIds"],
        json!([h.email("welcome"), h.email("reply")])
    );

    let paged = h
        .call(
            &h.editor,
            "Email/query",
            json!({"accountId": account, "filter": {"inMailbox": inbox()}, "anchor": h.email("shared"), "anchorOffset": 1, "limit": 5}),
        )
        .await;
    assert_eq!(paged["ids"], json!([h.email("welcome")]));
    assert_eq!(paged["position"], 2);
    let collapsed = h
        .call(
            &h.editor,
            "Email/query",
            json!({"accountId": account, "filter": {"inMailbox": inbox()}, "collapseThreads": true}),
        )
        .await;
    assert_eq!(collapsed["ids"].as_array().unwrap().len(), 2);
    let bad_sort = h
        .calls(
            &h.editor,
            json!([["Email/query", {"accountId": account, "sort": [{"property": "subject"}]}, "s"]]),
        )
        .await;
    assert_eq!(bad_sort[0][1]["type"], "unsupportedSort");
}

#[tokio::test]
async fn bodies_headers_and_values() {
    let h = harness();
    let account = h.account();
    let got = h
        .call(
            &h.editor,
            "Email/get",
            json!({
                "accountId": account,
                "ids": [h.email("welcome"), h.email("reply"), "Mnope"],
                "fetchTextBodyValues": true,
                "properties": ["textBody", "bodyValues", "messageId", "inReplyTo", "header:Subject:asText", "headers", "preview", "size"],
            }),
        )
        .await;
    assert_eq!(got["notFound"], json!(["Mnope"]));
    let welcome = &got["list"][0];
    let part = welcome["textBody"][0]["partId"].as_str().unwrap();
    assert_eq!(
        welcome["bodyValues"][part]["value"],
        "Glad to have you here.\r\n"
    );
    assert_eq!(welcome["messageId"], json!(["welcome@example.com"]));
    assert_eq!(welcome["header:Subject:asText"], "Welcome aboard");
    assert!(
        welcome["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"] == "Message-ID")
    );
    assert_eq!(got["list"][1]["inReplyTo"], json!(["welcome@example.com"]));
    let raw = h
        .world
        .view
        .raw(&h.world.editor, &h.world.email("welcome"))
        .await
        .unwrap();
    assert_eq!(welcome["size"], json!(raw.len()));
}

#[tokio::test]
async fn attachments_and_raw_bytes_download() {
    let h = harness();
    let account = h.account();
    let got = h
        .call(
            &h.editor,
            "Email/get",
            json!({"accountId": account, "ids": [h.email("report")], "properties": ["attachments", "blobId", "hasAttachment", "bodyStructure"]}),
        )
        .await;
    let email = &got["list"][0];
    assert_eq!(email["hasAttachment"], true);
    let attachment = &email["attachments"][0];
    assert_eq!(attachment["name"], "report.csv");
    assert_eq!(attachment["type"], "text/csv");
    assert_eq!(attachment["size"], 12);
    assert_eq!(email["bodyStructure"]["type"], "multipart/mixed");
    assert_eq!(
        email["bodyStructure"]["subParts"].as_array().unwrap().len(),
        2
    );
    let part = attachment["blobId"].as_str().unwrap();
    let answer = h
        .get(
            &format!("/jmap/download/{account}/{part}/report.csv?accept=text/csv"),
            Some(&h.editor),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(&answer.body[..], b"a,b,c\n1,2,3\n");
    assert_eq!(answer.headers["content-type"], "text/csv");

    let blob = email["blobId"].as_str().unwrap();
    let answer = h
        .get(
            &format!("/jmap/download/{account}/{blob}/m.eml?accept=message/rfc822"),
            Some(&h.editor),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(format!("B{}", BlobId::of(&answer.body)), blob);
    let scenario = holtmail_mailview::testing::Scenario::two_accounts();
    assert_eq!(&answer.body[..], scenario.raw("report").as_bytes());

    let unknown = format!("B{}", BlobId::of(b"nothing"));
    let answer = h
        .get(
            &format!("/jmap/download/{account}/{unknown}/x"),
            Some(&h.editor),
        )
        .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    let other = holtmail_core::ArchiveId::new();
    let answer = h
        .get(&format!("/jmap/download/{other}/{blob}/x"), Some(&h.editor))
        .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn search_and_snippets_match_word_starts() {
    let h = harness();
    let account = h.account();
    let responses = h
        .calls(
            &h.editor,
            json!([
                ["Email/query", {"accountId": account, "filter": {"text": "numbers"}}, "q"],
                ["SearchSnippet/get", {
                    "accountId": account,
                    "filter": {"text": "numbers"},
                    "#emailIds": {"resultOf": "q", "name": "Email/query", "path": "/ids"},
                }, "s"],
            ]),
        )
        .await;
    let ids = responses[0][1]["ids"].as_array().unwrap().clone();
    assert!(ids.contains(&json!(h.email("shared"))));
    assert!(!ids.contains(&json!(h.email("welcome"))));
    let snippets = responses[1][1]["list"].as_array().unwrap();
    let shared = snippets
        .iter()
        .find(|s| s["emailId"] == json!(h.email("shared")))
        .unwrap();
    assert_eq!(shared["subject"], "Quarterly <mark>numbers</mark>");
    assert_eq!(shared["preview"], "The <mark>numbers</mark> are in.");
    let none = h
        .call(
            &h.editor,
            "Email/query",
            json!({"accountId": account, "filter": {"text": "umbers"}}),
        )
        .await;
    assert_eq!(none["ids"], json!([]));
    let not = h
        .call(
            &h.editor,
            "Email/query",
            json!({"accountId": account, "filter": {"inMailboxOtherThan": [inbox()], "hasKeyword": "$seen"}}),
        )
        .await;
    let not = not["ids"].as_array().unwrap();
    assert!(not.contains(&json!(h.email("report"))));
    assert!(!not.contains(&json!(h.email("shared"))));
}

#[tokio::test]
async fn keywords_change_and_show_in_changes() {
    let h = harness();
    let account = h.account();
    let welcome = h.email("welcome");
    let before = h
        .call(
            &h.editor,
            "Email/get",
            json!({"accountId": account, "ids": [], "properties": ["id"]}),
        )
        .await["state"]
        .clone();
    let set = h
        .call(
            &h.editor,
            "Email/set",
            json!({"accountId": account, "ifInState": before, "update": {welcome.clone(): {"keywords/$seen": true, "keywords/$flagged": true}}}),
        )
        .await;
    assert_eq!(set["updated"], json!({welcome.clone(): null}));
    assert_ne!(set["newState"], before);
    let got = h
        .call(
            &h.editor,
            "Email/get",
            json!({"accountId": account, "ids": [welcome], "properties": ["keywords"]}),
        )
        .await;
    assert_eq!(
        got["list"][0]["keywords"],
        json!({"$seen": true, "$flagged": true})
    );
    let changes = h
        .call(
            &h.editor,
            "Email/changes",
            json!({"accountId": account, "sinceState": before}),
        )
        .await;
    assert!(
        changes["updated"]
            .as_array()
            .unwrap()
            .contains(&json!(welcome))
    );
    assert_eq!(changes["hasMoreChanges"], false);
    let again = h
        .call(
            &h.editor,
            "Email/changes",
            json!({"accountId": account, "sinceState": changes["newState"]}),
        )
        .await;
    assert_eq!(again["updated"], json!([]));
    let mailbox_changes = h
        .call(
            &h.editor,
            "Mailbox/changes",
            json!({"accountId": account, "sinceState": before}),
        )
        .await;
    assert!(
        mailbox_changes["updated"]
            .as_array()
            .unwrap()
            .contains(&json!(inbox()))
    );

    let stale = h
        .calls(
            &h.editor,
            json!([["Email/set", {"accountId": account, "ifInState": before, "update": {h.email("reply"): {"keywords/$seen": true}}}, "s"]]),
        )
        .await;
    assert_eq!(stale[0][1]["type"], "stateMismatch");
    let bad = h
        .call(
            &h.editor,
            "Email/set",
            json!({"accountId": account, "update": {h.email("reply"): {"subject": "Changed"}}}),
        )
        .await;
    assert_eq!(
        bad["notUpdated"][h.email("reply")]["type"],
        "invalidProperties"
    );
}

#[tokio::test]
async fn moves_and_destroys_go_through_the_actions() {
    let h = harness();
    let account = h.account();
    let welcome = h.email("welcome");
    let reply = h.email("reply");
    let set = h
        .call(
            &h.editor,
            "Email/set",
            json!({
                "accountId": account,
                "update": {welcome.clone(): {"mailboxIds": {trash(): true}}},
                "destroy": [reply.clone()],
            }),
        )
        .await;
    assert_eq!(set["updated"], json!({welcome.clone(): null}), "{set}");
    assert_eq!(set["destroyed"], json!([reply.clone()]));
    let got = h
        .call(
            &h.editor,
            "Email/get",
            json!({"accountId": account, "ids": [welcome, reply], "properties": ["mailboxIds"]}),
        )
        .await;
    let welcome_boxes = got["list"][0]["mailboxIds"].as_object().unwrap();
    assert!(welcome_boxes.contains_key(&trash()));
    assert!(!welcome_boxes.contains_key(&inbox()));
    let reply_boxes = got["list"][1]["mailboxIds"].as_object().unwrap();
    assert!(reply_boxes.contains_key("kept"), "{reply_boxes:?}");
    let again = h
        .call(
            &h.editor,
            "Email/set",
            json!({"accountId": account, "destroy": [h.email("reply")]}),
        )
        .await;
    assert_eq!(again["notDestroyed"][h.email("reply")]["type"], "forbidden");
}

#[tokio::test]
async fn a_read_only_device_keeps_local_flags_only() {
    let h = harness();
    let account = h.account();
    let shared = h.email("shared");
    let set = h
        .call(
            &h.reader,
            "Email/set",
            json!({
                "accountId": account,
                "update": {
                    shared.clone(): {"keywords/$answered": true},
                    h.email("welcome"): {"mailboxIds": {trash(): true}},
                },
            }),
        )
        .await;
    assert_eq!(set["updated"], json!({shared.clone(): null}));
    assert_eq!(set["notUpdated"][h.email("welcome")]["type"], "forbidden");
    let got = h
        .call(
            &h.reader,
            "Email/get",
            json!({"accountId": account, "ids": [shared], "properties": ["keywords"]}),
        )
        .await;
    assert_eq!(got["list"][0]["keywords"]["$answered"], true);
}

#[tokio::test]
async fn uploads_import_and_stay_private() {
    let h = harness();
    let account = h.account();
    let raw = "From: Gina <gina@example.org>\r\nTo: ada@example.com\r\nSubject: Imported\r\n\
        Date: Sat, 07 Nov 2026 09:00:00 +0000\r\nMessage-ID: <imported@example.org>\r\n\r\nHello.\r\n";
    let answer = h
        .post(
            &format!("/jmap/upload/{account}"),
            &h.editor,
            raw,
            "message/rfc822",
        )
        .await;
    assert_eq!(answer.status, StatusCode::CREATED);
    let upload = answer.json();
    assert_eq!(upload["size"], raw.len());
    assert_eq!(upload["type"], "message/rfc822");
    let blob = upload["blobId"].as_str().unwrap().to_string();
    let download = h
        .get(
            &format!("/jmap/download/{account}/{blob}/x.eml"),
            Some(&h.editor),
        )
        .await;
    assert_eq!(&download.body[..], raw.as_bytes());

    let imported = h
        .call(
            &h.editor,
            "Email/import",
            json!({"accountId": account, "emails": {"i1": {"blobId": blob, "mailboxIds": {inbox(): true}, "keywords": {"$seen": true}}}}),
        )
        .await;
    let created = &imported["created"]["i1"];
    assert_eq!(created["blobId"], json!(blob));
    let got = h
        .call(&h.editor, "Email/get", json!({"accountId": account, "ids": [created["id"]], "properties": ["subject", "mailboxIds"]}))
        .await;
    assert_eq!(got["list"][0]["subject"], "Imported");
    let missing = h
        .call(
            &h.editor,
            "Email/import",
            json!({"accountId": account, "emails": {"i2": {"blobId": format!("B{}", BlobId::of(b"x")), "mailboxIds": {inbox(): true}}}}),
        )
        .await;
    assert_eq!(missing["notCreated"]["i2"]["type"], "blobNotFound");
}

#[tokio::test]
async fn uploads_need_a_writing_device_and_fit_a_budget() {
    let h = harness_with(|s| s.limits.upload_budget = 100);
    let account = h.account();
    let path = format!("/jmap/upload/{account}");
    let refused = h.post(&path, &h.reader, "x".repeat(10), "text/plain").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "a read-only device");
    let first = h.post(&path, &h.editor, "a".repeat(60), "text/plain").await;
    assert_eq!(first.status, StatusCode::CREATED);
    let over = h.post(&path, &h.editor, "b".repeat(60), "text/plain").await;
    assert_eq!(
        over.status,
        StatusCode::BAD_REQUEST,
        "over the user's budget"
    );
    assert_eq!(over.json()["limit"], "maxSizeUpload");
}

#[tokio::test]
async fn new_mail_shows_in_email_and_thread_changes() {
    let h = harness();
    let account = h.account();
    let before = h
        .call(
            &h.editor,
            "Thread/get",
            json!({"accountId": account, "ids": []}),
        )
        .await["state"]
        .clone();
    let email_before = h
        .call(
            &h.editor,
            "Email/get",
            json!({"accountId": account, "ids": []}),
        )
        .await["state"]
        .clone();
    let raw = holtmail_mailview::testing::mail(
        "Hana Ito <hana@example.org>",
        "Bob Builder <bob@example.org>",
        "Fresh news",
        "fresh",
        "Sat, 07 Nov 2026 10:00:00 +0000",
        "",
        "Something new.\r\n",
    );
    let id = h.world.driver.deliver("Work", "INBOX", &raw).await;
    let email = ids::email(&holtmail_mailview::EmailRef::Message(id));
    let changes = h
        .call(
            &h.editor,
            "Email/changes",
            json!({"accountId": account, "sinceState": email_before}),
        )
        .await;
    assert_eq!(changes["created"], json!([email.clone()]));
    let threads = h
        .call(
            &h.editor,
            "Thread/changes",
            json!({"accountId": account, "sinceState": before}),
        )
        .await;
    assert_eq!(threads["created"].as_array().unwrap().len(), 1);
    let first = h
        .call(
            &h.editor,
            "Email/query",
            json!({"accountId": account, "filter": {"inMailbox": inbox()}, "limit": 1}),
        )
        .await;
    assert_eq!(first["ids"], json!([email]));
}
