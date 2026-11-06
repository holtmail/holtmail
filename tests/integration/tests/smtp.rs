//! SMTP ingestion through a real MTA: Postfix relays to Holtmail's listener over verified
//! STARTTLS; journal reports are unwrapped, unknown recipients refused, oversized mail refused.

use std::process::Command;
use std::time::{Duration, Instant};

use holtmail_integration::{Api, Holtmail, Options, require_it, state_dir, unique, workspace_root};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const LISTEN: u16 = 42525;
const POSTFIX: u16 = 42025;
const REPORT: &[u8] = include_bytes!("../../../crates/mime/tests/fixtures/journal-report.eml");

fn crlf(raw: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(raw)
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .into_bytes()
}

async fn say(io: &mut BufReader<TcpStream>, line: &str) -> String {
    if !line.is_empty() {
        io.get_mut().write_all(line.as_bytes()).await.unwrap();
    }
    loop {
        let mut reply = String::new();
        io.read_line(&mut reply).await.unwrap();
        if reply.as_bytes().get(3) != Some(&b'-') {
            return reply.trim_end().to_owned();
        }
    }
}

async fn send(port: u16, to: &[&str], data: &[u8]) -> Vec<String> {
    let mut io = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
    let mut replies = vec![say(&mut io, "").await];
    replies.push(say(&mut io, "EHLO client.example.org\r\n").await);
    replies.push(say(&mut io, "MAIL FROM:<journal@contoso.example.com>\r\n").await);
    for rcpt in to {
        replies.push(say(&mut io, &format!("RCPT TO:<{rcpt}>\r\n")).await);
    }
    replies.push(say(&mut io, "DATA\r\n").await);
    if replies.last().is_some_and(|r| r.starts_with("354")) {
        io.get_mut().write_all(data).await.unwrap();
        replies.push(say(&mut io, "\r\n.\r\n").await);
    }
    replies.push(say(&mut io, "QUIT\r\n").await);
    replies
}

fn codes(replies: &[String]) -> Vec<&str> {
    replies.iter().map(|r| &r[..3.min(r.len())]).collect()
}

fn postfix_logs() -> String {
    let output = Command::new("docker")
        .args(["compose", "-f"])
        .arg(workspace_root().join("tests/integration/compose.yaml"))
        .args(["logs", "--no-log-prefix", "postfix"])
        .output()
        .expect("docker runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_mail_relayed_by_postfix() {
    require_it!();
    let state = state_dir();
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            r#"
[smtp]
listen = "127.0.0.1:{LISTEN}"
hostname = "localhost"
allow = ["0.0.0.0/0", "::/0"]
max_size_mb = 1
tls_cert = {cert:?}
tls_key = {key:?}
require_tls = true
[[smtp.route]]
recipient = "journal@archive.example.com"
archive = "Journal"
journal = "exchange"
[[smtp.route]]
recipient = "*@bcc.example.com"
archive = "Journal"
folder = "BCC"
"#,
            cert = state.join("server.crt"),
            key = state.join("server-shared.key"),
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;

    let direct = send(
        LISTEN,
        &["journal@archive.example.com"],
        b"Subject: x\r\n\r\nx\r\n",
    )
    .await;
    assert_eq!(codes(&direct)[..3], ["220", "250", "530"], "{direct:?}");

    let marker = unique("relay");
    let plain = crlf(
        format!(
            "From: Ada <ada@example.com>\nTo: bob@bcc.example.com\nSubject: Relayed {marker}\nMessage-ID: <{marker}@example.com>\n\nHello through Postfix\n"
        )
        .as_bytes(),
    );
    let report = crlf(REPORT);
    for (to, data) in [
        (vec!["journal@archive.example.com"], report.clone()),
        (
            vec!["bob@bcc.example.com", "nobody@nowhere.example.net"],
            plain.clone(),
        ),
    ] {
        let replies = send(POSTFIX, &to, &data).await;
        assert!(
            replies
                .iter()
                .any(|r| r.starts_with("250 2.0.0 Ok: queued")),
            "{replies:?}"
        );
    }
    let big = crlf(
        format!(
            "From: Ada <ada@example.com>\nTo: big@bcc.example.com\nSubject: Too big {marker}\n\n{}\n",
            "x".repeat(76).repeat(20_000).as_str()
        )
        .as_bytes(),
    );
    send(POSTFIX, &["big@bcc.example.com"], &big).await;

    let archive = api.archive_named("Journal").await;
    let deadline = Instant::now() + Duration::from_secs(90);
    let listed = loop {
        let listed = api.messages(&archive).await;
        if listed.len() >= 2 {
            break listed;
        }
        assert!(
            Instant::now() < deadline,
            "{} messages arrived; postfix said:\n{}",
            listed.len(),
            postfix_logs()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(listed.len(), 2, "the oversized message was taken");
    let journaled = listed
        .iter()
        .find(|m| m["subject"] == "Harbour figures")
        .expect("the journaled message");
    let raw = api.raw(journaled["id"].as_str().unwrap()).await;
    assert_eq!(
        raw,
        holtmail_mime::journal_original(&report).unwrap().to_vec(),
        "the original was not kept byte for byte"
    );
    let relayed = listed
        .iter()
        .find(|m| m["subject"].as_str().unwrap_or_default().contains(&marker))
        .expect("the relayed message");
    let raw = api.raw(relayed["id"].as_str().unwrap()).await;
    assert!(String::from_utf8_lossy(&raw).contains("Hello through Postfix"));

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let logs = postfix_logs();
        if logs.contains("to=<nobody@nowhere.example.net>")
            && logs.contains("to=<big@bcc.example.com>")
        {
            assert!(
                logs.contains("Verified TLS connection established"),
                "{logs}"
            );
            assert!(
                logs.contains("550 5.1.1"),
                "the unknown recipient was not refused: {logs}"
            );
            assert!(
                logs.contains("exceeds size limit 1048576"),
                "the advertised size was not honoured: {logs}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "{logs}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let status = api.get("/api/smtp").await.ok();
    assert_eq!(status["enabled"], true);
    assert_eq!(status["received"], 2, "{status}");
    assert!(status["refused_recipients"].as_u64() >= Some(1), "{status}");
}
