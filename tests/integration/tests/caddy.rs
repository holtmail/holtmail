//! Holtmail behind Caddy with TLS from the stack's CA (`https://localhost:49443` forwarding to
//! the binary on `127.0.0.1:49480`) and `public_url` naming Caddy: the setup link names the
//! public address, setup and sign-in work through the proxy with a `Secure` cookie, writes with
//! the public `Origin` succeed and others are refused, and the sign-in rate limit, which sees
//! Caddy's address for every client, is kept per email.

use holtmail_integration::{Holtmail, LogScanner, Options, require_it};
use reqwest::header::{CONTENT_TYPE, ORIGIN, SET_COOKIE};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const PUBLIC: &str = "https://localhost:49443";
const UPSTREAM_PORT: u16 = 49480;
const OWNER: (&str, &str) = ("caddy-owner@example.com", "it-caddy-owner-1");
const MEMBER: (&str, &str) = ("caddy-member@example.com", "it-caddy-member-1");
const WRONG: &str = "it-caddy-wrong-1";

fn client() -> reqwest::Client {
    let pem = std::fs::read(holtmail_integration::ca_file()).expect("the stack's CA");
    reqwest::Client::builder()
        .tls_certs_merge(reqwest::Certificate::from_pem_bundle(&pem).expect("a PEM CA"))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
}

async fn send(
    client: &reqwest::Client,
    method: Method,
    path: &str,
    origin: Option<&str>,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value, Option<String>) {
    let mut request = client.request(method, format!("{PUBLIC}{path}"));
    if let Some(origin) = origin {
        request = request.header(ORIGIN, origin);
    }
    if let Some(body) = body {
        request = request
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.expect("Caddy answers");
    let status = response.status();
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body, cookie)
}

async fn login(client: &reqwest::Client, email: &str, password: &str, forwarded: &str) -> u16 {
    send(
        client,
        Method::POST,
        "/api/login",
        Some(PUBLIC),
        Some(json!({ "email": email, "password": password })),
        &[("x-forwarded-for", forwarded)],
    )
    .await
    .0
    .as_u16()
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_setup_sign_in_and_writes_through_caddy() {
    require_it!();
    let holtmail = Holtmail::start(Options {
        port: Some(UPSTREAM_PORT),
        extra_config: format!("public_url = \"{PUBLIC}/\"\n"),
        ..Default::default()
    })
    .await;
    let logs = holtmail.logs();
    assert!(
        logs.contains(&format!("Open {PUBLIC}/setup?token=")),
        "the setup link does not name the public address:\n{logs}"
    );
    let token = holtmail.setup_token().expect("a setup token");

    let owner = client();
    let (status, body, _) = send(&owner, Method::GET, "/api/setup", None, None, &[]).await;
    assert_eq!(status, 200, "Caddy does not reach Holtmail: {body}");
    assert_eq!(body, json!({ "needed": true }));
    let (status, body, cookie) = send(
        &owner,
        Method::POST,
        "/api/setup",
        Some(PUBLIC),
        Some(json!({ "token": token, "email": OWNER.0, "name": "Owner Example", "password": OWNER.1 })),
        &[],
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let cookie = cookie.expect("a session cookie");
    assert!(cookie.contains("; Secure"), "{cookie}");

    let (status, body, _) = send(
        &owner,
        Method::POST,
        "/api/archives",
        Some(PUBLIC),
        Some(json!({ "name": "Behind Caddy" })),
        &[],
    )
    .await;
    assert_eq!(status, 201, "a write with the public origin: {body}");
    for origin in [
        "https://attacker.example.org",
        "https://localhost:49444",
        &format!("http://127.0.0.1:{UPSTREAM_PORT}"),
    ] {
        let (status, body, _) = send(
            &owner,
            Method::POST,
            "/api/archives",
            Some(origin),
            Some(json!({ "name": "Refused" })),
            &[],
        )
        .await;
        assert_eq!(status, 403, "a write from {origin}: {body}");
    }
    let (status, body, _) = send(
        &owner,
        Method::POST,
        "/api/archives",
        Some(PUBLIC),
        Some(json!({ "name": "Refused" })),
        &[("sec-fetch-site", "cross-site")],
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let direct = reqwest::Client::new()
        .post(format!("{}/api/login", holtmail.url()))
        .header(ORIGIN, PUBLIC)
        .header(CONTENT_TYPE, "application/json")
        .body(json!({ "email": OWNER.0, "password": OWNER.1 }).to_string())
        .send()
        .await
        .expect("an answer");
    assert_eq!(
        direct.status(),
        403,
        "the public origin was accepted on the upstream address"
    );
    let (_, archives, _) = send(&owner, Method::GET, "/api/archives", None, None, &[]).await;
    let names: Vec<&str> = archives
        .as_array()
        .expect("archives")
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert_eq!(names, vec!["Behind Caddy"]);

    let (status, body, _) = send(
        &owner,
        Method::POST,
        "/api/users",
        Some(PUBLIC),
        Some(json!({ "email": MEMBER.0, "name": "Member Example", "password": MEMBER.1 })),
        &[],
    )
    .await;
    assert_eq!(status, 201, "{body}");

    let attacker = client();
    for attempt in 1..=10 {
        let forwarded = format!("198.51.100.{attempt}");
        assert_eq!(
            login(&attacker, MEMBER.0, WRONG, &forwarded).await,
            401,
            "attempt {attempt}"
        );
    }
    let member = client();
    assert_eq!(
        login(&member, MEMBER.0, MEMBER.1, "203.0.113.7").await,
        429,
        "the member is not limited after ten failures from other forwarded addresses"
    );
    assert_eq!(
        login(&member, OWNER.0, OWNER.1, "198.51.100.1").await,
        200,
        "another email is limited with the shared proxy address"
    );

    let (status, body, _) = send(&owner, Method::GET, "/api/session", None, None, &[]).await;
    assert_eq!(status, 200, "{body}");
    LogScanner::new()
        .secret("the owner's email", OWNER.0)
        .secret("the owner's password", OWNER.1)
        .secret("the member's email", MEMBER.0)
        .secret("the member's password", MEMBER.1)
        .secret("a wrong password", WRONG)
        .check(&holtmail.logs());
}
