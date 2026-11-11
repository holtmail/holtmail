use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::*;

struct FakeFolder {
    name: &'static str,
    attributes: &'static str,
    uid_validity: u32,
    messages: Vec<(u32, &'static str, &'static [u8])>,
}

struct FakeLabels {
    uid: u32,
    labels: &'static str,
    modseq: u64,
}

fn mailbox(gmail: bool) -> Vec<FakeFolder> {
    if gmail {
        return vec![
            FakeFolder {
                name: "INBOX",
                attributes: "\\HasNoChildren",
                uid_validity: 1,
                messages: vec![(
                    1,
                    "",
                    b"From: ada@example.com\r\nSubject: One\r\n\r\nFirst\r\n",
                )],
            },
            FakeFolder {
                name: "[Gmail]",
                attributes: "\\HasChildren \\Noselect",
                uid_validity: 1,
                messages: vec![],
            },
            FakeFolder {
                name: "[Gmail]/All Mail",
                attributes: "\\All \\HasNoChildren",
                uid_validity: 11,
                messages: vec![
                    (
                        4,
                        "\\Seen",
                        b"From: ada@example.com\r\nSubject: One\r\n\r\nFirst\r\n",
                    ),
                    (
                        9,
                        "\\Flagged",
                        b"From: bob@example.org\r\nSubject: Two\r\n\r\nSecond\r\n",
                    ),
                ],
            },
            FakeFolder {
                name: "[Gmail]/Sent Mail",
                attributes: "\\HasNoChildren \\Sent",
                uid_validity: 12,
                messages: vec![],
            },
            FakeFolder {
                name: "[Gmail]/Spam",
                attributes: "\\HasNoChildren \\Junk",
                uid_validity: 13,
                messages: vec![],
            },
            FakeFolder {
                name: "Projects/&BBIEMAQ2BD0EPgQ1-",
                attributes: "\\HasNoChildren",
                uid_validity: 14,
                messages: vec![],
            },
        ];
    }
    vec![
        FakeFolder {
            name: "INBOX",
            attributes: "\\HasChildren",
            uid_validity: 42,
            messages: vec![
                (
                    3,
                    "\\Seen",
                    b"From: ada@example.com\r\nSubject: One\r\n\r\nFirst\r\n",
                ),
                (
                    7,
                    "\\Flagged $Work",
                    b"From: bob@example.org\r\nSubject: Two\r\n\r\nSecond\r\n",
                ),
            ],
        },
        FakeFolder {
            name: "INBOX.&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-",
            attributes: "\\HasNoChildren \\Sent",
            uid_validity: 43,
            messages: vec![],
        },
        FakeFolder {
            name: "Archive",
            attributes: "\\Noselect \\HasChildren",
            uid_validity: 1,
            messages: vec![],
        },
        FakeFolder {
            name: "Junk",
            attributes: "\\HasNoChildren",
            uid_validity: 9,
            messages: vec![],
        },
    ]
}

fn gmail_labels() -> Vec<FakeLabels> {
    vec![
        FakeLabels {
            uid: 4,
            labels: r#"("\\Inbox" "\\Important" Projects/&BBIEMAQ2BD0EPgQ1-)"#,
            modseq: 100,
        },
        FakeLabels {
            uid: 9,
            labels: r#"(\Sent "Say \"hi\"")"#,
            modseq: 205,
        },
    ]
}

pub(super) async fn serve_port() -> u16 {
    serve().await
}

async fn serve() -> u16 {
    serve_as(false).await
}

async fn serve_as(gmail: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(session(socket, gmail));
        }
    });
    port
}

