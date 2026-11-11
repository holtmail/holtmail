use std::net::SocketAddr;

use holtmail_mailview::FolderRole;
use holtmail_source::{Credentials, Source, SourceError};
use holtmail_source_jmap::{JmapAuth, JmapSettings, JmapSource};

use super::support::*;

async fn serve() -> (Harness, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let url = base.clone();
    let h = harness_with(move |s| s.public_url = url);
    let app = h
        .router
        .clone()
        .into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (h, base)
}

fn source(base: &str, creds: &Creds) -> JmapSource {
    JmapSource::new(
        JmapSettings {
            url: base.into(),
            username: creds.login.clone(),
            auth: JmapAuth::Password,
        },
        Credentials::Password(creds.secret.clone()),
    )
    .unwrap()
}

#[tokio::test]
async fn holtmail_reads_holtmail_through_source_jmap() {
    let (h, base) = serve().await;
    let mut session = within(source(&base, &h.editor).connect()).await.unwrap();
    let folders = session.folders().await.unwrap();
    let inbox = folders
        .iter()
        .find(|f| f.role == Some(FolderRole::Inbox))
        .expect("an inbox");
    assert_eq!(inbox.path, "INBOX");
    assert!(
        folders
            .iter()
            .any(|f| f.path == "Accounts/Work/Projects/2026")
    );
    session.select("INBOX").await.unwrap();
    let uids = session.uids().await.unwrap();
    assert_eq!(uids.len(), 3);
    let messages = session.fetch(&uids).await.unwrap();
    let scenario = holtmail_mailview::testing::Scenario::two_accounts();
    let mut raws: Vec<Vec<u8>> = messages.iter().map(|m| m.raw.to_vec()).collect();
    raws.sort();
    let mut expected: Vec<Vec<u8>> = ["welcome", "shared", "reply"]
        .iter()
        .map(|n| scenario.raw(n).as_bytes().to_vec())
        .collect();
    expected.sort();
    assert_eq!(raws, expected);
}

#[tokio::test]
async fn the_loop_guard_refuses_this_instance() {
    let (h, base) = serve().await;
    let refused = within(source(&base, &h.editor).refusing_instance(TOKEN).connect()).await;
    assert!(matches!(refused, Err(SourceError::Config(_))));
    let other = within(
        source(&base, &h.editor)
            .refusing_instance("another-instance")
            .connect(),
    )
    .await;
    assert!(other.is_ok());
}

#[tokio::test]
async fn a_third_party_client_reads_and_flags() {
    use jmap_client::client::{Client, Credentials as ClientCredentials};
    use jmap_client::email;
    let (h, base) = serve().await;
    let client = within(
        Client::new()
            .credentials(ClientCredentials::basic(&h.editor.login, &h.editor.secret))
            .follow_redirects(["127.0.0.1"])
            .connect(&base),
    )
    .await
    .expect("jmap-client connects");
    let inbox = crate::ids::mailbox(&holtmail_mailview::MailboxId::Role(FolderRole::Inbox));
    let found = client
        .email_query(
            Some(email::query::Filter::in_mailbox(inbox)),
            Some([email::query::Comparator::received_at()]),
        )
        .await
        .unwrap();
    let ids = found.ids().to_vec();
    assert_eq!(ids.len(), 3);
    let first = client
        .email_get(&ids[0], None::<Vec<email::Property>>)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.subject(), Some("Welcome aboard"));
    let bytes = client.download(first.blob_id().unwrap()).await.unwrap();
    let scenario = holtmail_mailview::testing::Scenario::two_accounts();
    assert_eq!(bytes, scenario.raw("welcome").as_bytes());
    client
        .email_set_keyword(&ids[0], "$flagged", true)
        .await
        .unwrap();
    let flagged = client
        .email_get(&ids[0], Some([email::Property::Keywords]))
        .await
        .unwrap()
        .unwrap();
    assert!(flagged.keywords().contains(&"$flagged"));
}

