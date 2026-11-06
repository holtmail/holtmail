//! Signing in through the real binary: wrong passwords and unknown addresses, the rate limit of
//! ten failures per client address and email, a session past its expiry, sign-out and a password
//! set from the CLI ending sessions, writes refused for a wrong `Origin`, `Sec-Fetch-Site:
//! cross-site` or a missing JSON content type, and the one-time setup link.

use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD};
use holtmail_integration::{Api, Holtmail, LogScanner, require_it};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, COOKIE, ORIGIN, RETRY_AFTER, SET_COOKIE};
use serde_json::{Value, json};

const MEMBER_EMAIL: &str = "signin-member@example.com";
const MEMBER_PASSWORD: &str = "it-signin-member-1";
const WRONG: &str = "it-wrong-password-1";

fn plain() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
}

async fn login(holtmail: &Holtmail, email: &str, password: &str) -> reqwest::Response {
    plain()
        .post(format!("{}/api/login", holtmail.url()))
        .header(ORIGIN, holtmail.url())
        .header(CONTENT_TYPE, "application/json")
        .body(json!({ "email": email, "password": password }).to_string())
        .send()
        .await
        .expect("an answer")
}

fn session_cookie(response: &reqwest::Response) -> String {
    let set = response
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("a session cookie")
        .to_owned();
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Lax"),
        "{set}"
    );
    set.split(';').next().expect("name=value").to_owned()
}

async fn status_with(holtmail: &Holtmail, cookie: &str) -> StatusCode {
    plain()
        .get(format!("{}/api/session", holtmail.url()))
        .header(COOKIE, cookie)
        .send()
        .await
        .expect("an answer")
        .status()
}

async fn instance() -> (Holtmail, Api, String) {
    let holtmail = Holtmail::start_default().await;
    let owner = Api::setup_owner(&holtmail).await;
    let member = owner
        .post(
            "/api/users",
            json!({ "email": MEMBER_EMAIL, "name": "Member Example", "password": MEMBER_PASSWORD }),
        )
        .await
        .ok();
    let member = member["user"]["id"].as_str().expect("an id").to_owned();
    (holtmail, owner, member)
}