async fn session(socket: tokio::net::TcpStream, gmail: bool) {
    let folders = Arc::new(mailbox(gmail));
    let labels = gmail_labels();
    let (read, mut write) = socket.into_split();
    let mut lines = BufReader::new(read).lines();
    write
        .write_all(b"* OK [CAPABILITY IMAP4rev1] fake server ready\r\n")
        .await
        .unwrap();
    let mut selected: Option<usize> = None;
    let mut enabled = false;
    while let Ok(Some(line)) = lines.next_line().await {
        let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
        let upper = rest.to_ascii_uppercase();
        let mut out = Vec::new();
        if upper.starts_with("CAPABILITY") {
            let extra = if gmail {
                " X-GM-EXT-1 CONDSTORE ENABLE"
            } else {
                ""
            };
            out.extend_from_slice(
                format!("* CAPABILITY IMAP4rev1{extra}\r\n{tag} OK CAPABILITY completed\r\n")
                    .as_bytes(),
            );
        } else if upper.starts_with("ENABLE CONDSTORE") {
            assert!(gmail, "ENABLE sent to a server without it");
            enabled = true;
            out.extend_from_slice(
                format!("* ENABLED CONDSTORE\r\n{tag} OK ENABLE completed\r\n").as_bytes(),
            );
        } else if upper.starts_with("AUTHENTICATE XOAUTH2") {
            let _ = write.write_all(b"+ \r\n").await;
            let Ok(Some(answer)) = lines.next_line().await else {
                return;
            };
            let decoded = STANDARD.decode(answer.trim()).unwrap_or_default();
            if decoded == b"user=ada@example.com\x01auth=Bearer good-token\x01\x01" {
                out.extend_from_slice(format!("{tag} OK AUTHENTICATE completed\r\n").as_bytes());
            } else {
                let error = STANDARD.encode(
                    r#"{"status":"400","schemes":"Bearer","scope":"https://mail.google.com/"}"#,
                );
                let _ = write.write_all(format!("+ {error}\r\n").as_bytes()).await;
                let Ok(Some(empty)) = lines.next_line().await else {
                    return;
                };
                assert!(
                    empty.is_empty(),
                    "the error challenge was not answered with an empty line"
                );
                out.extend_from_slice(
                    format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n").as_bytes(),
                );
            }
        } else if upper.starts_with("LOGIN") {
            if rest.contains("\"ada@example.com\"") && rest.contains("\"secret\"") {
                out.extend_from_slice(format!("{tag} OK LOGIN completed\r\n").as_bytes());
            } else {
                out.extend_from_slice(
                    format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n").as_bytes(),
                );
            }
        } else if upper.starts_with("LIST") {
            for folder in folders.iter() {
                out.extend_from_slice(
                    format!(
                        "* LIST ({}) \".\" \"{}\"\r\n",
                        folder.attributes, folder.name
                    )
                    .as_bytes(),
                );
            }
            out.extend_from_slice(format!("{tag} OK LIST completed\r\n").as_bytes());
        } else if upper.starts_with("EXAMINE") {
            let name = rest[8..].trim().trim_matches('"');
            match folders.iter().position(|f| f.name == name) {
                Some(index) => {
                    selected = Some(index);
                    let folder = &folders[index];
                    let next = folder.messages.iter().map(|m| m.0).max().unwrap_or(0) + 1;
                    let modseq = if enabled {
                        "* OK [HIGHESTMODSEQ 205] Highest\r\n"
                    } else {
                        ""
                    };
                    out.extend_from_slice(
                        format!(
                            "* {} EXISTS\r\n* OK [UIDVALIDITY {}] UIDs valid\r\n* OK [UIDNEXT {next}] Predicted next UID\r\n{modseq}{tag} OK [READ-ONLY] EXAMINE completed\r\n",
                            folder.messages.len(),
                            folder.uid_validity
                        )
                        .as_bytes(),
                    );
                }
                None => out.extend_from_slice(format!("{tag} NO no such mailbox\r\n").as_bytes()),
            }
        } else if upper.starts_with("UID SEARCH") {
            let folder = &folders[selected.unwrap()];
            let uids: Vec<String> = folder.messages.iter().map(|m| m.0.to_string()).collect();
            out.extend_from_slice(
                format!(
                    "* SEARCH {}\r\n{tag} OK SEARCH completed\r\n",
                    uids.join(" ")
                )
                .as_bytes(),
            );
        } else if upper.starts_with("UID FETCH") {
            assert!(
                !upper.contains("BODY[") && !upper.contains("RFC822"),
                "fetch must not mark mail as read: {line}"
            );
            assert_eq!(
                upper.contains("X-GM-"),
                gmail,
                "Gmail items asked of the wrong server: {line}"
            );
            let set = rest.split_whitespace().nth(2).unwrap();
            let wanted = parse_set(set);
            let since: Option<u64> = upper
                .split_once("CHANGEDSINCE ")
                .map(|(_, n)| n.trim_end_matches(')').parse().unwrap());
            assert!(
                since.is_none() || enabled,
                "CHANGEDSINCE before CONDSTORE was enabled"
            );
            let folder = &folders[selected.unwrap()];
            for (seq, (uid, flags, raw)) in folder.messages.iter().enumerate() {
                if !wanted.contains(uid) {
                    continue;
                }
                let known = labels.iter().find(|l| l.uid == *uid);
                let modseq = known.map_or(1, |l| l.modseq);
                if since.is_some_and(|since| modseq <= since) {
                    continue;
                }
                let mut items = format!("UID {uid} FLAGS ({flags})");
                if gmail {
                    items.push_str(&format!(
                        " X-GM-MSGID {} X-GM-LABELS {}",
                        1_000 + uid,
                        known.map_or("()", |l| l.labels)
                    ));
                }
                if since.is_some() {
                    items.push_str(&format!(" MODSEQ ({modseq})"));
                }
                if upper.contains("BODY.PEEK[]") {
                    out.extend_from_slice(
                        format!(
                            "* {} FETCH ({items} INTERNALDATE \"14-Mar-2025 09:30:00 +0100\" BODY[] {{{}}}\r\n",
                            seq + 1,
                            raw.len()
                        )
                        .as_bytes(),
                    );
                    out.extend_from_slice(raw);
                    out.extend_from_slice(b")\r\n");
                } else {
                    out.extend_from_slice(format!("* {} FETCH ({items})\r\n", seq + 1).as_bytes());
                }
            }
            out.extend_from_slice(format!("{tag} OK FETCH completed\r\n").as_bytes());
        } else if upper.starts_with("LOGOUT") {
            out.extend_from_slice(format!("* BYE\r\n{tag} OK LOGOUT completed\r\n").as_bytes());
            let _ = write.write_all(&out).await;
            return;
        } else {
            out.extend_from_slice(format!("{tag} BAD unknown command\r\n").as_bytes());
        }
        if write.write_all(&out).await.is_err() {
            return;
        }
    }
}

