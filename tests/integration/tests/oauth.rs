//! Own-client OAuth against mock-oauth2-server, with XOAUTH2 into Dovecot: the authorize
//! address, the callback finishing sign-in, the paste flow, refreshing before syncs with rotated
//! refresh tokens across a restart, a revoked refresh token stopping syncs until signed in
//! again, the server of an OAuth account fixed, and a token the mail server refuses.

use std::time::{Duration, Instant};

use holtmail_integration::api::id;
use holtmail_integration::data::Message;
use holtmail_integration::oauth::{self, CLIENT_SECRET};
use holtmail_integration::services::{DOVECOT, DOVECOT_IMAP, DOVECOT_IMAPS, DOVECOT_PASSWORD};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it};
use serde_json::{Value, json};

struct Instance {
    holtmail: Holtmail,
    api: Api,
    archive: String,
    client_id: String,
}

async fn instance(provider: &str, redirect_url: Option<&str>, interval: Option<u64>) -> Instance {
    oauth::wait_ready().await;
    let client_id = oauth::client_id("oauth");
    let holtmail = Holtmail::start(Options {
        extra_config: oauth::config(provider, &client_id, redirect_url),
        sync_interval_minutes: interval,
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("OAuth").await;
    Instance {
        holtmail,
        api,
        archive,
        client_id,
    }
}

impl Instance {
    async fn start(&self, provider: &str, target: Value) -> Value {
        let mut request =
            json!({ "provider": provider, "email": self.client_id, "return_to": "/welcome" });
        for (key, value) in target.as_object().expect("an object") {
            request[key] = value.clone();
        }
        self.api.post("/api/oauth/start", request).await.ok()
    }

    async fn seed(&self, count: usize) -> Vec<Message> {
        let mut seeder = DOVECOT.seeder(&self.client_id).await;
        let messages: Vec<Message> = (0..count).map(|_| Message::new(&self.client_id)).collect();
        for message in &messages {
            seeder.append("INBOX", &[], None, &message.bytes()).await;
        }
        seeder.logout().await;
        messages
    }

    async fn subjects(&self) -> Vec<String> {
        let mut subjects: Vec<String> = self
            .api
            .messages(&self.archive)
            .await
            .iter()
            .map(|m| m["subject"].as_str().unwrap_or_default().to_owned())
            .collect();
        subjects.sort();
        subjects
    }

    fn scanner(
        &self,
        codes: &[String],
        refresh_tokens: &[String],
        messages: &[Message],
    ) -> LogScanner {
        LogScanner::new()
            .secret("the client secret", CLIENT_SECRET)
            .secret("the user", &self.client_id)
            .secret("a JWT (access or ID token)", "eyJ")
            .secret("the IMAP password", DOVECOT_PASSWORD)
            .secrets("a code", codes.iter().map(String::as_str))
            .secrets("a refresh token", refresh_tokens.iter().map(String::as_str))
            .secrets("a subject", messages.iter().map(|m| m.subject.as_str()))
            .secrets("a sender", messages.iter().map(|m| m.from.as_str()))
    }

    async fn sync_tasks(&self, account: &str) -> usize {
        self.api
            .tasks()
            .await
            .iter()
            .filter(|t| t["kind"] == "sync" && t["account_id"] == account)
            .count()
    }
}

fn sorted(messages: &[Message]) -> Vec<String> {
    let mut subjects: Vec<String> = messages.iter().map(|m| m.subject.clone()).collect();
    subjects.sort();
    subjects
}

#[tokio::test(flavor = "multi_thread")]
async fn an_own_client_signs_in_through_the_callback_and_syncs_over_xoauth2() {
    require_it!();
    let it = instance("google", None, None).await;
    let messages = it.seed(2).await;

    let started = it
        .start(
            "google",
            json!({ "archive_id": it.archive, "name": "Mail over OAuth" }),
        )
        .await;
    assert_eq!(started["needs_paste"], false, "{started}");
    assert_eq!(started["thunderbird_client"], false, "{started}");
    let url = started["authorize_url"].as_str().expect("an address");
    assert!(
        url.starts_with(&format!("{}/authorize?", oauth::issuer())),
        "{url}"
    );
    let state = started["state"].as_str().expect("a state");
    let callback = format!("{}/api/oauth/callback", it.holtmail.url());
    for (key, expected) in [
        ("response_type", "code"),
        ("client_id", it.client_id.as_str()),
        ("redirect_uri", callback.as_str()),
        ("scope", "https://mail.google.com/"),
        ("state", state),
        ("code_challenge_method", "S256"),
        ("login_hint", it.client_id.as_str()),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ] {
        assert_eq!(
            oauth::query_value(url, key).as_deref(),
            Some(expected),
            "{key} in {url}"
        );
    }
    assert_eq!(
        oauth::query_value(url, "code_challenge").map(|c| c.len()),
        Some(43),
        "{url}"
    );

    let redirect = oauth::authorize(url).await;
    assert!(redirect.starts_with(&callback), "{redirect}");
    let code = oauth::query_value(&redirect, "code").expect("a code");
    let back = oauth::callback(&redirect).await;
    assert!(back.starts_with("/welcome?oauth=done&account="), "{back}");
    let account = oauth::query_value(&back, "account").expect("an account");

    let replayed = oauth::callback(&redirect).await;
    assert!(replayed.starts_with("/settings?oauth_error="), "{replayed}");

    let listed = it.api.account(&it.archive, &account).await;
    assert_eq!(listed["oauth_provider"], "google", "{listed}");
    assert_eq!(listed["needs_sign_in"], false, "{listed}");
    assert_eq!(listed["name"], "Mail over OAuth", "{listed}");
    assert_eq!(listed["connection"]["host"], "localhost", "{listed}");
    assert_eq!(listed["connection"]["port"], DOVECOT_IMAPS, "{listed}");
    assert_eq!(listed["connection"]["security"], "tls", "{listed}");
    assert_eq!(
        listed["connection"]["username"],
        it.client_id.as_str(),
        "{listed}"
    );

    let synced = it.api.settle(&it.archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    assert_eq!(it.subjects().await, sorted(&messages));

    let more = it.seed(1).await;
    let synced = it.api.sync(&it.archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    let all: Vec<Message> = messages.iter().chain(&more).cloned().collect();
    assert_eq!(it.subjects().await, sorted(&all));

    let refresh = it.holtmail.open_secret(&account).await;
    it.scanner(&[code], &[refresh], &all)
        .check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_paste_flow_signs_in_with_an_address_copied_from_the_browser() {
    require_it!();
    let it = instance("microsoft", Some("http://127.0.0.1"), None).await;
    let messages = it.seed(1).await;

    let started = it
        .start("microsoft", json!({ "archive_id": it.archive }))
        .await;
    assert_eq!(started["needs_paste"], true, "{started}");
    let url = started["authorize_url"].as_str().expect("an address");
    assert_eq!(
        oauth::query_value(url, "scope").as_deref(),
        Some("https://outlook.office.com/IMAP.AccessAsUser.All offline_access")
    );
    assert_eq!(
        oauth::query_value(url, "redirect_uri").as_deref(),
        Some("http://127.0.0.1")
    );
    let pasted = oauth::authorize(url).await;
    assert!(pasted.starts_with("http://127.0.0.1"), "{pasted}");
    let code = oauth::query_value(&pasted, "code").expect("a code");

    let refused = it
        .api
        .post(
            "/api/oauth/finish",
            json!({ "pasted_url": "http://127.0.0.1/?error=access_denied&state=nope" }),
        )
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    let refused = it
        .api
        .post(
            "/api/oauth/finish",
            json!({ "pasted_url": "not an address" }),
        )
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);

    let finished = it
        .api
        .post("/api/oauth/finish", json!({ "pasted_url": pasted }))
        .await
        .ok();
    let account = id(&finished);
    assert_eq!(finished["oauth_provider"], "microsoft", "{finished}");
    let again = it
        .api
        .post("/api/oauth/finish", json!({ "pasted_url": pasted }))
        .await;
    assert_eq!(
        again.status, 400,
        "a pasted answer was used twice: {}",
        again.body
    );

    let synced = it.api.settle(&it.archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    assert_eq!(it.subjects().await, sorted(&messages));
    assert_eq!(it.api.accounts(&it.archive).await.len(), 1);

    let refresh = it.holtmail.open_secret(&account).await;
    it.scanner(&[code], &[refresh], &messages)
        .check(&it.holtmail.logs());
}

async fn sign_in(it: &Instance, provider: &str, target: Value) -> (String, String) {
    let started = it.start(provider, target).await;
    let redirect = oauth::authorize(started["authorize_url"].as_str().expect("an address")).await;
    let code = oauth::query_value(&redirect, "code").expect("a code");
    let back = oauth::callback(&redirect).await;
    assert!(back.contains("oauth=done"), "{back}");
    (
        oauth::query_value(&back, "account").expect("an account"),
        code,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn refreshes_before_each_sync_and_keeps_rotated_tokens_across_a_restart() {
    require_it!();
    let mut it = instance("google", None, None).await;
    let messages = it.seed(1).await;
    let (account, code) = sign_in(&it, "google", json!({ "archive_id": it.archive })).await;
    let synced = it.api.settle(&it.archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");

    let mut tokens = vec![it.holtmail.open_secret(&account).await];
    for restart in [false, true, false] {
        if restart {
            it.holtmail.restart().await;
            it.api = Api::sign_in(
                &it.holtmail,
                holtmail_integration::api::OWNER_EMAIL,
                holtmail_integration::api::OWNER_PASSWORD,
            )
            .await;
        }
        let synced = it.api.sync(&it.archive, &account).await;
        assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
        let now = it.holtmail.open_secret(&account).await;
        assert!(
            !tokens.contains(&now),
            "the sync did not refresh (restart: {restart})"
        );
        tokens.push(now);
    }
    assert_eq!(it.subjects().await, sorted(&messages));
    for old in &tokens[..tokens.len() - 1] {
        assert!(
            !oauth::refresh_accepted(&it.client_id, old).await,
            "a rotated refresh token still works"
        );
    }
    it.scanner(&[code], &tokens, &messages)
        .check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_refresh_token_stops_syncs_until_signed_in_again() {
    require_it!();
    let it = instance("google", None, Some(1)).await;
    let messages = it.seed(1).await;
    let (account, first_code) = sign_in(&it, "google", json!({ "archive_id": it.archive })).await;
    let synced = it.api.settle(&it.archive, &account).await;
    assert_eq!(synced["status"]["error"], Value::Null, "{synced}");

    let revoked = it.holtmail.open_secret(&account).await;
    oauth::revoke(&it.client_id, &revoked).await;
    let failed = it.api.sync(&it.archive, &account).await;
    let error = failed["status"]["error"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(error.contains("Sign in again"), "{failed}");
    let listed = it.api.account(&it.archive, &account).await;
    assert_eq!(listed["needs_sign_in"], true, "{listed}");
    assert_eq!(listed["status"]["next_sync_at"], Value::Null, "{listed}");

    let refused = it
        .api
        .post(&format!("/api/accounts/{account}/sync"), json!({}))
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    let tasks = it.sync_tasks(&account).await;
    tokio::time::sleep(Duration::from_secs(75)).await;
    assert_eq!(
        it.sync_tasks(&account).await,
        tasks,
        "the scheduler synced an account that needs sign-in"
    );
    assert_eq!(it.holtmail.open_secret(&account).await, revoked);

    let more = it.seed(1).await;
    let (again, second_code) = sign_in(&it, "google", json!({ "account_id": account })).await;
    assert_eq!(again, account);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let listed = it.api.account(&it.archive, &account).await;
        if listed["needs_sign_in"] == false
            && listed["status"]["running"] == false
            && it.subjects().await.len() == 2
        {
            assert_eq!(listed["status"]["error"], Value::Null, "{listed}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no sync after signing in again: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let all: Vec<Message> = messages.iter().chain(&more).cloned().collect();
    assert_eq!(it.subjects().await, sorted(&all));
    assert_eq!(it.api.accounts(&it.archive).await.len(), 1);
    let current = it.holtmail.open_secret(&account).await;
    assert_ne!(current, revoked);
    it.scanner(&[first_code, second_code], &[revoked, current], &all)
        .check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oauth_account_keeps_its_server_port_and_security() {
    require_it!();
    let it = instance("google", None, None).await;
    let (account, code) = sign_in(&it, "google", json!({ "archive_id": it.archive })).await;
    it.api.settle(&it.archive, &account).await;

    for (host, port, security) in [
        ("127.0.0.1", DOVECOT_IMAP, "plain"),
        ("localhost", DOVECOT_IMAP, "starttls"),
        ("localhost", DOVECOT_IMAPS, "starttls"),
        ("dovecot.example.com", DOVECOT_IMAPS, "tls"),
    ] {
        let refused = it
            .api
            .update_account(
                &account,
                json!({ "connection": { "host": host, "port": port, "security": security, "username": it.client_id } }),
            )
            .await;
        assert_eq!(
            refused.status, 400,
            "{host}:{port} {security}: {}",
            refused.body
        );
        let reason = refused.body["error"].as_str().unwrap_or_default();
        assert!(reason.contains("fixed"), "{reason}");
    }
    let refused = it
        .api
        .update_account(&account, json!({ "password": DOVECOT_PASSWORD }))
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(
        refused.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no password"),
        "{}",
        refused.body
    );
    let unchanged = it
        .api
        .update_account(
            &account,
            json!({ "connection": { "host": "LOCALHOST", "port": DOVECOT_IMAPS, "security": "tls", "username": it.client_id, "exclude": ["Junk"] } }),
        )
        .await
        .ok();
    assert_eq!(
        unchanged["connection"]["exclude"],
        json!(["Junk"]),
        "{unchanged}"
    );
    let listed = it.api.account(&it.archive, &account).await;
    assert_eq!(listed["connection"]["port"], DOVECOT_IMAPS, "{listed}");
    assert_eq!(listed["connection"]["security"], "tls", "{listed}");

    let refresh = it.holtmail.open_secret(&account).await;
    it.scanner(&[code], &[refresh], &[])
        .check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_the_mail_server_refuses_stores_nothing() {
    require_it!();
    let it = instance("google", None, None).await;
    let other = oauth::client_id("oauth-other");
    let started = it
        .api
        .post(
            "/api/oauth/start",
            json!({ "provider": "google", "email": other, "archive_id": it.archive }),
        )
        .await
        .ok();
    let redirect = oauth::authorize(started["authorize_url"].as_str().expect("an address")).await;
    let code = oauth::query_value(&redirect, "code").expect("a code");
    let back = oauth::callback(&redirect).await;
    let reason = oauth::query_value(&back, "oauth_error").unwrap_or_default();
    assert!(back.starts_with("/settings?oauth_error="), "{back}");
    assert!(reason.contains("refused access"), "{reason}");
    assert!(it.api.accounts(&it.archive).await.is_empty());
    it.scanner(&[code], &[], &[])
        .secret("the other user", &other)
        .check(&it.holtmail.logs());
}
