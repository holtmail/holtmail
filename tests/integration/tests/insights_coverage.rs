//! One-click coverage from a real DKIM verifier (profile `aliases`): newsletters from
//! `news.example.com`, signed by the test with the key the stack publishes in its CoreDNS zone,
//! travel through Postfix in `sl-mx`, where OpenDKIM (`AuthservID mx.example.org`) stamps
//! `Authentication-Results`, to Holtmail's SMTP listener with `mx.example.org` trusted. The
//! signed newsletter is covered; an unsigned copy, a copy whose signature leaves out
//! `List-Unsubscribe-Post` and a copy with a prepended second `List-Unsubscribe` are not.

use std::time::{Duration, Instant};

use holtmail_integration::data::now;
use holtmail_integration::dkim::{NEWS_DOMAIN, news_key};
use holtmail_integration::submit::send;
use holtmail_integration::{Api, Holtmail, LogScanner, Options, profile, require_it, unique};
use serde_json::Value;

const LISTEN: u16 = 42526;
const MX: u16 = 47025;
const RECIPIENT: &str = "reader@example.org";
const SIGNED: &[&str] = &[
    "from",
    "to",
    "subject",
    "date",
    "message-id",
    "list-id",
    "list-unsubscribe",
    "list-unsubscribe-post",
];

struct Letter {
    token: String,
    link: String,
    list: String,
}

impl Letter {
    fn new() -> Self {
        let token = unique("news");
        Self {
            link: format!("https://{NEWS_DOMAIN}/leave/{token}"),
            list: format!("{token}.{NEWS_DOMAIN}"),
            token,
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("From".into(), format!("Example News <news@{NEWS_DOMAIN}>")),
            ("To".into(), format!("<{RECIPIENT}>")),
            ("Subject".into(), format!("Example News {}", self.token)),
            ("Date".into(), now().to_rfc2822()),
            (
                "Message-ID".into(),
                format!("<{}@{NEWS_DOMAIN}>", self.token),
            ),
            ("List-Id".into(), format!("Example News <{}>", self.list)),
            ("List-Unsubscribe".into(), format!("<{}>", self.link)),
            (
                "List-Unsubscribe-Post".into(),
                "List-Unsubscribe=One-Click".into(),
            ),
            ("MIME-Version".into(), "1.0".into()),
            ("Content-Type".into(), "text/plain; charset=utf-8".into()),
        ]
    }

    fn body(&self) -> String {
        "Hello,\n\nThis week: the garden club, the library hours and the bake sale.\n\nThe Example News team\n".into()
    }

    fn signed(&self, names: &[&str]) -> Vec<u8> {
        let headers = self.headers();
        let pairs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        news_key().sign(&pairs, &self.body(), names)
    }

