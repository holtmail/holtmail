use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::*;

struct FakeFolder {
    name: &'static str,
    attributes: &'static str,
    uid_validity: u32,
    messages: Vec<(u32, &'static str, &'static [u8])>,
}

fn mailbox() -> Vec<FakeFolder> {
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

async fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(session(socket));
        }
    });
    port
}

async fn session(socket: tokio::net::TcpStream) {
    let folders = Arc::new(mailbox());
    let (read, mut write) = socket.into_split();
    let mut lines = BufReader::new(read).lines();
    write
        .write_all(b"* OK [CAPABILITY IMAP4rev1] fake server ready\r\n")
        .await
        .unwrap();
    let mut selected: Option<usize> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
        let upper = rest.to_ascii_uppercase();
        let mut out = Vec::new();
        if upper.starts_with("LOGIN") {
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
                    out.extend_from_slice(
                        format!(
                            "* {} EXISTS\r\n* OK [UIDVALIDITY {}] UIDs valid\r\n* OK [UIDNEXT {next}] Predicted next UID\r\n{tag} OK [READ-ONLY] EXAMINE completed\r\n",
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
                upper.contains("BODY.PEEK[]"),
                "fetch must not mark mail as read: {line}"
            );
            let set = rest.split_whitespace().nth(2).unwrap();
            let wanted = parse_set(set);
            let folder = &folders[selected.unwrap()];
            for (seq, (uid, flags, raw)) in folder.messages.iter().enumerate() {
                if !wanted.contains(uid) {
                    continue;
                }
                out.extend_from_slice(
                    format!(
                        "* {} FETCH (UID {uid} FLAGS ({flags}) INTERNALDATE \"14-Mar-2025 09:30:00 +0100\" BODY[] {{{}}}\r\n",
                        seq + 1,
                        raw.len()
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(raw);
                out.extend_from_slice(b")\r\n");
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
            Some((a, b)) => out.extend(a.parse::<u32>().unwrap()..=b.parse::<u32>().unwrap()),
            None => out.push(part.parse().unwrap()),
        }
    }
    out
}

fn source(port: u16, password: &str) -> ImapSource {
    ImapSource::new(
        ImapSettings {
            host: "127.0.0.1".into(),
            port: Some(port),
            security: Security::Plain,
            username: "ada@example.com".into(),
        },
        password.into(),
    )
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
