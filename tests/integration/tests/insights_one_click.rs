//! One-click unsubscribe from a container (`IT_PROFILES=docker`): Holtmail runs from
//! `holtmail:it-insights` (the release Dockerfile with `FEATURES=insights-test-data`) on the
//! internal networks of the stack's HTTPS endpoint, trusts the stack's CA and allows only the
//! endpoint's subnet through `[insights] one_click_test_networks`. Newsletters are imported with a
//! trusted `Authentication-Results` and a `DKIM-Signature` that covers both unsubscribe fields.
//! The POST reaches nginx once as `multipart/form-data` with `List-Unsubscribe=One-Click` and no
//! cookie, authorization, referrer or origin; a second POST for the same list within 24 h sends
//! nothing; a 302 is recorded as `redirected` and not followed; a 500 is `http_error` and blocks a
//! retry for a minute; the same server under a private address outside the allowed subnet is
//! `refused` without a request; `[insights] one_click = false` gives `disabled`. The logs carry no
//! URL, host, sender domain or list id.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD};
use holtmail_integration::docker::{Container, INSIGHTS_IMAGE, Volume, image_present};
use holtmail_integration::unsubscribe::{
    ALLOWED_NETWORK, ALLOWED_SUBNET, ENDPOINT_NAME, OUTSIDE_NAME, UNLISTED_NETWORK, requests,
    wait_requests,
};
use holtmail_integration::{Api, LogScanner, profile, require_it, state_dir, unique};
use serde_json::{Value, json};

const SENDER_DOMAIN: &str = "news.example.com";
const AUTHSERV: &str = "mx.example.org";

struct Letter {
    token: String,
    path: String,
    url: String,
    list: String,
}

impl Letter {
    fn new(host: &str, endpoint: &str) -> Self {
        let token = unique("leave");
        Self::on_list(host, endpoint, &format!("{token}.{SENDER_DOMAIN}"), token)
    }

    fn twin(&self) -> Self {
        let token = unique("leave");
        let endpoint = self
            .path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default();
        Self::on_list(ENDPOINT_NAME, endpoint, &self.list, token)
    }

    fn on_list(host: &str, endpoint: &str, list: &str, token: String) -> Self {
        let path = format!("/{endpoint}/{token}");
        Self {
            url: format!("https://{host}{path}"),
            path,
            list: list.to_owned(),
            token,
        }
    }

    fn eml(&self) -> String {
        format!(
            "Authentication-Results: {AUTHSERV}; dkim=pass header.d={SENDER_DOMAIN} header.s=it2026 header.b=T25lQ2xp\r\n\
             DKIM-Signature: v=1; a=rsa-sha256; c=relaxed/relaxed; d={SENDER_DOMAIN}; s=it2026;\r\n \
             h=from:to:subject:date:message-id:list-id:list-unsubscribe:list-unsubscribe-post;\r\n \
             bh=SW52ZW50ZWQgYm9keSBoYXNo; b=T25lQ2xpY2tJbnZlbnRlZFNpZ25hdHVyZQ==\r\n\
             From: Example News <news@{SENDER_DOMAIN}>\r\n\
             To: <reader@example.org>\r\n\
             Subject: Example News {token}\r\n\
             Date: Sat, 07 Nov 2026 09:00:00 +0000\r\n\
             Message-ID: <{token}@{SENDER_DOMAIN}>\r\n\
             List-Id: Example News <{list}>\r\n\
             List-Unsubscribe: <{url}>\r\n\
             List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             This week: the garden club and the bake sale.\r\n",
            token = self.token,
            list = self.list,
            url = self.url,
        )
    }
}

fn mbox(letters: &[&Letter]) -> Vec<u8> {
    let mut out = String::new();
    for letter in letters {
        out.push_str(&format!(
            "From news@{SENDER_DOMAIN} Sat Nov  7 09:00:00 2026\n"
        ));
        out.push_str(&letter.eml().replace("\r\n", "\n"));
    }
    out.into_bytes()
}

