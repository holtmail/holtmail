use std::collections::HashMap;

use super::*;
use crate::testing::FakeTokenEndpoint;

fn query(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[test]
fn computes_the_rfc_7636_challenge() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn builds_the_google_authorize_address() {
    let client = Client::thunderbird(Provider::Google);
    let started = client.authorize(Some("ada@example.com")).unwrap();
    assert!(
        started
            .url
            .starts_with("https://accounts.google.com/o/oauth2/auth?")
    );
    let q = query(&started.url);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], client.client_id);
    assert_eq!(q["redirect_uri"], "http://127.0.0.1");
    assert_eq!(q["scope"], "https://mail.google.com/");
    assert_eq!(q["state"], started.state);
    assert_eq!(q["code_challenge"], challenge(&started.verifier));
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["login_hint"], "ada@example.com");
    assert_eq!(q["access_type"], "offline");
    assert_eq!(q["prompt"], "consent");
    assert!(
        !started.url.contains(&started.verifier),
        "the verifier leaked"
    );
    assert!(!started.url.contains("client_secret"));
    assert!(client.is_thunderbird());
}

#[test]
fn builds_the_microsoft_authorize_address() {
    let client = Client::thunderbird(Provider::Microsoft);
    let started = client.authorize(None).unwrap();
    let q = query(&started.url);
    assert!(
        started
            .url
            .starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?")
    );
    assert_eq!(
        q["scope"],
        "https://outlook.office.com/IMAP.AccessAsUser.All offline_access"
    );
    assert!(!q.contains_key("login_hint"));
    assert!(!q.contains_key("access_type"));
    assert_eq!(client.client_secret, None);
}

#[tokio::test]
async fn asks_for_graph_scopes_for_graph_access() {
    let imap = Client::thunderbird(Provider::Microsoft);
    let graph = imap.for_access(Access::Graph).unwrap();
    let q = query(&graph.authorize(Some("ada@example.com")).unwrap().url);
    assert_eq!(
        q["scope"],
        "https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/User.Read offline_access"
    );
    assert_eq!(graph.client_id, imap.client_id);
    assert_eq!(graph.redirect_url, imap.redirect_url);
    assert_eq!(
        graph.for_access(Access::Imap).unwrap().scope,
        imap.scope,
        "switching back lost the IMAP scope"
    );
    assert!(
        Client::thunderbird(Provider::Google)
            .for_access(Access::Graph)
            .is_none()
    );
    assert_eq!(Access::parse(Access::Graph.as_str()), Some(Access::Graph));
    assert_eq!(Access::parse("pop3"), None);

    let endpoint = FakeTokenEndpoint::default();
    endpoint.answer(200, serde_json::json!({ "access_token": "a1" }));
    graph.refresh(&endpoint, "r1").await.unwrap();
    assert_eq!(
        endpoint.last("scope").as_deref(),
        Some(graph.scope.as_str())
    );
}

#[test]
fn every_sign_in_gets_its_own_state_and_verifier() {
    let client = Client::thunderbird(Provider::Google);
    let a = client.authorize(None).unwrap();
    let b = client.authorize(None).unwrap();
    assert_ne!(a.state, b.state);
    assert_ne!(a.verifier, b.verifier);
    assert_eq!(a.verifier.len(), 43);
}

#[test]
fn own_clients_keep_the_provider_endpoints() {
    let client = Client::own(
        Provider::Microsoft,
        "own-id",
        None,
        "https://mail.example.com/api/oauth/callback",
    );
    assert!(!client.is_thunderbird());
    assert_eq!(
        client.token_url,
        "https://login.microsoftonline.com/common/oauth2/v2.0/token"
    );
    let debug = format!(
        "{:?}",
        Client::own(Provider::Google, "id", Some("hidden".into()), "x")
    );
    assert!(!debug.contains("hidden"));
}

#[test]
fn knows_providers_by_address() {
    assert_eq!(
        Provider::for_address("ada@gmail.com"),
        Some(Provider::Google)
    );
    assert_eq!(
        Provider::for_address("Ada@Hotmail.com "),
        Some(Provider::Microsoft)
    );
    assert_eq!(Provider::for_address("ada@example.com"), None);
    assert_eq!(Provider::for_address("no address"), None);
    assert_eq!(Provider::Microsoft.imap(), ("outlook.office365.com", 993));
    for provider in Provider::ALL {
        assert_eq!(Provider::parse(provider.as_str()), Some(provider));
    }
}

