use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_source::testing::{MailboxProbe, Probed, WriteFolders, writer_conformance};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::*;
use holtmail_source::{Flag, MailboxWriter};

#[derive(Clone)]
struct Message {
    uid: u32,
    flags: Vec<String>,
    labels: Vec<String>,
    modseq: u64,
    date: Option<String>,
    raw: Vec<u8>,
}

struct Folder {
    raw: String,
    path: String,
    attributes: &'static str,
    uid_validity: u32,
    uid_next: u32,
    messages: Vec<Message>,
}

#[derive(Default)]
struct State {
    folders: Vec<Folder>,
    modseq: u64,
    commands: Vec<String>,
}

#[derive(Clone, Copy)]
struct Caps {
    condstore: bool,
    uidplus: bool,
    moves: bool,
    gmail: bool,
}

const FULL: Caps = Caps {
    condstore: true,
    uidplus: true,
    moves: true,
    gmail: false,
};
const BARE: Caps = Caps {
    condstore: false,
    uidplus: false,
    moves: false,
    gmail: false,
};
const GMAIL: Caps = Caps {
    condstore: true,
    uidplus: true,
    moves: true,
    gmail: true,
};

#[derive(Clone)]
struct Fake {
    state: Arc<Mutex<State>>,
    caps: Caps,
    port: u16,
}

impl Fake {
    async fn start(caps: Caps) -> Self {
        let folder = |raw: &str, path: &str, attributes, uid_validity| Folder {
            raw: raw.into(),
            path: path.into(),
            attributes,
            uid_validity,
            uid_next: 1,
            messages: Vec::new(),
        };
        let folders = if caps.gmail {
            vec![
                folder("INBOX", "INBOX", "\\HasNoChildren", 1),
                folder("[Gmail]", "[Gmail]", "\\HasChildren \\Noselect", 2),
                folder("[Gmail]/All Mail", "[Gmail]/All Mail", "\\All", 3),
                folder("[Gmail]/Trash", "[Gmail]/Trash", "\\Trash", 4),
            ]
        } else {
            vec![
                folder("INBOX", "INBOX", "\\HasChildren", 1),
                folder(
                    "INBOX.Archive",
                    "INBOX/Archive",
                    "\\HasNoChildren \\Archive",
                    2,
                ),
                folder(
                    "INBOX.&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-",
                    "INBOX/Отправленные",
                    "\\HasNoChildren \\Sent",
                    3,
                ),
                folder("Shared", "Shared", "\\Noselect \\HasChildren", 4),
            ]
        };
        let state = Arc::new(Mutex::new(State {
            folders,
            modseq: 10,
            commands: Vec::new(),
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(session(socket, shared.clone(), caps));
            }
        });
        Self { state, caps, port }
    }

    fn source(&self) -> ImapSource {
        ImapSource::new(
            ImapSettings {
                host: "127.0.0.1".into(),
                port: Some(self.port),
                security: Security::Plain,
                username: "ada@example.com".into(),
            },
            Credentials::Password("secret".into()),
        )
    }

    fn commands(&self) -> Vec<String> {
        self.state.lock().unwrap().commands.clone()
    }

    fn mark(&self, path: &str, uid: u64, flag: &str) {
        let mut state = self.state.lock().unwrap();
        let folder = state.folders.iter_mut().find(|f| f.path == path).unwrap();
        let message = folder
            .messages
            .iter_mut()
            .find(|m| u64::from(m.uid) == uid)
            .unwrap();
        message.flags.push(flag.into());
    }
}

