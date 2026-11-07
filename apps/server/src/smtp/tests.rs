use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::*;
use crate::config::{Config, Services};

const REPORT: &[u8] = include_bytes!("../../../../crates/mime/tests/fixtures/journal-report.eml");

#[test]
fn reads_networks_and_patterns() {
    let lan = Network::parse("10.20.0.0/16").unwrap();
    assert!(lan.contains("10.20.3.4".parse().unwrap()));
    assert!(!lan.contains("10.21.0.1".parse().unwrap()));
    assert!(!lan.contains("::1".parse().unwrap()));
    let one = Network::parse("192.0.2.7").unwrap();
    assert!(
        one.contains("192.0.2.7".parse().unwrap()) && !one.contains("192.0.2.8".parse().unwrap())
    );
    assert!(
        Network::parse("0.0.0.0/0")
            .unwrap()
            .contains("203.0.113.9".parse().unwrap())
    );
    assert!(
        Network::parse("fd00::/8")
            .unwrap()
            .contains("fd12::1".parse().unwrap())
    );
    for bad in ["10.0.0.0/33", "mail.example.com", "::1/129", ""] {
        assert!(Network::parse(bad).is_err(), "{bad}");
    }

    let domain = Pattern::parse("*@Archive.example.com").unwrap();
    assert!(domain.matches("Journal@archive.EXAMPLE.com"));
    assert!(!domain.matches("@archive.example.com"));
    assert!(!domain.matches("a@sub.archive.example.com"));
    let one = Pattern::parse("journal@archive.example.com").unwrap();
    assert!(
        one.matches("JOURNAL@archive.example.com") && !one.matches("other@archive.example.com")
    );
    for bad in ["journal", "*@", "@archive.example.com", "a@*.example.com"] {
        assert!(Pattern::parse(bad).is_err(), "{bad}");
    }
}

async fn say(io: &mut BufReader<TcpStream>, line: &str) -> String {
    if !line.is_empty() {
        io.get_mut().write_all(line.as_bytes()).await.unwrap();
    }
    loop {
        let mut reply = String::new();
        io.read_line(&mut reply).await.unwrap();
        if reply.as_bytes().get(3) != Some(&b'-') {
            return reply[..3].to_owned();
        }
    }
}

async fn send(address: SocketAddr, to: &[&str], data: &[u8]) -> Vec<String> {
    let mut io = BufReader::new(TcpStream::connect(address).await.unwrap());
    let mut codes = vec![say(&mut io, "").await];
    codes.push(say(&mut io, "EHLO mta.example.com\r\n").await);
    codes.push(say(&mut io, "MAIL FROM:<journal@contoso.example.com>\r\n").await);
    for rcpt in to {
        codes.push(say(&mut io, &format!("RCPT TO:<{rcpt}>\r\n")).await);
    }
    codes.push(say(&mut io, "DATA\r\n").await);
    if codes.last().map(String::as_str) == Some("354") {
        io.get_mut().write_all(data).await.unwrap();
        codes.push(say(&mut io, "\r\n.\r\n").await);
    }
    codes.push(say(&mut io, "QUIT\r\n").await);
    codes
}

