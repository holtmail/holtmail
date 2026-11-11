//! Holtmail's own IMAP client against this server, over a real socket on this machine.

use holtmail_serve_net::{ConnectionLimiter, Limits, TrustedProxies};
use holtmail_source::{Credentials, Source, SourceError};
use holtmail_source_imap::{ImapSettings, ImapSource, Security};
use tokio::net::TcpListener;

use super::harness::*;
use crate::{ImapServer, PortTls};

async fn listen(instance: &Instance) -> u16 {
    let server = ImapServer {
        shared: instance.shared.clone(),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(std::sync::Arc::new(server).serve(
        listener,
        PortTls::Starttls(None),
        false,
        TrustedProxies::default(),
        ConnectionLimiter::new(Limits::default()),
    ));
    port
}

fn source(port: u16) -> ImapSource {
    ImapSource::new(
        ImapSettings {
            host: "127.0.0.1".into(),
            port: Some(port),
            security: Security::Plain,
            username: EDITOR.into(),
        },
        Credentials::Password(SECRET.into()),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn source_imap_reads_the_served_archive() {
    let instance = Instance::new();
    let port = listen(&instance).await;
    let mut session = source(port).connect().await.unwrap();
    let folders = session.folders().await.unwrap();
    let inbox = folders.iter().find(|f| f.path == "INBOX").unwrap();
    assert_eq!(inbox.role, Some(holtmail_mailview::FolderRole::Inbox));
    let sent = folders.iter().find(|f| f.path == "Sent").unwrap();
    assert_eq!(sent.role, Some(holtmail_mailview::FolderRole::Sent));
    assert!(
        !folders
            .iter()
            .find(|f| f.path == "Accounts")
            .unwrap()
            .selectable
    );
    let status = session.select("INBOX").await.unwrap();
    assert_eq!(status.exists, 3);
    assert!(status.highest_modseq.is_some());
    let uids = session.uids().await.unwrap();
    assert_eq!(uids, vec![1, 2, 3]);
    let messages = session.fetch(&uids).await.unwrap();
    assert_eq!(messages.len(), 3);
    let shared = &messages[1];
    assert!(
        shared.flags.iter().any(|f| f == "\\Seen"),
        "{:?}",
        shared.flags
    );
    let raw = instance
        .world
        .view
        .raw(&instance.world.editor, &instance.world.email("shared"))
        .await
        .unwrap();
    assert_eq!(shared.raw, raw, "the served bytes are the archived bytes");
    session.logout().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn source_imap_refuses_this_instance_as_its_upstream() {
    let instance = Instance::new();
    let port = listen(&instance).await;
    let error = source(port)
        .refusing_instance(TOKEN)
        .connect()
        .await
        .err()
        .unwrap();
    assert!(matches!(error, SourceError::Config(_)), "{error:?}");
    let other = source(port)
        .refusing_instance("another-instance")
        .connect()
        .await;
    assert!(other.is_ok(), "a second instance is allowed");
}

#[tokio::test(flavor = "multi_thread")]
async fn source_imap_writes_through_the_served_archive() {
    use holtmail_source::{Flag, MailboxWriter};
    let instance = Instance::new();
    let port = listen(&instance).await;
    let writer = source(port).writer().await.unwrap();
    let caps = writer.capabilities();
    assert!(
        caps.conditional && caps.move_uids && caps.append_uid,
        "{caps:?}"
    );
    let flagged = writer
        .set_flags("INBOX", &[1], &[Flag::Flagged], &[], None)
        .await
        .unwrap();
    assert_eq!(flagged.updated, vec![1]);
    let stale = writer
        .set_flags("INBOX", &[1], &[Flag::Seen], &[], Some(1))
        .await
        .unwrap();
    assert_eq!(stale.modified, vec![1]);
    let moved = writer.move_to("INBOX", &[3], "Trash").await.unwrap();
    assert_eq!(moved.uids.len(), 1);
    assert!(moved.uids[0].1.is_some(), "{moved:?}");
    let raw = b"From: ada@example.com\r\nSubject: Loopback\r\nMessage-ID: <loop@example.com>\r\n\r\nHi\r\n";
    let uid = writer
        .append(
            "Accounts/Work/Projects/2026",
            raw,
            &[Flag::Seen],
            holtmail_core::Timestamp::from_secs(1_793_700_000),
        )
        .await
        .unwrap();
    assert!(uid.is_some());
    let flags = instance
        .world
        .view
        .flags(
            &instance.world.editor,
            None,
            &[instance.world.email("welcome")],
        )
        .await
        .unwrap();
    assert!(
        flags
            .values()
            .next()
            .unwrap()
            .iter()
            .any(|f| f == "\\Flagged")
    );
}