#[async_trait]
impl MailboxProbe for Fake {
    async fn deliver(&self, path: &str, raw: &[u8]) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.modseq += 1;
        let modseq = state.modseq;
        let folder = state.folders.iter_mut().find(|f| f.path == path).unwrap();
        let uid = folder.uid_next;
        folder.uid_next += 1;
        folder.messages.push(Message {
            uid,
            flags: Vec::new(),
            labels: Vec::new(),
            modseq,
            date: None,
            raw: raw.to_vec(),
        });
        u64::from(uid)
    }

    async fn messages(&self, path: &str) -> Vec<Probed> {
        let state = self.state.lock().unwrap();
        let folder = state.folders.iter().find(|f| f.path == path).unwrap();
        folder
            .messages
            .iter()
            .map(|m| Probed {
                uid: u64::from(m.uid),
                flags: m.flags.clone(),
                labels: self.caps.gmail.then(|| {
                    let mut labels = m.labels.clone();
                    labels.sort();
                    labels
                }),
                modseq: self.caps.condstore.then_some(m.modseq),
                raw: Bytes::from(m.raw.clone()),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Atom(String),
    Str(String),
    List(Vec<Tok>),
}

impl Tok {
    fn text(&self) -> String {
        match self {
            Tok::Atom(s) | Tok::Str(s) => s.clone(),
            Tok::List(_) => panic!("a list where text was expected: {self:?}"),
        }
    }

    fn list(&self) -> Vec<Tok> {
        match self {
            Tok::List(items) => items.clone(),
            other => panic!("not a list: {other:?}"),
        }
    }
}

fn tokens(line: &str) -> Vec<Tok> {
    fn parse(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Vec<Tok> {
        let mut out = Vec::new();
        while let Some(&c) = chars.peek() {
            match c {
                ' ' => {
                    chars.next();
                }
                '(' => {
                    chars.next();
                    out.push(Tok::List(parse(chars)));
                }
                ')' => {
                    chars.next();
                    return out;
                }
                '"' => {
                    chars.next();
                    let mut s = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => s.extend(chars.next()),
                            '"' => break,
                            c => s.push(c),
                        }
                    }
                    out.push(Tok::Str(s));
                }
                _ => {
                    let mut s = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == ' ' || c == '(' || c == ')' {
                            break;
                        }
                        s.push(c);
                        chars.next();
                    }
                    out.push(Tok::Atom(s));
                }
            }
        }
        out
    }
    parse(&mut line.chars().peekable())
}

fn parse_set(set: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in set.split(',') {
        match part.split_once(':') {
            Some((a, b)) => {
                let a: u32 = a.parse().unwrap();
                let b: u32 = if b == "*" {
                    u32::MAX
                } else {
                    b.parse().unwrap()
                };
                out.extend(a.min(b)..=a.max(b).min(a.min(b) + 100_000));
            }
            None => out.push(part.parse().unwrap()),
        }
    }
    out
}

fn format_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn has(flags: &[String], flag: &str) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
}

fn label_of(tok: &Tok) -> String {
    match tok {
        Tok::Atom(atom) => labels::label_name(atom),
        Tok::Str(s) => utf7::decode(s),
        Tok::List(_) => panic!("a list as a label"),
    }
}

