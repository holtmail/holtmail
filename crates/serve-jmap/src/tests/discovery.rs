use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};

use super::support::*;

#[tokio::test]
async fn the_well_known_address_redirects_to_the_session() {
    let h = harness();
    let answer = h.get("/.well-known/jmap", None).await;
    assert_eq!(answer.status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        answer.headers[header::LOCATION],
        "https://mail.example.com/jmap/session"
    );
}

#[tokio::test]
async fn signing_in_needs_an_app_password() {
    let h = harness();
    let answer = h.get("/jmap/session", None).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert!(
        answer.headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .starts_with("Basic")
    );
    let wrong = Creds {
        secret: "hm_wrong".into(),
        ..h.editor.clone()
    };
    assert_eq!(
        h.get("/jmap/session", Some(&wrong)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let bearer = Request::get("/jmap/session")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}:{}", h.editor.login, h.editor.secret),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.send(bearer).await.status, StatusCode::OK);
}

#[tokio::test]
async fn plain_http_signs_in_only_from_loopback() {
    let h = harness_with(|s| s.public_url = "http://mail.example.com".into());
    assert_eq!(
        h.get("/jmap/session", Some(&h.editor)).await.status,
        StatusCode::FORBIDDEN
    );
    let mut request = Request::get("/jmap/session")
        .header(header::AUTHORIZATION, h.editor.basic())
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(loopback());
    assert_eq!(h.send(request).await.status, StatusCode::OK);
}

#[tokio::test]
async fn browsers_get_cors_without_cookies() {
    let h = harness();
    let preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/jmap/api")
        .header(header::ORIGIN, "https://webmail.example.org")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .body(Body::empty())
        .unwrap();
    let answer = h.send(preflight).await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT);
    assert_eq!(answer.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert!(
        answer.headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("Authorization")
    );
    assert!(
        !answer
            .headers
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
    );
    let answer = h.get("/jmap/session", Some(&h.editor)).await;
    assert_eq!(answer.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
}

#[tokio::test]
async fn the_session_names_accounts_urls_limits_and_the_instance() {
    let h = harness();
    let session = h.get("/jmap/session", Some(&h.editor)).await.json();
    let account = h.account();
    assert_eq!(session["username"], "ada@example.com");
    assert_eq!(
        session["primaryAccounts"]["urn:ietf:params:jmap:mail"],
        json!(account)
    );
    assert_eq!(session["accounts"][&account]["name"], "Ada's mail");
    assert_eq!(session["accounts"][&account]["isReadOnly"], false);
    assert_eq!(session["apiUrl"], "https://mail.example.com/jmap/api");
    assert!(
        session["downloadUrl"]
            .as_str()
            .unwrap()
            .contains("{accountId}/{blobId}/{name}?accept={type}")
    );
    let core = &session["capabilities"]["urn:ietf:params:jmap:core"];
    assert_eq!(core["maxCallsInRequest"], 32);
    assert_eq!(core["maxObjectsInGet"], 500);
    assert_eq!(core["maxConcurrentRequests"], 4);
    assert_eq!(
        session["capabilities"]["urn:ietf:params:jmap:websocket"]["url"],
        "wss://mail.example.com/jmap/ws"
    );
    assert_eq!(
        session["capabilities"][crate::INSTANCE_CAPABILITY]["token"],
        TOKEN
    );
    assert!(session["state"].is_string());
}

#[tokio::test]
async fn request_errors_follow_rfc_8620() {
    let h = harness();
    let unknown = json!({"using": ["urn:example:nope"], "methodCalls": []});
    let answer = h
        .post(
            "/jmap/api",
            &h.editor,
            unknown.to_string(),
            "application/json",
        )
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        answer.json()["type"],
        "urn:ietf:params:jmap:error:unknownCapability"
    );
    let answer = h
        .post("/jmap/api", &h.editor, "{nope", "application/json")
        .await;
    assert_eq!(answer.json()["type"], "urn:ietf:params:jmap:error:notJSON");
    let calls: Vec<Value> = (0..33)
        .map(|i| json!(["Core/echo", {}, i.to_string()]))
        .collect();
    let many = json!({"using": USING, "methodCalls": calls});
    let answer = h
        .post("/jmap/api", &h.editor, many.to_string(), "application/json")
        .await;
    assert_eq!(answer.json()["limit"], "maxCallsInRequest");

    let account = h.account();
    let other = holtmail_core::ArchiveId::new().to_string();
    let ids: Vec<String> = (0..501).map(|i| format!("M{i}")).collect();
    let responses = h
        .calls(
            &h.editor,
            json!([
                ["Core/echo", {"hello": true}, "0"],
                ["Nope/get", {}, "1"],
                ["Mailbox/get", {"accountId": other}, "2"],
                ["Email/queryChanges", {"accountId": account, "sinceQueryState": "1"}, "3"],
                ["Email/get", {"accountId": account, "ids": ids}, "4"],
                ["Email/get", {"accountId": account, "#ids": {"resultOf": "9", "name": "Email/query", "path": "/ids"}}, "5"],
            ]),
        )
        .await;
    assert_eq!(responses[0], json!(["Core/echo", {"hello": true}, "0"]));
    let kind = |i: usize| responses[i][1]["type"].clone();
    assert_eq!(kind(1), "unknownMethod");
    assert_eq!(kind(2), "accountNotFound");
    assert_eq!(kind(3), "cannotCalculateChanges");
    assert_eq!(kind(4), "requestTooLarge");
    assert_eq!(kind(5), "invalidResultReference");
}

#[tokio::test]
async fn too_many_requests_at_once_hit_the_limit() {
    let h = harness();
    let slots: Vec<_> = (0..4)
        .map(|_| {
            let inner = &h.server.inner;
            let authed = crate::auth::Authed {
                principal: holtmail_device::Principal {
                    user: h.world.editor.user,
                    device: h.editor.device,
                    mail_archive: None,
                    archives: vec![h.archive],
                    scopes: Default::default(),
                    implicit_seen: false,
                },
                login: h.editor.login.clone(),
            };
            crate::request::slot(inner, &authed).unwrap()
        })
        .collect();
    let body = json!({"using": USING, "methodCalls": []});
    let answer = h
        .post("/jmap/api", &h.editor, body.to_string(), "application/json")
        .await;
    assert_eq!(answer.json()["limit"], "maxConcurrentRequests");
    drop(slots);
    let answer = h
        .post("/jmap/api", &h.editor, body.to_string(), "application/json")
        .await;
    assert_eq!(answer.status, StatusCode::OK);
}

#[tokio::test]
async fn browsers_get_no_password_prompt_and_other_sites_no_way_in() {
    let h = harness();
    let navigation = Request::get("/jmap/session")
        .header("sec-fetch-mode", "navigate")
        .body(Body::empty())
        .unwrap();
    let answer = h.send(navigation).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert!(
        answer.headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .starts_with("Bearer"),
        "a browser is never asked for Basic credentials"
    );

    let form = Request::post("/jmap/api")
        .header(header::AUTHORIZATION, h.editor.basic())
        .header(header::CONTENT_TYPE, "text/plain")
        .header("sec-fetch-site", "cross-site")
        .header("sec-fetch-mode", "navigate")
        .body(Body::from(
            json!({"using": USING, "methodCalls": []}).to_string(),
        ))
        .unwrap();
    assert_eq!(h.send(form).await.status, StatusCode::FORBIDDEN);

    let plain = h
        .post(
            "/jmap/api",
            &h.editor,
            json!({"using": USING, "methodCalls": []}).to_string(),
            "text/plain",
        )
        .await;
    assert_eq!(plain.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        plain.json()["type"],
        "urn:ietf:params:jmap:error:notJSON",
        "only application/json, which a browser preflights"
    );
    let charset = h
        .post(
            "/jmap/api",
            &h.editor,
            json!({"using": USING, "methodCalls": []}).to_string(),
            "application/json; charset=utf-8",
        )
        .await;
    assert_eq!(charset.status, StatusCode::OK);

    let socket = Request::get("/jmap/ws")
        .header(header::AUTHORIZATION, h.editor.basic())
        .header(header::HOST, "mail.example.com")
        .header(header::ORIGIN, "https://evil.example.org")
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-protocol", "jmap")
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.send(socket).await.status, StatusCode::FORBIDDEN);
}
