use std::collections::BTreeSet;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::*;

fn server() -> Server {
    Server {
        name: "Holtmail".into(),
        mail_host: "mail.example.com".into(),
        web_host: "archive.example.com".into(),
        web_port: 443,
        web_tls: true,
        imap: Ports {
            tls: Some(993),
            starttls: Some(143),
        },
        submission: Ports {
            tls: Some(465),
            starttls: Some(587),
        },
        jmap: true,
        dav: true,
    }
}

fn minimal() -> Server {
    Server {
        name: "Holtmail".into(),
        mail_host: "mail.example.org".into(),
        web_host: "mail.example.org".into(),
        web_port: 8443,
        web_tls: true,
        imap: Ports {
            tls: Some(993),
            starttls: None,
        },
        submission: Ports {
            tls: None,
            starttls: Some(587),
        },
        jmap: false,
        dav: false,
    }
}

fn profile() -> Profile {
    Profile {
        instance: "0190a1b2-0000-7000-8000-00000000000a".into(),
        device: "0190a1b2-0000-7000-8000-0000000000d1".into(),
        account: Account {
            login: "alice@example.com".into(),
            display_name: "Alice Example".into(),
        },
        app_password: "test-pass-word-0001".into(),
        mail: true,
        calendars: true,
        contacts: true,
    }
}

fn golden(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("HOLTMAIL_BLESS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} is missing; run with HOLTMAIL_BLESS=1", path.display()));
    assert_eq!(
        actual,
        expected,
        "the output differs from {}",
        path.display()
    );
}

fn well_formed(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    let mut texts = Vec::new();
    loop {
        match reader.read_event().expect("well-formed XML") {
            Event::Text(text) => texts.push(text.decode().unwrap().into_owned()),
            Event::GeneralRef(r) => texts.push(format!("&{};", r.decode().unwrap())),
            Event::Eof => return texts,
            _ => {}
        }
    }
}

fn uuids(plist: &str) -> Vec<String> {
    plist
        .split("<key>PayloadUUID</key>")
        .skip(1)
        .map(|rest| {
            let start = rest.find("<string>").unwrap() + "<string>".len();
            let end = rest.find("</string>").unwrap();
            rest[start..end].to_owned()
        })
        .collect()
}

#[test]
fn mobileconfig_golden() {
    let out = mobileconfig(&server(), &profile()).unwrap();
    well_formed(&out);
    golden("full.mobileconfig", &out);
}

#[test]
fn mobileconfig_starttls_and_escaping_golden() {
    let mut server = minimal();
    server.name = "Ann's \"Archive\" & <Co>".into();
    server.dav = true;
    let mut profile = profile();
    profile.account.display_name = "Ann <ann> & Co".into();
    profile.app_password = "a&b<c>d\"e'f".into();
    profile.calendars = false;
    let out = mobileconfig(&server, &profile).unwrap();
    let texts = well_formed(&out);
    assert!(out.contains("<string>a&amp;b&lt;c&gt;d&quot;e&apos;f</string>"));
    assert!(texts.iter().any(|t| t.contains("Ann")));
    assert!(!out.contains("CalDAV"));
    golden("starttls-escaped.mobileconfig", &out);
}

#[test]
fn payload_uuids_are_stable_unique_and_per_device() {
    let first = uuids(&mobileconfig(&server(), &profile()).unwrap());
    let again = uuids(&mobileconfig(&server(), &profile()).unwrap());
    assert_eq!(first, again);
    assert_eq!(first.len(), 4);
    assert_eq!(first.iter().collect::<BTreeSet<_>>().len(), 4);
    for uuid in &first {
        let parsed = uuid::Uuid::parse_str(uuid).unwrap();
        assert_eq!(parsed.get_version_num(), 8);
        assert_eq!(uuid, &uuid.to_uppercase());
    }

    let mut other = profile();
    other.device = "0190a1b2-0000-7000-8000-0000000000d2".into();
    let theirs = uuids(&mobileconfig(&server(), &other).unwrap());
    assert!(first.iter().all(|u| !theirs.contains(u)));

    let mut other = profile();
    other.instance = "0190a1b2-0000-7000-8000-00000000000b".into();
    let theirs = uuids(&mobileconfig(&server(), &other).unwrap());
    assert!(first.iter().all(|u| !theirs.contains(u)));

    let mut renamed = profile();
    renamed.account.display_name = "Alice E.".into();
    renamed.app_password = "test-pass-word-0002".into();
    assert_eq!(uuids(&mobileconfig(&server(), &renamed).unwrap()), first);
}