#[tokio::test]
async fn websocket_requests_and_pushes() {
    use futures::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let (h, base) = serve().await;
    let url = format!("{}/jmap/ws", base.replacen("http", "ws", 1));
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", h.editor.basic().parse().unwrap());
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "jmap".parse().unwrap());
    let (mut socket, response) = within(tokio_tungstenite::connect_async(request))
        .await
        .unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "jmap");
    let account = h.account();
    let call = json!({
        "@type": "Request",
        "id": "r1",
        "using": USING,
        "methodCalls": [["Email/get", {"accountId": account, "ids": [h.email("welcome")], "properties": ["subject"]}, "0"]],
    });
    socket
        .send(Message::Text(call.to_string().into()))
        .await
        .unwrap();
    let answer = within(next_text(&mut socket)).await;
    assert_eq!(answer["@type"], "Response");
    assert_eq!(answer["requestId"], "r1");
    assert_eq!(
        answer["methodResponses"][0][1]["list"][0]["subject"],
        "Welcome aboard"
    );

    socket
        .send(Message::Text(
            json!({"@type": "WebSocketPushEnable", "dataTypes": ["Email"]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let set = json!({
        "@type": "Request",
        "id": "r2",
        "using": USING,
        "methodCalls": [["Email/set", {"accountId": account, "update": {h.email("welcome"): {"keywords/$flagged": true}}}, "0"]],
    });
    socket
        .send(Message::Text(set.to_string().into()))
        .await
        .unwrap();
    let answer = within(next_text(&mut socket)).await;
    let state = answer["methodResponses"][0][1]["newState"].clone();
    h.hub.poke(h.archive);
    let push = within(next_text(&mut socket)).await;
    assert_eq!(push["@type"], "StateChange");
    assert_eq!(push["changed"][&account]["Email"], state);

    use holtmail_device::DeviceAuth;
    h.auth.revoke(h.editor.device).await.unwrap();
    let ended = within(async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(ended);
}

async fn next_text<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> serde_json::Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).unwrap(),
            Some(Ok(_)) => continue,
            other => panic!("socket ended: {other:?}"),
        }
    }
}

#[tokio::test]
async fn the_jmap_mailbox_writer_flags_served_mail() {
    use holtmail_source::{Flag, MailboxWriter};
    let (h, base) = serve().await;
    let source = source(&base, &h.editor);
    let mut session = within(source.connect()).await.unwrap();
    session.select("INBOX").await.unwrap();
    let uids = session.uids().await.unwrap();
    let messages = session.fetch(&uids).await.unwrap();
    let scenario = holtmail_mailview::testing::Scenario::two_accounts();
    let welcome = messages
        .iter()
        .find(|m| m.raw.as_ref() == scenario.raw("welcome").as_bytes())
        .expect("the welcome message")
        .uid;
    let writer = source.writer();
    within(writer.set_flags("INBOX", &[welcome], &[Flag::Flagged], &[], None))
        .await
        .unwrap();
    let got = h
        .call(
            &h.editor,
            "Email/get",
            serde_json::json!({"accountId": h.account(), "ids": [h.email("welcome")], "properties": ["keywords"]}),
        )
        .await;
    assert_eq!(got["list"][0]["keywords"]["$flagged"], true, "{got}");
}

#[tokio::test]
async fn submit_jmap_sends_through_the_served_account() {
    use holtmail_submit::{Envelope, Submitter};
    use holtmail_submit_jmap::{JmapSecret, JmapSubmitConfig, JmapSubmitter};
    let (h, base) = serve().await;
    let mut config = JmapSubmitConfig::new(
        base.clone(),
        h.editor.login.clone(),
        JmapSecret::Password(h.editor.secret.clone()),
    );
    config.address = Some("ada@example.com".into());
    let submitter = JmapSubmitter::new(config).unwrap();
    within(submitter.check())
        .await
        .expect("the served account can send");
    let message = b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\nSubject: Loopback\r\nMessage-ID: <loopback-1@example.com>\r\nDate: Sat, 7 Nov 2026 10:00:00 +0000\r\n\r\nThrough Holtmail's own JMAP.\r\n";
    let envelope = Envelope {
        mail_from: "ada@example.com".into(),
        recipients: vec!["bob@example.org".into()],
    };
    let report = within(submitter.submit(&envelope, message)).await.unwrap();
    assert_eq!(report.recipients.len(), 1);
    assert_eq!(report.refused().count(), 0);
    let listed = h
        .call(
            &h.editor,
            "EmailSubmission/get",
            serde_json::json!({"accountId": h.account()}),
        )
        .await;
    let list = listed["list"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{listed}");
    assert_eq!(list[0]["envelope"]["rcptTo"][0]["email"], "bob@example.org");
}
