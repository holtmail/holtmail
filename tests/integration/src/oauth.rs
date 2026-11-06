//! The stack's OAuth provider, `mock-oauth2-server`: an own client for each Holtmail instance,
//! the browser's part of a sign-in, and revoking refresh tokens.
//!
//! The provider grants every client at once (no login page) and puts the client id into the
//! tokens' `sub`. Dovecot checks XOAUTH2 tokens through the provider's introspection and signs
//! in the user named by `sub`, so a client id that is an email address is also that mailbox.

use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, LOCATION};

const FORM: &str = "application/x-www-form-urlencoded";

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// The provider's HTTPS port on this machine (and inside Dovecot's network namespace, which it
/// shares so both reach it as `localhost`).
pub const OAUTH_PORT: u16 = 47443;
/// The client secret every test client uses.
pub const CLIENT_SECRET: &str = "it-oauth-client-secret";

/// The issuer Holtmail and Dovecot use. Its access tokens live 60 seconds, less than
/// Holtmail's two-minute margin, so every use of a token refreshes it first.
pub fn issuer() -> String {
    format!("https://localhost:{OAUTH_PORT}/default")
}

/// A fresh client id that is also a fresh Dovecot user.
pub fn client_id(prefix: &str) -> String {
    format!("{}@example.com", crate::unique(prefix))
}

/// The config of an own `[oauth.<provider>]` client at the stack's provider, whose new accounts
/// connect to Dovecot's IMAPS port. `redirect_url` overrides the default callback address.
pub fn config(provider: &str, client_id: &str, redirect_url: Option<&str>) -> String {
    let mut config = format!(
        "\n[oauth.{provider}]\nclient_id = {client_id:?}\nclient_secret = {CLIENT_SECRET:?}\nauthorize_url = \"{base}/authorize\"\ntoken_url = \"{base}/token\"\nimap_host = \"localhost\"\nimap_port = {port}\n",
        base = issuer(),
        port = crate::services::DOVECOT_IMAPS,
    );
    if let Some(url) = redirect_url {
        config.push_str(&format!("redirect_url = {url:?}\n"));
    }
    config
}

fn client() -> reqwest::Client {
    let pem = std::fs::read(crate::ca_file()).expect("the stack's CA; run `mise run it`");
    reqwest::Client::builder()
        .tls_certs_merge(reqwest::Certificate::from_pem_bundle(&pem).expect("a PEM CA"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .expect("client")
}

/// Waits until the provider answers its discovery document.
pub async fn wait_ready() {
    let url = format!("{}/.well-known/openid-configuration", issuer());
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(response) = client().get(&url).send().await
            && response.status().is_success()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the OAuth provider does not answer"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Plays the browser: opens the authorize address Holtmail gave, signs in on the provider's
/// page when it shows one (it does for `prompt=consent`, which Google's requests carry) as the
/// `login_hint`, and returns where the provider sends the browser back (the redirect address
/// with `code` and `state`).
///
/// The provider is an OpenID Connect server and refuses authorize requests whose scope lacks
/// `openid`, which Google's and Microsoft's mail scopes do; `openid` is added here, only for
/// this provider.
pub async fn authorize(authorize_url: &str) -> String {
    let mut url = reqwest::Url::parse(authorize_url).expect("an authorize URL");
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let v = if k == "scope" {
                format!("openid {v}")
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    url.query_pairs_mut().clear().extend_pairs(pairs);
    let client = client();
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .expect("the provider answers");
    if response.status().is_success() {
        let user = query_value(authorize_url, "login_hint")
            .or_else(|| query_value(authorize_url, "client_id"))
            .expect("a user to sign in as");
        response = client
            .post(url)
            .header(CONTENT_TYPE, FORM)
            .body(form(&[("username", &user)]))
            .send()
            .await
            .expect("the provider answers");
    }
    assert!(
        response.status().is_redirection(),
        "the provider did not redirect: {}",
        response.status()
    );
    response
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("a Location")
        .to_owned()
}

/// Opens Holtmail's callback address like a browser without following the redirect back into
/// the web app, and returns that redirect's `Location`.
pub async fn callback(redirect: &str) -> String {
    let response = client()
        .get(redirect)
        .send()
        .await
        .expect("the instance answers");
    assert_eq!(
        response.status().as_u16(),
        303,
        "callback answered {}",
        response.status()
    );
    response
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("a Location")
        .to_owned()
}

/// The value of `key` in the query of `url`.
pub fn query_value(url: &str, key: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url)
        .or_else(|_| reqwest::Url::parse(&format!("http://localhost{url}")))
        .ok()?;
    parsed
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Revokes `refresh_token`, as removing an app's access in the account would.
pub async fn revoke(client_id: &str, refresh_token: &str) {
    let response = client()
        .post(format!("{}/revoke", issuer()))
        .basic_auth(client_id, Some(CLIENT_SECRET))
        .header(CONTENT_TYPE, FORM)
        .body(form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
        ]))
        .send()
        .await
        .expect("the provider answers");
    assert!(response.status().is_success(), "{}", response.status());
}

/// Whether the provider still accepts `refresh_token` from `client_id`: asks it for a token. A
/// refresh the provider accepts uses the token up (it rotates them), so ask only for tokens
/// expected to be refused.
pub async fn refresh_accepted(client_id: &str, refresh_token: &str) -> bool {
    let response = client()
        .post(format!("{}/token", issuer()))
        .header(CONTENT_TYPE, FORM)
        .body(form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", CLIENT_SECRET),
        ]))
        .send()
        .await
        .expect("the provider answers");
    response.status().is_success()
}