fn scanner() -> LogScanner {
    LogScanner::new()
        .secret("the member's email", MEMBER_EMAIL)
        .secret("the member's password", MEMBER_PASSWORD)
        .secret("a wrong password", WRONG)
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_passwords_are_refused_and_limited_per_address_and_email() {
    require_it!();
    let (holtmail, _owner, _) = instance().await;

    let wrong = login(&holtmail, MEMBER_EMAIL, WRONG).await;
    assert_eq!(wrong.status(), 401);
    let wrong: Value = wrong.json().await.expect("JSON");
    let unknown = login(&holtmail, "nobody-here@example.com", WRONG).await;
    assert_eq!(unknown.status(), 401);
    let unknown: Value = unknown.json().await.expect("JSON");
    assert_eq!(
        wrong, unknown,
        "an unknown address is told apart from a wrong password"
    );

    for attempt in 2..=10 {
        let response = login(&holtmail, MEMBER_EMAIL, WRONG).await;
        assert_eq!(response.status(), 401, "attempt {attempt}");
    }
    for password in [WRONG, MEMBER_PASSWORD] {
        let limited = login(&holtmail, MEMBER_EMAIL, password).await;
        assert_eq!(limited.status(), 429);
        let retry: u64 = limited
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .expect("Retry-After in seconds");
        assert!((1..=15 * 60).contains(&retry), "Retry-After {retry}");
    }
    let other = login(&holtmail, OWNER_EMAIL, WRONG).await;
    assert_eq!(
        other.status(),
        401,
        "another email from the same address is limited"
    );
    let other = login(&holtmail, OWNER_EMAIL, OWNER_PASSWORD).await;
    assert_eq!(other.status(), 200);

    scanner().check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_end_on_expiry_sign_out_and_a_new_password() {
    require_it!();
    let (holtmail, _owner, member) = instance().await;

    let signed_in = login(&holtmail, MEMBER_EMAIL, MEMBER_PASSWORD).await;
    assert_eq!(signed_in.status(), 200);
    let set = signed_in.headers()[SET_COOKIE].to_str().unwrap().to_owned();
    assert!(
        set.contains(&format!("Max-Age={}", 30 * 24 * 60 * 60)),
        "{set}"
    );
    assert!(
        !set.contains("Secure"),
        "plain http marked the cookie Secure: {set}"
    );
    let expiring = session_cookie(&signed_in);
    let kept = session_cookie(&login(&holtmail, MEMBER_EMAIL, MEMBER_PASSWORD).await);
    assert_eq!(status_with(&holtmail, &expiring).await, 200);
    let rows = holtmail
        .query("SELECT expires_at - created_at FROM sessions", &[])
        .await;
    assert!(
        rows.iter()
            .all(|r| r[0] == (30_i64 * 24 * 3_600_000).to_string()),
        "{rows:?}"
    );

    let changed = holtmail
        .execute(
            "UPDATE sessions SET expires_at = CAST(strftime('%s','now') AS INTEGER) * 1000 - 1000 \
             WHERE rowid = (SELECT MIN(rowid) FROM sessions WHERE user_id = ?)",
            &[&member],
        )
        .await;
    assert_eq!(changed, 1, "the first session of the member is found");
    assert_eq!(
        status_with(&holtmail, &expiring).await,
        401,
        "an expired session still works"
    );
    assert_eq!(
        status_with(&holtmail, &kept).await,
        200,
        "another session ended too"
    );

    let logged_out = plain()
        .post(format!("{}/api/logout", holtmail.url()))
        .header(COOKIE, &kept)
        .header(CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .expect("an answer");
    assert_eq!(logged_out.status(), 204);
    assert!(
        logged_out.headers()[SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        status_with(&holtmail, &kept).await,
        401,
        "a signed-out cookie still works"
    );

    let before = session_cookie(&login(&holtmail, MEMBER_EMAIL, MEMBER_PASSWORD).await);
    let new_password = "it-signin-member-2";
    let output = holtmail.command(
        &["user", "password", "--email", MEMBER_EMAIL],
        &format!("{new_password}\n"),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        status_with(&holtmail, &before).await,
        401,
        "a session outlived a new password"
    );
    assert_eq!(
        login(&holtmail, MEMBER_EMAIL, MEMBER_PASSWORD)
            .await
            .status(),
        401
    );
    assert_eq!(
        login(&holtmail, MEMBER_EMAIL, new_password).await.status(),
        200
    );

    let logs: String = holtmail
        .logs()
        .lines()
        .map(|line| line.trim_start_matches(&format!("Password for {MEMBER_EMAIL}: ")))
        .collect::<Vec<_>>()
        .join("\n");
    scanner()
        .secret("the new password", new_password)
        .check(&logs);
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_site_writes_and_writes_without_json_are_refused() {
    require_it!();
    let (holtmail, owner, _) = instance().await;
    let cookie = session_cookie(&login(&holtmail, OWNER_EMAIL, OWNER_PASSWORD).await);
    let base = holtmail.url();
    let port = base.rsplit(':').next().expect("a port");
    let write = |origin: Option<String>,
                 content_type: Option<&'static str>,
                 fetch_site: Option<&'static str>| {
        let mut request = plain()
            .post(format!("{base}/api/archives"))
            .header(COOKIE, &cookie)
            .body(json!({ "name": "Refused archive" }).to_string());
        if let Some(origin) = origin {
            request = request.header(ORIGIN, origin);
        }
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        if let Some(site) = fetch_site {
            request = request.header("sec-fetch-site", site);
        }
        request
    };
    let cases: Vec<(&str, reqwest::RequestBuilder, u16)> = vec![
        (
            "another site",
            write(
                Some("https://attacker.example.org".into()),
                Some("application/json"),
                None,
            ),
            403,
        ),
        (
            "another port",
            write(
                Some(format!(
                    "http://127.0.0.1:{}",
                    port.parse::<u16>().unwrap() + 1
                )),
                Some("application/json"),
                None,
            ),
            403,
        ),
        (
            "another host name",
            write(
                Some(format!("http://localhost:{port}")),
                Some("application/json"),
                None,
            ),
            403,
        ),
        (
            "a null origin",
            write(Some("null".into()), Some("application/json"), None),
            403,
        ),
        (
            "cross-site fetch",
            write(
                Some(base.clone()),
                Some("application/json"),
                Some("cross-site"),
            ),
            403,
        ),
        (
            "cross-site without origin",
            write(None, Some("application/json"), Some("cross-site")),
            403,
        ),
        (
            "no content type",
            write(Some(base.clone()), None, None),
            415,
        ),
        (
            "a form",
            write(
                Some(base.clone()),
                Some("application/x-www-form-urlencoded"),
                None,
            ),
            415,
        ),
        (
            "plain text",
            write(Some(base.clone()), Some("text/plain"), None),
            415,
        ),
        (
            "multipart",
            write(
                Some(base.clone()),
                Some("multipart/form-data; boundary=x"),
                None,
            ),
            415,
        ),
    ];
    for (what, request, expected) in cases {
        let response = request.send().await.expect("an answer");
        assert_eq!(response.status().as_u16(), expected, "{what}");
    }
    let login_cross_site = plain()
        .post(format!("{base}/api/login"))
        .header(ORIGIN, "https://attacker.example.org")
        .header(CONTENT_TYPE, "application/json")
        .body(json!({ "email": OWNER_EMAIL, "password": OWNER_PASSWORD }).to_string())
        .send()
        .await
        .expect("an answer");
    assert_eq!(
        login_cross_site.status(),
        403,
        "a sign-in from another site"
    );
    assert!(
        owner
            .get("/api/archives")
            .await
            .ok()
            .as_array()
            .unwrap()
            .is_empty(),
        "a refused write created an archive"
    );

    for (what, request, expected) in [
        (
            "same origin",
            write(
                Some(base.clone()),
                Some("application/json; charset=utf-8"),
                Some("same-origin"),
            ),
            201,
        ),
        (
            "no origin (a script)",
            write(None, Some("application/json"), None),
            201,
        ),
    ] {
        let response = request.send().await.expect("an answer");
        assert_eq!(response.status().as_u16(), expected, "{what}");
    }
    let read = plain()
        .get(format!("{base}/api/archives"))
        .header(COOKIE, &cookie)
        .header(ORIGIN, "https://attacker.example.org")
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .expect("an answer");
    assert_eq!(read.status(), 200, "reads are not refused by origin");
    scanner().check(&holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_setup_link_works_once() {
    require_it!();
    let mut holtmail = Holtmail::start_default().await;
    let token = holtmail.setup_token().expect("a setup link");
    let api = Api::new(&holtmail);
    assert_eq!(api.get("/api/setup").await.ok(), json!({ "needed": true }));
    assert_eq!(api.get("/api/archives").await.status, 401);
    let setup = |token: String, email: &'static str| json!({ "token": token, "email": email, "name": "Owner Example", "password": OWNER_PASSWORD });
    let wrong = api
        .post("/api/setup", setup(format!("{token}x"), OWNER_EMAIL))
        .await;
    assert_eq!(wrong.status, 403, "{}", wrong.body);
    let short = api
        .post("/api/setup", json!({ "token": token, "email": OWNER_EMAIL, "name": "Owner Example", "password": "short" }))
        .await;
    assert_eq!(short.status, 400, "{}", short.body);
    let done = api
        .post("/api/setup", setup(token.clone(), OWNER_EMAIL))
        .await;
    assert_eq!(done.status, 200, "{}", done.body);
    assert_eq!(done.body["user"]["role"], "owner");
    assert_eq!(
        api.get("/api/archives").await.status,
        200,
        "setup signs the owner in"
    );

    let again = Api::new(&holtmail)
        .post(
            "/api/setup",
            setup(token.clone(), "second-owner@example.com"),
        )
        .await;
    assert_eq!(again.status, 403, "{}", again.body);
    assert_eq!(api.get("/api/setup").await.ok(), json!({ "needed": false }));

    let starts = holtmail.logs().matches("setup?token=").count();
    holtmail.restart().await;
    assert_eq!(
        holtmail.logs().matches("setup?token=").count(),
        starts,
        "a setup link was printed with an owner present"
    );
    let after = Api::new(&holtmail)
        .post("/api/setup", setup(token, "second-owner@example.com"))
        .await;
    assert_eq!(after.status, 403, "{}", after.body);
    let users = Api::sign_in(&holtmail, OWNER_EMAIL, OWNER_PASSWORD)
        .await
        .get("/api/users")
        .await
        .ok();
    assert_eq!(users.as_array().map(Vec::len), Some(1), "{users}");
    LogScanner::new().check(&holtmail.logs());
}