async fn session(socket: tokio::net::TcpStream, state: Arc<Mutex<State>>, caps: Caps) {
    let (read, mut write) = socket.into_split();
    let mut reader = BufReader::new(read);
    write
        .write_all(b"* OK [CAPABILITY IMAP4rev1] fake writable server ready\r\n")
        .await
        .unwrap();
    let mut selected: Option<usize> = None;
    let mut enabled = false;
    loop {
        let mut bytes = Vec::new();
        if !matches!(reader.read_until(b'\n', &mut bytes).await, Ok(n) if n > 0) {
            return;
        }
        let line = String::from_utf8_lossy(&bytes).trim_end().to_owned();
        let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
        let tag = tag.to_owned();
        let upper = rest.to_ascii_uppercase();
        let args = tokens(rest);
        let mut out = String::new();
        let command = args
            .first()
            .map(Tok::text)
            .unwrap_or_default()
            .to_ascii_uppercase();
        let uid_command = args
            .get(1)
            .map(Tok::text)
            .unwrap_or_default()
            .to_ascii_uppercase();
        let logged = if command == "UID" {
            format!("UID {uid_command}")
        } else {
            command.clone()
        };
        state.lock().unwrap().commands.push(logged.clone());
        match logged.as_str() {
            "CAPABILITY" => {
                let mut extra = String::from(" ENABLE");
                for (on, name) in [
                    (caps.condstore, " CONDSTORE"),
                    (caps.uidplus, " UIDPLUS"),
                    (caps.moves, " MOVE"),
                    (caps.gmail, " X-GM-EXT-1"),
                ] {
                    if on {
                        extra.push_str(name);
                    }
                }
                out = format!("* CAPABILITY IMAP4rev1{extra}\r\n{tag} OK done\r\n");
            }
            "LOGIN" => out = format!("{tag} OK LOGIN completed\r\n"),
            "ENABLE" => {
                assert!(caps.condstore, "ENABLE CONDSTORE without CONDSTORE");
                enabled = true;
                out = format!("* ENABLED CONDSTORE\r\n{tag} OK done\r\n");
            }
            "NOOP" => out = format!("{tag} OK done\r\n"),
            "LIST" => {
                let delimiter = if caps.gmail { "/" } else { "." };
                for folder in &state.lock().unwrap().folders {
                    out.push_str(&format!(
                        "* LIST ({}) \"{delimiter}\" \"{}\"\r\n",
                        folder.attributes, folder.raw
                    ));
                }
                out.push_str(&format!("{tag} OK LIST completed\r\n"));
            }
            "EXAMINE" | "SELECT" => {
                let name = args[1].text();
                let state = state.lock().unwrap();
                match state
                    .folders
                    .iter()
                    .position(|f| f.raw == name && !f.attributes.contains("Noselect"))
                {
                    Some(index) => {
                        selected = Some(index);
                        let folder = &state.folders[index];
                        out.push_str(&format!(
                            "* {} EXISTS\r\n* FLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft)\r\n\
                             * OK [PERMANENTFLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft \\*)] ok\r\n\
                             * OK [UIDVALIDITY {}] ok\r\n* OK [UIDNEXT {}] ok\r\n",
                            folder.messages.len(),
                            folder.uid_validity,
                            folder.uid_next
                        ));
                        if enabled {
                            out.push_str(&format!("* OK [HIGHESTMODSEQ {}] ok\r\n", state.modseq));
                        }
                        let mode = if command == "SELECT" {
                            "READ-WRITE"
                        } else {
                            "READ-ONLY"
                        };
                        out.push_str(&format!("{tag} OK [{mode}] done\r\n"));
                    }
                    None => out = format!("{tag} NO no such mailbox\r\n"),
                }
            }
            "UID SEARCH" => {
                let state = state.lock().unwrap();
                let folder = &state.folders[selected.unwrap()];
                let mut found: Vec<u32> = folder.messages.iter().map(|m| m.uid).collect();
                let mut keys = args[2..].iter();
                while let Some(key) = keys.next() {
                    match key.text().to_ascii_uppercase().as_str() {
                        "ALL" => {}
                        "UID" => {
                            let set = parse_set(&keys.next().unwrap().text());
                            found.retain(|uid| set.contains(uid));
                        }
                        "DELETED" => found.retain(|uid| {
                            let m = folder.messages.iter().find(|m| m.uid == *uid).unwrap();
                            has(&m.flags, "\\Deleted")
                        }),
                        other => panic!("search key {other}"),
                    }
                }
                let list: Vec<String> = found.iter().map(u32::to_string).collect();
                out = format!("* SEARCH {}\r\n{tag} OK done\r\n", list.join(" "))
                    .replace("SEARCH \r\n", "SEARCH\r\n");
            }
            "UID STORE" => {
                let set = parse_set(&args[2].text());
                let mut at = 3;
                let mut since = None;
                if let Tok::List(condition) = &args[at] {
                    assert!(
                        caps.condstore && enabled,
                        "UNCHANGEDSINCE without CONDSTORE"
                    );
                    assert_eq!(condition[0].text().to_ascii_uppercase(), "UNCHANGEDSINCE");
                    since = Some(condition[1].text().parse::<u64>().unwrap());
                    at += 1;
                }
                let item = args[at].text().to_ascii_uppercase();
                let values = args[at + 1].list();
                let silent = item.ends_with(".SILENT");
                let labels = item.contains("X-GM-LABELS");
                assert!(
                    !labels || caps.gmail,
                    "X-GM-LABELS on a server without them"
                );
                let adding = item.starts_with('+');
                let mut state = state.lock().unwrap();
                let mut modseq = state.modseq;
                let mut changed = Vec::new();
                let folder = &mut state.folders[selected.unwrap()];
                for (seq, message) in folder.messages.iter_mut().enumerate() {
                    if !set.contains(&message.uid) {
                        continue;
                    }
                    if since.is_some_and(|since| message.modseq > since) {
                        changed.push(message.uid);
                        continue;
                    }
                    let before = (message.flags.clone(), message.labels.clone());
                    for value in &values {
                        if labels {
                            let label = label_of(value);
                            message.labels.retain(|l| *l != label);
                            if adding {
                                message.labels.push(label);
                            }
                        } else {
                            let flag = value.text();
                            message.flags.retain(|f| !f.eq_ignore_ascii_case(&flag));
                            if adding {
                                message.flags.push(flag);
                            }
                        }
                    }
                    let mut after = (message.flags.clone(), message.labels.clone());
                    let mut was = before.clone();
                    was.0.sort();
                    was.1.sort();
                    after.0.sort();
                    after.1.sort();
                    if was != after {
                        modseq += 1;
                        message.modseq = modseq;
                    }
                    if !silent {
                        let mut items =
                            format!("UID {} FLAGS ({})", message.uid, message.flags.join(" "));
                        if enabled {
                            items.push_str(&format!(" MODSEQ ({})", message.modseq));
                        }
                        out.push_str(&format!("* {} FETCH ({items})\r\n", seq + 1));
                    }
                }
                state.modseq = modseq;
                if changed.is_empty() {
                    out.push_str(&format!("{tag} OK STORE completed\r\n"));
                } else {
                    out.push_str(&format!(
                        "{tag} OK [MODIFIED {}] Conditional STORE failed\r\n",
                        format_set(&changed)
                    ));
                }
            }
            "UID COPY" | "UID MOVE" => {
                assert!(logged == "UID COPY" || caps.moves, "MOVE without MOVE");
                let set = parse_set(&args[2].text());
                let name = args[3].text();
                let source = selected.unwrap();
                let target = state
                    .lock()
                    .unwrap()
                    .folders
                    .iter()
                    .position(|f| f.raw == name);
                let Some(target) = target else {
                    out = format!("{tag} NO [TRYCREATE] no such mailbox\r\n");
                    write.write_all(out.as_bytes()).await.unwrap();
                    continue;
                };
                let mut state = state.lock().unwrap();
                let picked: Vec<Message> = state.folders[source]
                    .messages
                    .iter()
                    .filter(|m| set.contains(&m.uid))
                    .cloned()
                    .collect();
                let mut from = Vec::new();
                let mut to = Vec::new();
                for mut message in picked {
                    state.modseq += 1;
                    from.push(message.uid);
                    message.modseq = state.modseq;
                    message
                        .flags
                        .retain(|f| !f.eq_ignore_ascii_case("\\Deleted"));
                    let folder = &mut state.folders[target];
                    message.uid = folder.uid_next;
                    folder.uid_next += 1;
                    to.push(message.uid);
                    folder.messages.push(message);
                }
                let validity = state.folders[target].uid_validity;
                let code = if caps.uidplus && !from.is_empty() {
                    format!(
                        "[COPYUID {validity} {} {}] ",
                        format_set(&from),
                        format_set(&to)
                    )
                } else {
                    String::new()
                };
                if logged == "UID MOVE" {
                    out.push_str(&format!("* OK {code}moving\r\n"));
                    let folder = &mut state.folders[source];
                    while let Some(seq) =
                        folder.messages.iter().rposition(|m| from.contains(&m.uid))
                    {
                        folder.messages.remove(seq);
                        out.push_str(&format!("* {} EXPUNGE\r\n", seq + 1));
                    }
                    out.push_str(&format!("{tag} OK done\r\n"));
                } else {
                    out.push_str(&format!("{tag} OK {code}done\r\n"));
                }
            }
            "UID EXPUNGE" | "EXPUNGE" => {
                assert!(
                    logged == "EXPUNGE" || caps.uidplus,
                    "UID EXPUNGE without UIDPLUS"
                );
                let set = (logged == "UID EXPUNGE").then(|| parse_set(&args[2].text()));
                let mut state = state.lock().unwrap();
                let folder = &mut state.folders[selected.unwrap()];
                while let Some(seq) = folder.messages.iter().rposition(|m| {
                    has(&m.flags, "\\Deleted") && set.as_ref().is_none_or(|s| s.contains(&m.uid))
                }) {
                    folder.messages.remove(seq);
                    out.push_str(&format!("* {} EXPUNGE\r\n", seq + 1));
                }
                out.push_str(&format!("{tag} OK done\r\n"));
            }
            "APPEND" => {
                let name = args[1].text();
                let flags = args.iter().find_map(|t| match t {
                    Tok::List(items) => Some(items.iter().map(Tok::text).collect::<Vec<_>>()),
                    _ => None,
                });
                let date = args[2..].iter().find_map(|t| match t {
                    Tok::Str(s) => Some(s.clone()),
                    _ => None,
                });
                let size: usize = upper
                    .rsplit_once('{')
                    .and_then(|(_, n)| n.trim_end_matches('}').parse().ok())
                    .unwrap();
                let target = state
                    .lock()
                    .unwrap()
                    .folders
                    .iter()
                    .position(|f| f.raw == name && !f.attributes.contains("Noselect"));
                let Some(target) = target else {
                    out = format!("{tag} NO [TRYCREATE] no such mailbox\r\n");
                    write.write_all(out.as_bytes()).await.unwrap();
                    continue;
                };
                write
                    .write_all(b"+ Ready for literal data\r\n")
                    .await
                    .unwrap();
                let mut raw = vec![0; size];
                reader.read_exact(&mut raw).await.unwrap();
                let mut end = Vec::new();
                reader.read_until(b'\n', &mut end).await.unwrap();
                let mut state = state.lock().unwrap();
                state.modseq += 1;
                let modseq = state.modseq;
                let folder = &mut state.folders[target];
                let uid = folder.uid_next;
                folder.uid_next += 1;
                folder.messages.push(Message {
                    uid,
                    flags: flags.unwrap_or_default(),
                    labels: Vec::new(),
                    modseq,
                    date,
                    raw,
                });
                let code = if caps.uidplus {
                    format!("[APPENDUID {} {uid}] ", folder.uid_validity)
                } else {
                    String::new()
                };
                out = format!("{tag} OK {code}APPEND completed\r\n");
            }
            "LOGOUT" => {
                let _ = write
                    .write_all(format!("* BYE\r\n{tag} OK done\r\n").as_bytes())
                    .await;
                return;
            }
            _ => out = format!("{tag} BAD unknown command\r\n"),
        }
        if write.write_all(out.as_bytes()).await.is_err() {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_writer_contract_with_condstore_move_and_uidplus() {
    let fake = Fake::start(FULL).await;
    let writer = fake.source().writer().await.unwrap();
    let caps = writer.capabilities();
    assert!(caps.conditional && caps.move_uids && caps.append_uid && !caps.labels);
    writer_conformance(
        &writer,
        &fake,
        WriteFolders {
            folder: "INBOX",
            target: "INBOX/Отправленные",
            all: None,
        },
    )
    .await;
    let commands = fake.commands();
    assert!(commands.iter().any(|c| c == "UID MOVE"));
    assert!(commands.iter().any(|c| c == "UID EXPUNGE"));
    assert!(!commands.iter().any(|c| c == "EXPUNGE" || c == "UID COPY"));
    assert!(!commands.iter().any(|c| c == "EXAMINE"), "a writer selects");
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_writer_contract_on_a_bare_server() {
    let fake = Fake::start(BARE).await;
    let writer = fake.source().writer().await.unwrap();
    let caps = writer.capabilities();
    assert!(!caps.conditional && !caps.move_uids && !caps.append_uid && !caps.labels);
    writer_conformance(
        &writer,
        &fake,
        WriteFolders {
            folder: "INBOX",
            target: "INBOX/Archive",
            all: None,
        },
    )
    .await;
    let commands = fake.commands();
    assert!(commands.iter().any(|c| c == "UID COPY"));
    assert!(commands.iter().any(|c| c == "EXPUNGE"));
    assert!(
        !commands
            .iter()
            .any(|c| c == "UID MOVE" || c == "UID EXPUNGE")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_writer_contract_on_gmail() {
    let fake = Fake::start(GMAIL).await;
    let writer = fake.source().writer().await.unwrap();
    assert!(writer.capabilities().labels);
    writer_conformance(
        &writer,
        &fake,
        WriteFolders {
            folder: "INBOX",
            target: "[Gmail]/Trash",
            all: Some("[Gmail]/All Mail"),
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_gmail_labels_in_gmail_spelling() {
    let fake = Fake::start(GMAIL).await;
    let writer = fake.source().writer().await.unwrap();
    let uid = fake
        .deliver("[Gmail]/All Mail", b"Subject: x\r\n\r\nx\r\n")
        .await;
    writer
        .set_labels(
            &[uid],
            &[
                "Important".into(),
                "Projects/Важное".into(),
                "Say \"hi\"".into(),
            ],
            &[],
        )
        .await
        .unwrap();
    let labels = fake.messages("[Gmail]/All Mail").await[0]
        .labels
        .clone()
        .unwrap();
    assert_eq!(labels, ["Important", "Projects/Важное", "Say \"hi\""]);
    writer
        .set_labels(&[uid], &[], &["Important".into()])
        .await
        .unwrap();
    let labels = fake.messages("[Gmail]/All Mail").await[0]
        .labels
        .clone()
        .unwrap();
    assert_eq!(labels, ["Projects/Важное", "Say \"hi\""]);

    let commands = fake.commands().len();
    for (add, remove) in [
        (vec!["Trash".to_string()], vec![]),
        (vec!["spam".to_string()], vec![]),
        (vec![], vec!["Inbox".to_string()]),
        (vec!["\\Trash".to_string()], vec![]),
    ] {
        let error = writer.set_labels(&[uid], &add, &remove).await.unwrap_err();
        assert!(matches!(error, SourceError::Unsupported(_)), "{error:?}");
    }
    assert_eq!(
        fake.commands().len(),
        commands,
        "refused before any command"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn never_expunges_other_messages_without_uidplus() {
    let fake = Fake::start(BARE).await;
    let writer = fake.source().writer().await.unwrap();
    let mine = fake
        .deliver("INBOX", b"Subject: mine\r\n\r\nmine\r\n")
        .await;
    let theirs = fake
        .deliver("INBOX", b"Subject: theirs\r\n\r\ntheirs\r\n")
        .await;
    fake.mark("INBOX", theirs, "\\Deleted");

    let error = writer
        .move_to("INBOX", &[mine], "INBOX/Archive")
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::Protocol(_)), "{error:?}");
    assert!(error.is_transient());
    let inbox = fake.messages("INBOX").await;
    assert_eq!(inbox.len(), 2, "nothing expunged");
    assert!(
        fake.messages("INBOX/Archive").await.is_empty(),
        "a move that could not expunge copies nothing"
    );
    assert!(!fake.commands().iter().any(|c| c == "EXPUNGE"));

    let error = writer.delete("INBOX", &[mine], true).await.unwrap_err();
    assert!(matches!(error, SourceError::Protocol(_)));
    assert_eq!(fake.messages("INBOX").await.len(), 2);

    fake.state.lock().unwrap().folders[0]
        .messages
        .retain(|m| u64::from(m.uid) != theirs);
    let moved = writer
        .move_to("INBOX", &[mine], "INBOX/Archive")
        .await
        .unwrap();
    assert_eq!(moved.uids, [(mine, None)]);
    assert!(fake.messages("INBOX").await.is_empty());
    assert_eq!(fake.messages("INBOX/Archive").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn moves_a_message_already_marked_deleted_without_move() {
    for caps in [
        BARE,
        Caps {
            uidplus: true,
            ..BARE
        },
    ] {
        let fake = Fake::start(caps).await;
        let writer = fake.source().writer().await.unwrap();
        let uid = fake
            .deliver("INBOX", b"Subject: marked\r\n\r\nmarked\r\n")
            .await;
        fake.mark("INBOX", uid, "\\Deleted");
        writer
            .move_to("INBOX", &[uid], "INBOX/Archive")
            .await
            .unwrap();
        assert!(fake.messages("INBOX").await.is_empty());
        assert_eq!(
            fake.messages("INBOX/Archive").await.len(),
            1,
            "a message marked deleted before the move is copied, never lost"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn appends_with_the_received_time_and_reports_the_uid() {
    let fake = Fake::start(FULL).await;
    let writer = fake.source().writer().await.unwrap();
    let raw = b"From: ada@example.com\r\nSubject: Sent\r\n\r\nHello\r\n";
    let uid = writer
        .append(
            "INBOX/Отправленные",
            raw,
            &[Flag::Seen, Flag::Keyword("$HoltmailSent".into())],
            Timestamp::from_secs(1_793_000_000),
        )
        .await
        .unwrap();
    let state = fake.state.lock().unwrap();
    let message = &state.folders[2].messages[0];
    assert_eq!(uid, Some(u64::from(message.uid)));
    assert_eq!(message.raw, raw);
    assert_eq!(message.flags, ["\\Seen", "$HoltmailSent"]);
    assert_eq!(message.date.as_deref(), Some("26-Oct-2026 07:33:20 +0000"));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_backslash_flags_and_bad_keywords_before_connecting() {
    let fake = Fake::start(FULL).await;
    let writer = fake.source().writer().await.unwrap();
    let before = fake.commands().len();
    for bad in ["\\Deleted", "two words", "", "a(b"] {
        assert!(matches!(
            writer
                .set_flags("INBOX", &[1], &[Flag::Keyword(bad.into())], &[], None)
                .await,
            Err(SourceError::Unsupported(_))
        ));
    }
    assert_eq!(fake.commands().len(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn reuses_one_connection_and_names_missing_folders() {
    let fake = Fake::start(FULL).await;
    let writer = fake.source().writer().await.unwrap();
    let uid = fake.deliver("INBOX", b"Subject: a\r\n\r\na\r\n").await;
    for _ in 0..3 {
        writer
            .set_flags("INBOX", &[uid], &[Flag::Seen], &[], None)
            .await
            .unwrap();
    }
    for missing in ["Shared", "Nowhere"] {
        assert!(matches!(
            writer
                .set_flags(missing, &[uid], &[Flag::Seen], &[], None)
                .await,
            Err(SourceError::Missing(_))
        ));
    }
    writer
        .set_flags("INBOX", &[uid], &[], &[Flag::Seen], None)
        .await
        .unwrap();
    let commands = fake.commands();
    assert_eq!(commands.iter().filter(|c| *c == "LOGIN").count(), 1);
    assert_eq!(
        commands.iter().filter(|c| *c == "LIST").count(),
        2,
        "the folder list is cached and read again for an unknown path"
    );
}