fn parse_set(set: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in set.split(',') {
        match part.split_once(':') {
            Some((a, "*")) => out.extend(a.parse::<u32>().unwrap()..=u32::MAX / 2),
            Some((a, b)) => out.extend(a.parse::<u32>().unwrap()..=b.parse::<u32>().unwrap()),
            None => out.push(part.parse().unwrap()),
        }
    }
    out
}

fn source_with(port: u16, credentials: Credentials) -> ImapSource {
    ImapSource::new(
        ImapSettings {
            host: "127.0.0.1".into(),
            port: Some(port),
            security: Security::Plain,
            username: "ada@example.com".into(),
        },
        credentials,
    )
}

fn source(port: u16, password: &str) -> ImapSource {
    source_with(port, Credentials::Password(password.into()))
}

#[tokio::test(flavor = "multi_thread")]
async fn signs_in_with_an_access_token() {
    let port = serve().await;
    let mut session = source_with(port, Credentials::AccessToken("good-token".into()))
        .connect()
        .await
        .unwrap();
    assert_eq!(session.select("INBOX").await.unwrap().uid_validity, 42);
    session.logout().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_refused_access_token_as_a_sign_in_error() {
    let port = serve().await;
    let error = source_with(port, Credentials::AccessToken("expired".into()))
        .connect()
        .await
        .err()
        .unwrap();
    assert!(matches!(error, SourceError::Auth(_)), "{error:?}");
}

#[test]
fn builds_the_xoauth2_response() {
    assert_eq!(
        xoauth2("ada@example.com", "t0k"),
        "user=ada@example.com\u{1}auth=Bearer t0k\u{1}\u{1}"
    );
    assert_eq!(
        format!("{:?}", Credentials::AccessToken("t0k".into())),
        "AccessToken(..)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_folders_with_roles() {
    let port = serve().await;
    let mut session = source(port, "secret").connect().await.unwrap();
    let folders = session.folders().await.unwrap();
    let find = |path: &str| folders.iter().find(|f| f.path == path).unwrap().clone();
    assert_eq!(find("INBOX").role, Some(FolderRole::Inbox));
    let sent = find("INBOX/Отправленные");
    assert_eq!(sent.role, Some(FolderRole::Sent));
    assert!(!find("Archive").selectable);
    assert_eq!(
        find("Junk").role,
        Some(FolderRole::Junk),
        "role not guessed from the name"
    );
    session.logout().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn selects_lists_and_fetches_read_only() {
    let port = serve().await;
    let mut session = source(port, "secret").connect().await.unwrap();
    session.folders().await.unwrap();
    let status = session.select("INBOX").await.unwrap();
    assert_eq!(status.uid_validity, 42);
    assert_eq!(status.uid_next, Some(8));
    assert_eq!(status.exists, 2);
    assert_eq!(session.uids().await.unwrap(), [3, 7]);

    let messages = session.fetch(&[7, 3, 5]).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].uid, 3);
    assert_eq!(messages[0].flags, ["\\Seen"]);
    assert_eq!(
        &messages[0].raw[..],
        b"From: ada@example.com\r\nSubject: One\r\n\r\nFirst\r\n"
    );
    assert_eq!(
        messages[0].received_at,
        Some(Timestamp::from_secs(1_741_941_000))
    );
    assert_eq!(messages[1].flags, ["\\Flagged", "$Work"]);
    assert!(session.fetch(&[]).await.unwrap().is_empty());

    let sent = session.select("INBOX/Отправленные").await.unwrap();
    assert_eq!(
        sent.uid_validity, 43,
        "decoded path did not map back to the server name"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_rejected_credentials_and_unreachable_servers() {
    let port = serve().await;
    assert!(matches!(
        source(port, "wrong").connect().await.err(),
        Some(SourceError::Auth(_))
    ));
    let closed = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    assert!(matches!(
        source(closed, "secret").connect().await.err(),
        Some(SourceError::Connection(_))
    ));
}

#[test]
fn compresses_uid_sets() {
    assert_eq!(uid_set(&[9, 1, 2, 3, 7, 3]), "1:3,7,9");
    assert_eq!(uid_set(&[5]), "5");
}

#[test]
fn reads_settings_with_defaults() {
    let settings = ImapSettings::from_json(
        &serde_json::json!({ "host": "imap.example.com", "username": "ada" }),
    )
    .unwrap();
    assert_eq!(settings.security, Security::Tls);
    assert_eq!(settings.port(), 993);
    assert!(matches!(
        ImapSettings::from_json(&serde_json::json!({ "host": "x" })),
        Err(SourceError::Config(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_flags_without_fetching_content() {
    let port = serve().await;
    let mut session = source(port, "secret").connect().await.unwrap();
    let status = session.select("INBOX").await.unwrap();
    assert_eq!(status.highest_modseq, None);
    let flags = session.flags(1..=7, None).await.unwrap();
    assert_eq!(
        flags,
        [
            RemoteFlags {
                uid: 3,
                flags: vec!["\\Seen".into()],
                labels: None,
            },
            RemoteFlags {
                uid: 7,
                flags: vec!["\\Flagged".into(), "$Work".into()],
                labels: None,
            },
        ]
    );
    assert_eq!(session.flags(4..=6, None).await.unwrap(), []);
    assert!(matches!(
        session.flags(1..=7, Some(3)).await,
        Err(SourceError::Protocol(_))
    ));
    let folders = session.folders().await.unwrap();
    assert!(folders.iter().all(|f| !f.label), "labels outside Gmail");
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_gmail_labels_and_changes_since_a_modseq() {
    let port = serve_as(true).await;
    let mut session = source(port, "secret").connect().await.unwrap();
    let folders = session.folders().await.unwrap();
    let find = |path: &str| folders.iter().find(|f| f.path == path).unwrap().clone();
    assert!(find("INBOX").label);
    assert!(find("[Gmail]/Sent Mail").label);
    assert!(find("Projects/Важное").label);
    let all = find("[Gmail]/All Mail");
    assert_eq!(all.role, Some(FolderRole::All));
    assert!(!all.label);
    assert!(!find("[Gmail]/Spam").label);
    assert!(!find("[Gmail]").label, "a parent is not a label");

    let status = session.select("[Gmail]/All Mail").await.unwrap();
    assert_eq!(status.highest_modseq, Some(205));
    let messages = session.fetch(&[4, 9]).await.unwrap();
    assert_eq!(
        messages[0].labels.as_deref(),
        Some(
            &[
                "Important".to_owned(),
                "Inbox".into(),
                "Projects/Важное".into()
            ][..]
        )
    );
    assert_eq!(messages[0].server_id, Some(1_004));
    assert_eq!(
        messages[1].labels.as_deref(),
        Some(&["Say \"hi\"".to_owned(), "Sent".into()][..])
    );

    let changed = session.flags(1..=9, Some(150)).await.unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].uid, 9);
    assert_eq!(changed[0].flags, ["\\Flagged"]);
    assert_eq!(
        changed[0].labels.as_deref(),
        Some(&["Say \"hi\"".to_owned(), "Sent".into()][..])
    );
    assert_eq!(session.flags(1..=9, Some(205)).await.unwrap(), []);
    session.logout().await.unwrap();
}

async fn serve_tls(ca: &holtmail_core::tls::testing::TestCa) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = ca.acceptor();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(stream) = acceptor.accept(stream).await else {
                continue;
            };
            let (read, mut write) = tokio::io::split(stream);
            let mut lines = BufReader::new(read).lines();
            if write.write_all(b"* OK test server\r\n").await.is_err() {
                continue;
            }
            while let Ok(Some(line)) = lines.next_line().await {
                let tag = line.split(' ').next().unwrap_or("*").to_owned();
                let answer = format!("{tag} NO [AUTHENTICATIONFAILED] refused\r\n");
                if write.write_all(answer.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });
    port
}

fn tls_source(port: u16) -> ImapSource {
    ImapSource::new(
        ImapSettings {
            host: "localhost".into(),
            port: Some(port),
            security: Security::Tls,
            username: "ada@example.com".into(),
        },
        Credentials::Password("secret".into()),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn trusts_extra_roots_only_when_given() {
    let ca = holtmail_core::tls::testing::TestCa::new(&["localhost"]);
    let port = serve_tls(&ca).await;
    let trusted = tls_source(port).with_roots(&ca.roots()).unwrap();
    assert!(matches!(trusted.connect().await, Err(SourceError::Auth(_))));
    assert!(matches!(
        tls_source(port).connect().await,
        Err(SourceError::Connection(_))
    ));
    let other = holtmail_core::tls::testing::TestCa::new(&["localhost"]);
    let wrong = tls_source(port).with_roots(&other.roots()).unwrap();
    assert!(matches!(
        wrong.connect().await,
        Err(SourceError::Connection(_))
    ));
}

#[test]
fn refuses_unusable_extra_roots() {
    let garbage = holtmail_core::TlsRoots::from_der(vec![vec![1, 2, 3]]);
    assert!(matches!(
        tls_source(993).with_roots(&garbage),
        Err(SourceError::Config(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_certificate_for_another_name() {
    let ca = holtmail_core::tls::testing::TestCa::new(&["imap.example.com"]);
    let port = serve_tls(&ca).await;
    let source = tls_source(port).with_roots(&ca.roots()).unwrap();
    assert!(matches!(
        source.connect().await,
        Err(SourceError::Connection(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_on_a_server_that_stops_answering() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            open.push(socket);
        }
    });
    let started = std::time::Instant::now();
    let error = source(port, "secret")
        .with_timeout(Duration::from_millis(300))
        .connect()
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, SourceError::Connection(reason) if reason.contains("no greeting within 0.3 s")),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

async fn listen_and(answer: &'static [u8]) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                if !answer.is_empty() {
                    let _ = socket.write_all(answer).await;
                }
                let mut sink = [0u8; 1024];
                while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {}
            });
        }
    });
    port
}

fn connecting(port: u16, security: Security) -> ImapSource {
    ImapSource::new(
        ImapSettings {
            host: "127.0.0.1".into(),
            port: Some(port),
            security,
            username: "ada@example.com".into(),
        },
        Credentials::Password("secret".into()),
    )
    .with_timeout(Duration::from_secs(1))
}

async fn connect_error(source: ImapSource) -> String {
    let started = std::time::Instant::now();
    let error = match source.connect().await {
        Ok(_) => panic!("connected"),
        Err(error) => error.to_string(),
    };
    assert!(started.elapsed() < Duration::from_secs(5), "{error}");
    error
}

#[tokio::test(flavor = "multi_thread")]
async fn names_a_tls_port_reached_without_tls() {
    let silent = listen_and(b"").await;
    for security in [Security::Plain, Security::Starttls] {
        let error = connect_error(connecting(silent, security)).await;
        assert!(
            error.contains("no greeting") && error.contains("security = tls"),
            "{error}"
        );
    }
    let tls = listen_and(&[0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x32]).await;
    let error = connect_error(connecting(tls, Security::Plain)).await;
    assert!(error.contains("answered with TLS"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn names_a_plain_port_reached_with_tls() {
    let plain = listen_and(b"* OK IMAP4rev1 ready\r\n").await;
    let error = connect_error(connecting(plain, Security::Tls)).await;
    assert!(error.contains("plain or STARTTLS"), "{error}");
}

async fn serve_holtmail(instance: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"* OK ready\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
                    let upper = rest.to_ascii_uppercase();
                    let out = if upper.starts_with("CAPABILITY") {
                        format!("* CAPABILITY IMAP4rev1 ID\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("ID") {
                        format!(
                            "* ID (\"name\" \"Holtmail\" \"holtmail-instance\" \"{instance}\")\r\n{tag} OK done\r\n"
                        )
                    } else if upper.starts_with("LOGOUT") {
                        format!("* BYE bye\r\n{tag} OK done\r\n")
                    } else {
                        format!("{tag} OK done\r\n")
                    };
                    if write.write_all(out.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_server_that_is_this_instance() {
    let port = serve_holtmail("instance-a").await;
    let error = source(port, "secret")
        .refusing_instance("instance-a")
        .connect()
        .await
        .err()
        .unwrap();
    assert!(matches!(error, SourceError::Config(_)), "{error:?}");
    assert!(!error.is_transient());
}

#[tokio::test(flavor = "multi_thread")]
async fn accepts_another_holtmail_instance() {
    let port = serve_holtmail("instance-b").await;
    let session = source(port, "secret")
        .refusing_instance("instance-a")
        .connect()
        .await;
    assert!(session.is_ok());
    let plain = source(port, "secret").connect().await;
    assert!(plain.is_ok());
}