fn config(path: &PathBuf, one_click: bool) {
    std::fs::write(
        path,
        format!(
            "tls_ca_file = \"/certs/ca.crt\"\n\n[insights]\nimport_authserv_ids = [\"{AUTHSERV}\"]\none_click = {one_click}\none_click_test_networks = [\"{ALLOWED_SUBNET}\"]\n"
        ),
    )
    .expect("config written");
}

async fn owner(container: &Container) -> Api {
    Api::sign_in_url(&container.url, OWNER_EMAIL, OWNER_PASSWORD).await
}

async fn message_id(api: &Api, archive: &str, letter: &Letter) -> String {
    api.messages(archive)
        .await
        .into_iter()
        .find(|m| {
            m["subject"]
                .as_str()
                .is_some_and(|s| s.contains(&letter.token))
        })
        .and_then(|m| m["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("{} was not imported", letter.token))
}

/// The message's insights once stage B has derived its trust.
async fn derived(api: &Api, message: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let insights = api.message(message).await["insights"].clone();
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

async fn unsubscribe(api: &Api, message: &str) -> (u16, Value) {
    let reply = api
        .post(
            &format!("/api/messages/{message}/unsubscribe"),
            json!({ "confirm": true }),
        )
        .await;
    (reply.status.as_u16(), reply.body)
}

#[tokio::test(flavor = "multi_thread")]
async fn one_click_posts_once_to_allowed_networks_only() {
    require_it!();
    if !profile("docker") {
        eprintln!("skipped: needs IT_PROFILES=docker");
        return;
    }
    assert!(
        image_present(INSIGHTS_IMAGE),
        "it.sh builds {INSIGHTS_IMAGE}"
    );
    let run = unique("one-click");
    let dir = state_dir().join("one-click").join(&run);
    std::fs::create_dir_all(&dir).expect("config directory");
    let config_file = dir.join("holtmail.toml");
    config(&config_file, true);
    let certs = format!("{}:/certs:ro", state_dir().display());
    let inside = format!("/certs/one-click/{run}/holtmail.toml");
    let volume = Volume::new("holtmail-it-one-click");
    let mut container = Container::run(
        INSIGHTS_IMAGE,
        &volume,
        &[("HOLTMAIL_SYNC_INTERVAL", "0")],
        &["-v", &certs],
        &["--config", &inside, "serve"],
    )
    .await;
    container.connect(ALLOWED_NETWORK);
    container.connect(UNLISTED_NETWORK);
    let added = container.exec(
        &[
            "user",
            "add",
            "--email",
            OWNER_EMAIL,
            "--name",
            "Owner Example",
        ],
        &format!("{OWNER_PASSWORD}\n"),
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let api = owner(&container).await;
    let archive = api.create_archive("Newsletters").await;

    let accepted = Letter::new(ENDPOINT_NAME, "accept");
    let twin = accepted.twin();
    let moved = Letter::new(ENDPOINT_NAME, "moved");
    let broken = Letter::new(ENDPOINT_NAME, "broken");
    let outside = Letter::new(OUTSIDE_NAME, "accept");
    let later = Letter::new(ENDPOINT_NAME, "accept");
    let letters = [&accepted, &twin, &moved, &broken, &outside, &later];
    let (_, task) = api
        .import(&archive, "newsletters.mbox", mbox(&letters))
        .await;
    assert_eq!(task["state"], "done", "{task}");

    let mut ids = Vec::new();
    for letter in letters {
        let id = message_id(&api, &archive, letter).await;
        let insights = derived(&api, &id).await;
        assert_eq!(insights["trust"]["state"], "aligned", "{insights}");
        assert_eq!(
            insights["unsubscribe"]["one_click"]["available"], true,
            "{} is not covered: {insights}",
            letter.token
        );
        ids.push(id);
    }
    let [
        accepted_id,
        twin_id,
        moved_id,
        broken_id,
        outside_id,
        later_id,
    ] = <[String; 6]>::try_from(ids).expect("six messages");

    let started = Instant::now();
    let (status, body) = unsubscribe(&api, &accepted_id).await;
    let took = started.elapsed();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["repeated"], false, "{body}");
    assert_eq!(body["request"]["outcome"], "accepted", "{body}");
    assert_eq!(body["request"]["status"], 200, "{body}");
    assert!(body["request"]["requested_at"].is_i64(), "{body}");
    let seen = wait_requests(&accepted.path, 1).await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    let request = &seen[0];
    assert_eq!(request.method, "POST", "{request:?}");
    assert!(
        request
            .content_type
            .starts_with("multipart/form-data; boundary="),
        "{request:?}"
    );
    let boundary = request
        .content_type
        .split_once("boundary=")
        .map(|(_, b)| b.trim_matches('"').to_owned())
        .expect("a boundary");
    assert_eq!(
        request.body,
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"List-Unsubscribe\"\r\n\r\nOne-Click\r\n--{boundary}--\r\n"
        ),
        "{request:?}"
    );
    assert!(request.cookie.is_empty(), "{request:?}");
    assert!(request.authorization.is_empty(), "{request:?}");
    assert!(request.referer.is_empty(), "{request:?}");
    assert!(request.origin.is_empty(), "{request:?}");

    let (status, body) = unsubscribe(&api, &accepted_id).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["repeated"], true, "{body}");
    assert_eq!(body["request"]["outcome"], "accepted", "{body}");
    let (status, body) = unsubscribe(&api, &twin_id).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["repeated"], true,
        "the same list a second time: {body}"
    );
    let insights = api.message(&twin_id).await["insights"].clone();
    assert_eq!(
        insights["unsubscribe"]["one_click"]["reason"], "recently_requested",
        "{insights}"
    );
    assert_eq!(
        insights["unsubscribe"]["last_request"]["outcome"], "accepted",
        "{insights}"
    );

    let (status, body) = unsubscribe(&api, &moved_id).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["request"]["outcome"], "redirected", "{body}");
    assert_eq!(body["request"]["status"], 302, "{body}");
    assert_eq!(wait_requests(&moved.path, 1).await.len(), 1);

    let (status, body) = unsubscribe(&api, &broken_id).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["request"]["outcome"], "http_error", "{body}");
    assert_eq!(body["request"]["status"], 500, "{body}");
    let (status, body) = unsubscribe(&api, &broken_id).await;
    assert_eq!(status, 429, "a retry within a minute of a failure: {body}");
    assert_eq!(wait_requests(&broken.path, 1).await.len(), 1);

    let (status, body) = unsubscribe(&api, &outside_id).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["request"]["outcome"], "refused",
        "a private address outside the allowed subnet: {body}"
    );
    assert!(body["request"]["status"].is_null(), "{body}");

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(requests(&accepted.path).len(), 1, "a second request");
    assert!(requests(&twin.path).is_empty(), "the twin was requested");
    assert!(
        requests(&format!("/landing{}", moved.path)).is_empty(),
        "the redirect was followed"
    );
    assert_eq!(requests(&broken.path).len(), 1, "the retry was sent");
    assert!(
        requests(&outside.path).is_empty(),
        "the unlisted address was contacted"
    );

    container.stop();
    config(&config_file, false);
    container.start_again().await;
    let api = owner(&container).await;
    let insights = derived(&api, &later_id).await;
    assert_eq!(insights["unsubscribe"]["one_click"]["available"], false);
    assert_eq!(
        insights["unsubscribe"]["one_click"]["reason"], "disabled",
        "{insights}"
    );
    let (status, body) = unsubscribe(&api, &later_id).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["reason"], "disabled", "{body}");
    assert!(requests(&later.path).is_empty(), "sent while switched off");

    let logs = container.logs();
    assert!(
        logs.contains("one-click unsubscribe sent"),
        "no outcome logged:\n{logs}"
    );
    LogScanner::new()
        .secrets(
            "an unsubscribe URL path",
            letters.iter().map(|l| l.path.as_str()),
        )
        .secrets("a list id", letters.iter().map(|l| l.list.as_str()))
        .secret("the endpoint's name", ENDPOINT_NAME)
        .secret("the unlisted name", OUTSIDE_NAME)
        .secret("the sender's domain", SENDER_DOMAIN)
        .check(&logs);
    eprintln!("one-click POST answered in {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