    fn unsigned(&self) -> Vec<u8> {
        let mut out = String::new();
        for (name, value) in self.headers() {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(&self.body().replace('\n', "\r\n"));
        out.into_bytes()
    }
}

async fn wait_for(api: &Api, archive: &str, token: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(found) = api
            .messages(archive)
            .await
            .into_iter()
            .find(|m| m["subject"].as_str().is_some_and(|s| s.contains(token)))
        {
            return found["id"].as_str().expect("an id").to_owned();
        }
        assert!(Instant::now() < deadline, "{token} did not arrive");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The message's insights once stage B has read its trust.
async fn derived(api: &Api, message: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let detail = api.message(message).await;
        let insights = detail["insights"].clone();
        if insights["trust"]["state"]
            .as_str()
            .is_some_and(|s| s != "unknown")
        {
            return insights;
        }
        assert!(
            Instant::now() < deadline,
            "no trust derived for {message}: {insights}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn head(api: &Api, message: &str) -> String {
    let raw = api.raw(message).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    text.split("\r\n\r\n").next().unwrap_or_default().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn opendkim_results_decide_one_click_coverage() {
    require_it!();
    if !profile("aliases") {
        eprintln!("skipped: run with IT_PROFILES=aliases");
        return;
    }
    let holtmail = Holtmail::start(Options {
        extra_config: format!(
            r#"
[smtp]
listen = "127.0.0.1:{LISTEN}"
hostname = "localhost"
allow = ["0.0.0.0/0", "::/0"]
trusted_authserv_ids = ["mx.example.org"]
[[smtp.route]]
recipient = "{RECIPIENT}"
archive = "Newsletters"
folder = "Inbox"
"#
        ),
        ..Options::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.archive_named("Newsletters").await;

    let covered = Letter::new();
    let unsigned = Letter::new();
    let partial = Letter::new();
    let prepended = Letter::new();
    let without_post: Vec<&str> = SIGNED
        .iter()
        .copied()
        .filter(|n| *n != "list-unsubscribe-post")
        .collect();
    let mut attack = format!(
        "List-Unsubscribe: <https://leave.example.org/{}>\r\n",
        prepended.token
    )
    .into_bytes();
    attack.extend(prepended.signed(SIGNED));
    for (letter, data) in [
        (&covered, covered.signed(SIGNED)),
        (&unsigned, unsigned.unsigned()),
        (&partial, partial.signed(&without_post)),
        (&prepended, attack),
    ] {
        let answer = send(MX, &format!("news@{NEWS_DOMAIN}"), RECIPIENT, &data).await;
        assert!(answer.starts_with("250"), "{}: {answer}", letter.token);
    }

    let id = wait_for(&api, &archive, &covered.token).await;
    let fields = head(&api, &id).await;
    assert!(
        fields.contains("mx.example.org") && fields.contains("dkim=pass"),
        "OpenDKIM did not pass the test signature:\n{fields}"
    );
    let insights = derived(&api, &id).await;
    assert_eq!(insights["trust"]["state"], "aligned", "{insights}");
    assert_eq!(insights["trust"]["domain"], NEWS_DOMAIN, "{insights}");
    let unsubscribe = &insights["unsubscribe"];
    assert_eq!(unsubscribe["https"], covered.link.as_str(), "{insights}");
    assert_eq!(
        unsubscribe["one_click"]["available"], true,
        "the signed newsletter is not covered: {insights}"
    );
    assert!(unsubscribe["one_click"]["reason"].is_null(), "{insights}");

    let id = wait_for(&api, &archive, &unsigned.token).await;
    let insights = derived(&api, &id).await;
    assert_eq!(insights["trust"]["state"], "unverified", "{insights}");
    assert_eq!(insights["unsubscribe"]["one_click"]["available"], false);
    assert_eq!(
        insights["unsubscribe"]["one_click"]["reason"], "not_aligned",
        "{insights}"
    );

    let id = wait_for(&api, &archive, &partial.token).await;
    let fields = head(&api, &id).await;
    assert!(fields.contains("dkim=pass"), "{fields}");
    let insights = derived(&api, &id).await;
    assert_eq!(insights["trust"]["state"], "aligned", "{insights}");
    assert_eq!(
        insights["unsubscribe"]["one_click"]["reason"], "not_covered",
        "{insights}"
    );

    let id = wait_for(&api, &archive, &prepended.token).await;
    let fields = head(&api, &id).await;
    assert!(
        fields.contains("dkim=pass"),
        "the prepended field broke the signature:\n{fields}"
    );
    let insights = derived(&api, &id).await;
    assert_eq!(insights["trust"]["state"], "aligned", "{insights}");
    assert_eq!(
        insights["unsubscribe"]["one_click"]["reason"], "multiple_fields",
        "{insights}"
    );
    assert_eq!(insights["unsubscribe"]["one_click"]["available"], false);

    let letters = [&covered, &unsigned, &partial, &prepended];
    LogScanner::new()
        .secrets("a subject token", letters.iter().map(|l| l.token.as_str()))
        .secrets(
            "an unsubscribe link",
            letters.iter().map(|l| l.link.as_str()),
        )
        .secrets("a list id", letters.iter().map(|l| l.list.as_str()))
        .secret("the sender's domain", NEWS_DOMAIN)
        .secret("the recipient", RECIPIENT)
        .check(&holtmail.logs());
}