#[test]
fn mobileconfig_refusals() {
    let mut empty = profile();
    empty.mail = false;
    empty.calendars = false;
    empty.contacts = false;
    assert_eq!(mobileconfig(&server(), &empty), Err(SetupError::Empty));

    let mut no_dav = server();
    no_dav.dav = false;
    assert_eq!(
        mobileconfig(&no_dav, &profile()),
        Err(SetupError::NotOffered("dav"))
    );

    let mut no_submission = server();
    no_submission.submission = Ports::default();
    assert_eq!(
        mobileconfig(&no_submission, &profile()),
        Err(SetupError::NotOffered("submission"))
    );

    let mut no_imap = server();
    no_imap.imap = Ports::default();
    assert_eq!(
        mobileconfig(&no_imap, &profile()),
        Err(SetupError::NotOffered("imap"))
    );
    let mut dav_only = profile();
    dav_only.mail = false;
    assert!(mobileconfig(&no_imap, &dav_only).is_ok());

    let mut bad = profile();
    bad.app_password = "line\nbreak".into();
    assert_eq!(
        mobileconfig(&server(), &bad),
        Err(SetupError::Invalid("app_password"))
    );
    let mut bad = profile();
    bad.account.login = "alice".into();
    assert_eq!(
        mobileconfig(&server(), &bad),
        Err(SetupError::Invalid("login"))
    );
    let mut bad = profile();
    bad.device = String::new();
    assert_eq!(
        mobileconfig(&server(), &bad),
        Err(SetupError::Invalid("device"))
    );
    let mut bad = server();
    bad.mail_host = "mail.example.com/evil".into();
    assert_eq!(
        mobileconfig(&bad, &profile()),
        Err(SetupError::Invalid("mail_host"))
    );
    let mut bad = server();
    bad.web_host = "-archive.example.com".into();
    assert_eq!(
        mobileconfig(&bad, &profile()),
        Err(SetupError::Invalid("web_host"))
    );
    let mut bad = server();
    bad.imap.tls = Some(0);
    assert_eq!(
        mobileconfig(&bad, &profile()),
        Err(SetupError::Invalid("port"))
    );
}

#[test]
fn xml_escape_drops_what_xml_cannot_carry() {
    assert_eq!(xml::escape("a\u{1}b\u{FFFE}c\td"), "abc\td");
    assert_eq!(xml::escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
}

#[test]
fn autoconfig_golden() {
    let out = autoconfig(&server(), "Example.com").unwrap();
    well_formed(&out);
    assert!(!out.contains("test-pass-word"));
    golden("full-autoconfig.xml", &out);

    let out = autoconfig(&minimal(), "example.org").unwrap();
    well_formed(&out);
    assert!(!out.contains("addressBook"));
    golden("minimal-autoconfig.xml", &out);
}

#[test]
fn autoconfig_refusals() {
    let mut no_imap = server();
    no_imap.imap = Ports::default();
    assert_eq!(
        autoconfig(&no_imap, "example.com"),
        Err(SetupError::NotOffered("imap"))
    );
    assert_eq!(
        autoconfig(&server(), "example..com"),
        Err(SetupError::Invalid("domain"))
    );
    assert_eq!(
        autoconfig(&server(), "<x>"),
        Err(SetupError::Invalid("domain"))
    );
}

#[test]
fn davx5_links() {
    assert_eq!(
        davx5_link(&server(), "alice@example.com").unwrap(),
        "davx5://alice%40example.com@archive.example.com/dav/"
    );
    let mut on_port = minimal();
    on_port.dav = true;
    assert_eq!(
        davx5_link(&on_port, "bo+b@example.org").unwrap(),
        "davx5://bo%2Bb%40example.org@mail.example.org:8443/dav/"
    );
    assert_eq!(
        davx5_link(&minimal(), "bob@example.org"),
        Err(SetupError::NotOffered("dav"))
    );
    let mut plain = server();
    plain.web_tls = false;
    plain.web_port = 80;
    assert_eq!(
        davx5_link(&plain, "bob@example.org"),
        Err(SetupError::NotOffered("https"))
    );
    assert_eq!(
        davx5_link(&server(), "a b"),
        Err(SetupError::Invalid("login"))
    );
}

#[test]
fn dns_golden() {
    let records = dns_records(&server(), "example.com").unwrap();
    assert!(
        records
            .iter()
            .all(|r| r.ttl == DNS_TTL && r.name.ends_with(".example.com."))
    );
    golden("full-dns.txt", &zone_text(&records));

    golden(
        "minimal-dns.txt",
        &zone_text(&dns_records(&minimal(), "example.org").unwrap()),
    );

    let mut plain = server();
    plain.web_tls = false;
    plain.web_port = 8080;
    golden(
        "plain-web-dns.txt",
        &zone_text(&dns_records(&plain, "example.com").unwrap()),
    );
}

#[test]
fn server_addresses() {
    assert_eq!(server().dav_url(), "https://archive.example.com/dav/");
    assert_eq!(
        server().jmap_url(),
        "https://archive.example.com/.well-known/jmap"
    );
    assert_eq!(minimal().dav_url(), "https://mail.example.org:8443/dav/");
    let mut plain = server();
    plain.web_tls = false;
    plain.web_port = 80;
    assert_eq!(plain.dav_url(), "http://archive.example.com/dav/");
}