#[tokio::test(flavor = "multi_thread")]
async fn archives_journaled_and_forwarded_mail() {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str(&format!(
        r#"
        data_dir = {:?}
        [smtp]
        listen = "127.0.0.1:0"
        [[smtp.route]]
        recipient = "journal@archive.example.com"
        archive = "Contoso"
        journal = "exchange"
        [[smtp.route]]
        recipient = "*@forward.example.com"
        archive = "Contoso"
        folder = "Forwarded"
        "#,
        dir.path()
    ))
    .unwrap();
    let services = Services::open(&config).await.unwrap();
    let smtp = config.smtp.clone().unwrap();
    let listener = start(&smtp, &*services.store, services.ingest())
        .await
        .unwrap();
    let address = listener.address;

    let codes = send(
        address,
        &["journal@archive.example.com", "nobody@example.net"],
        REPORT,
    )
    .await;
    assert_eq!(
        codes,
        ["220", "250", "250", "250", "550", "354", "250", "221"]
    );
    let plain = b"From: Ada <ada@example.com>\r\nTo: bob@forward.example.com\r\nSubject: Forwarded note\r\nMessage-ID: <fwd-1@example.com>\r\n\r\nHello\r\n";
    let codes = send(
        address,
        &["bob@forward.example.com", "carol@forward.example.com"],
        plain,
    )
    .await;
    assert_eq!(codes.iter().filter(|c| *c == "250").count(), 5, "{codes:?}");

    let archive = crate::export::find_archive(&*services.store, "Contoso")
        .await
        .unwrap();
    let page = services
        .store
        .list_messages(&holtmail_store::MessageQuery {
            archive_id: archive,
            folder_id: None,
            label: None,
            category: None,
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    let mut ids: Vec<_> = page
        .items
        .iter()
        .map(|m| m.subject.clone().unwrap_or_default())
        .collect();
    ids.sort();
    assert_eq!(ids, ["Forwarded note", "Harbour figures"]);
    let journaled = page
        .items
        .iter()
        .find(|m| m.subject.as_deref() == Some("Harbour figures"))
        .unwrap();
    let raw = services
        .blobs
        .get(&journaled.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        holtmail_mime::journal_original(REPORT).unwrap(),
        raw,
        "not unwrapped"
    );
    let folders: Vec<String> = services
        .store
        .list_folders(archive)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.folder.path)
        .collect();
    assert!(folders.contains(&"journal@archive.example.com".to_owned()));
    assert!(folders.contains(&"Forwarded".to_owned()));
    let accounts = services.store.list_accounts(archive).await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].name, ACCOUNT_NAME);

    let forwarded = page
        .items
        .iter()
        .find(|m| m.subject.as_deref() == Some("Forwarded note"))
        .unwrap();
    services
        .store
        .delete_messages(archive, &[forwarded.id], None)
        .await
        .unwrap();
    let codes = send(address, &["bob@forward.example.com"], plain).await;
    assert!(
        codes.contains(&"250".to_owned())
            && !codes
                .iter()
                .any(|c| c.starts_with('4') || c.starts_with('5'))
    );
    let counters = &listener.counters;
    assert_eq!(counters.received.load(Ordering::Relaxed), 2);
    assert_eq!(counters.deleted.load(Ordering::Relaxed), 1);
    assert_eq!(counters.refused_recipients.load(Ordering::Relaxed), 1);
    assert!(
        services
            .store
            .get_message(forwarded.id)
            .await
            .unwrap()
            .is_none()
    );

    let again = routes(&*services.store, &smtp).await.unwrap();
    assert_eq!(again.len(), 2);
    assert_eq!(
        services.store.list_accounts(archive).await.unwrap().len(),
        1,
        "a second account"
    );
}

#[test]
fn checks_the_config() {
    let bad = [
        "listen = \"127.0.0.1:0\"\nrequire_tls = true",
        "listen = \"127.0.0.1:0\"\nusername = \"journal\"",
        "listen = \"127.0.0.1:0\"\ntls_cert = \"/nonexistent.pem\"",
        "listen = \"127.0.0.1:0\"\nallow = [\"everyone\"]",
    ];
    for text in bad {
        let config: SmtpConfig = toml::from_str(text).unwrap();
        assert!(options(&config).is_err(), "{text}");
    }
    let config: SmtpConfig = toml::from_str(
        "listen = \"127.0.0.1:0\"\nusername = \"journal\"\npassword = \"harbour\"\nmax_size_mb = 5",
    )
    .unwrap();
    let (options, networks) = options(&config).unwrap();
    assert_eq!(options.max_size, 5 << 20);
    assert!(options.credentials.is_some());
    assert_eq!(networks.len(), 2);
    assert!(toml::from_str::<SmtpConfig>("listen = \"127.0.0.1:0\"\nrelay = true").is_err());
}