#[test]
fn reads_pasted_addresses() {
    let answer = read_redirect(
        "  http://127.0.0.1/?state=s1&code=4%2F0Ab-c&scope=https%3A%2F%2Fmail.google.com%2F \n",
    )
    .unwrap();
    assert_eq!(answer.code, "4/0Ab-c");
    assert_eq!(answer.state, "s1");
    for pasted in [
        "\"http://localhost/?code=c&state=s\"",
        "<https://localhost?code=c&state=s>",
        "?code=c&state=s",
        "code=c&state=s",
        "127.0.0.1/?code=c&state=s",
        "http://127.0.0.1/#code=c&state=s",
    ] {
        let answer = read_redirect(pasted).unwrap_or_else(|e| panic!("{pasted}: {e}"));
        assert_eq!((answer.code.as_str(), answer.state.as_str()), ("c", "s"));
    }
}

#[test]
fn reports_refusals_and_bad_pastes() {
    assert!(matches!(
        read_redirect("http://127.0.0.1/?error=access_denied&state=s"),
        Err(OAuthError::Denied(_))
    ));
    assert_eq!(
        read_redirect("http://localhost/?error=invalid_scope&error_description=Bad+scope&state=s"),
        Err(OAuthError::Denied("Bad scope".into()))
    );
    for pasted in [
        "",
        "hello",
        "http://127.0.0.1/",
        "http://127.0.0.1/?state=s",
        "http://127.0.0.1/?code=c",
    ] {
        assert!(
            matches!(read_redirect(pasted), Err(OAuthError::Redirect(_))),
            "{pasted:?}"
        );
    }
}

#[tokio::test]
async fn exchanges_a_code_with_the_verifier() {
    let endpoint = FakeTokenEndpoint::default();
    endpoint.tokens("access-1", Some("refresh-1"), 3599);
    let client = Client::thunderbird(Provider::Google);
    let tokens = client
        .exchange(&endpoint, "code-1", "verifier-1")
        .await
        .unwrap();
    assert_eq!(tokens.access_token, "access-1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-1"));
    assert_eq!(tokens.expires_in, Duration::from_secs(3599));
    let (url, _) = endpoint.requests().pop().unwrap();
    assert_eq!(url, "https://www.googleapis.com/oauth2/v3/token");
    assert_eq!(
        endpoint.last("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(endpoint.last("code").as_deref(), Some("code-1"));
    assert_eq!(
        endpoint.last("code_verifier").as_deref(),
        Some("verifier-1")
    );
    assert_eq!(
        endpoint.last("redirect_uri").as_deref(),
        Some("http://127.0.0.1")
    );
    assert!(endpoint.last("client_secret").is_some());
    let debug = format!("{tokens:?}");
    assert!(!debug.contains("access-1") && !debug.contains("refresh-1"));
}

#[tokio::test]
async fn refreshes_and_keeps_new_refresh_tokens() {
    let endpoint = FakeTokenEndpoint::default();
    let client = Client::thunderbird(Provider::Microsoft);
    endpoint.answer(
        200,
        serde_json::json!({ "access_token": "a2", "refresh_token": "r2", "expires_in": "3600" }),
    );
    let tokens = client.refresh(&endpoint, "r1").await.unwrap();
    assert_eq!(tokens.refresh_token.as_deref(), Some("r2"));
    assert_eq!(tokens.expires_in, Duration::from_secs(3600));
    assert_eq!(endpoint.last("refresh_token").as_deref(), Some("r1"));
    assert_eq!(
        endpoint.last("scope").as_deref(),
        Some(client.scope.as_str())
    );
    assert_eq!(endpoint.last("client_secret"), None);

    endpoint.answer(200, serde_json::json!({ "access_token": "a3" }));
    let tokens = client.refresh(&endpoint, "r2").await.unwrap();
    assert_eq!(tokens.refresh_token, None);
    assert_eq!(tokens.expires_in, DEFAULT_LIFETIME);
}

#[tokio::test]
async fn tells_revoked_grants_from_other_failures() {
    let endpoint = FakeTokenEndpoint::default();
    let client = Client::thunderbird(Provider::Google);
    endpoint.error("invalid_grant");
    let revoked = client.refresh(&endpoint, "r").await.unwrap_err();
    assert!(matches!(revoked, OAuthError::Revoked(_)));
    assert!(!revoked.is_transient());

    endpoint.error("invalid_client");
    assert!(matches!(
        client.refresh(&endpoint, "r").await,
        Err(OAuthError::Rejected(_))
    ));
    endpoint.answer(503, serde_json::json!("busy"));
    let busy = client.refresh(&endpoint, "r").await.unwrap_err();
    assert!(matches!(busy, OAuthError::Protocol(_)) && busy.is_transient());
    endpoint.answer(200, serde_json::json!({ "token_type": "Bearer" }));
    assert!(matches!(
        client.refresh(&endpoint, "r").await,
        Err(OAuthError::Protocol(_))
    ));
    endpoint.unreachable();
    assert!(matches!(
        client.refresh(&endpoint, "r").await,
        Err(OAuthError::Connection(_))
    ));
}
