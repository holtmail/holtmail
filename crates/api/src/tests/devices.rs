use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use holtmail_core::{ArchiveId, DeviceProtocol};
use holtmail_device::DeviceAuthError;
use holtmail_serve_net::TrustedProxies;

use super::*;
use crate::ServeOptions;

const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));

impl Fixture {
    fn with_serve(&self, serve: ServeOptions) -> Router {
        router(ApiState {
            serve,
            ..self.state.clone()
        })
    }

    async fn on(&self, app: &Router, cookie: Option<&str>, mut request: Request<Body>) -> Answer {
        request
            .headers_mut()
            .insert(header::HOST, "mail.example.com".parse().unwrap());
        if let Some(cookie) = cookie
            .or(Some(self.cookie.as_str()))
            .filter(|c| !c.is_empty())
        {
            request
                .headers_mut()
                .insert(header::COOKIE, cookie.parse().unwrap());
        }
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 7], 50_000))));
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, body)
    }

    async fn create(&self, fields: Value) -> Value {
        let (status, headers, body) = self.send(post_json("/api/devices", fields)).await;
        assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        serde_json::from_slice(&body).unwrap()
    }
}

fn stale() -> ServeOptions {
    ServeOptions {
        recent_sign_in: Duration::ZERO,
        ..ServeOptions::default()
    }
}

fn message(body: &Bytes) -> String {
    serde_json::from_slice::<Value>(body).unwrap()["error"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_the_secret_once_and_it_verifies() {
    let f = fixture().await;
    let made = f
        .create(json!({ "name": "  Ada's phone ", "protocols": ["jmap"], "scopes": ["read", "write:mail"] }))
        .await;
    let secret = made["secret"].as_str().unwrap();
    assert!(secret.starts_with("hm_") && secret.len() == 35, "{secret}");
    assert_eq!(made["device"]["name"], "Ada's phone");
    assert_eq!(made["device"]["scopes"], json!(["read", "write:mail"]));
    assert_eq!(made["device"]["protocols"], json!(["jmap"]));
    assert_eq!(made["device"]["archives"], Value::Null);
    assert_eq!(made["device"]["has_secret"], true);
    assert_eq!(made["device"]["implicit_seen"], false);

    let listed = f.json("/api/devices").await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], made["device"]["id"]);
    assert!(listed[0].get("secret").is_none());
    assert!(!listed.to_string().contains(secret), "the secret came back");

    let principal = f
        .state
        .devices
        .verify("ada@example.com", secret, PEER, DeviceProtocol::Jmap)
        .await
        .unwrap();
    assert_eq!(principal.device.to_string(), made["device"]["id"]);

    let audit = f.json("/api/audit").await;
    assert_eq!(audit[0]["kind"], "device_created");
    assert_eq!(audit[0]["targets"][0], made["device"]["id"]);
    assert!(
        !audit.to_string().contains(secret),
        "the secret was audited"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_device_is_read_only_by_default() {
    let f = fixture().await;
    let made = f
        .create(json!({ "name": "Tab", "protocols": ["dav"] }))
        .await;
    assert_eq!(made["device"]["scopes"], json!(["read"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_what_a_device_cannot_be() {
    let f = fixture().await;
    let archive = f.archive.clone();
    for (fields, expected) in [
        (json!({ "name": " ", "protocols": ["jmap"] }), "empty"),
        (json!({ "name": "A", "protocols": [] }), "protocol"),
        (json!({ "name": "A", "protocols": ["pop3"] }), "pop3"),
        (
            json!({ "name": "A", "protocols": ["jmap"], "scopes": [] }),
            "scope",
        ),
        (
            json!({ "name": "A", "protocols": ["jmap"], "scopes": ["rules"] }),
            "rules",
        ),
        (
            json!({ "name": "A", "protocols": ["imap"] }),
            "mail archive",
        ),
    ] {
        let (status, _, body) = f.send(post_json("/api/devices", fields.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{fields}");
        assert!(message(&body).contains(expected), "{}", body_text(&body));
    }
    let foreign = ArchiveId::new().to_string();
    for fields in [
        json!({ "name": "A", "protocols": ["imap"], "mail_archive": foreign }),
        json!({ "name": "A", "protocols": ["jmap"], "archives": [archive, foreign] }),
    ] {
        let (status, _, _) = f.send(post_json("/api/devices", fields.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{fields}");
    }
    let (status, _, _) = f
        .send(
            Request::post("/api/devices")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(f.json("/api/devices").await.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn making_a_device_and_raising_its_rights_need_a_recent_sign_in() {
    let f = fixture().await;
    let old = f.with_serve(stale());
    let fields = json!({ "name": "Phone", "protocols": ["jmap"] });
    let (status, _, body) = f.on(&old, None, post_json("/api/devices", fields)).await;
    assert_eq!(
        status,
        StatusCode::PRECONDITION_REQUIRED,
        "{}",
        body_text(&body)
    );
    assert!(f.json("/api/devices").await.as_array().unwrap().is_empty());

    let made = f
        .create(json!({ "name": "Phone", "protocols": ["jmap", "dav"], "scopes": ["read", "write:mail"] }))
        .await;
    let uri = format!("/api/devices/{}", made["device"]["id"].as_str().unwrap());

    let patch = |value: Value| request("PATCH", &uri, value);
    for raise in [
        json!({ "scopes": ["read", "write:mail", "send:mail"] }),
        json!({ "protocols": ["jmap", "dav", "api"] }),
    ] {
        let (status, _, _) = f.on(&old, None, patch(raise.clone())).await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{raise}");
    }
    let (status, _, body) = f
        .on(
            &old,
            None,
            patch(json!({ "name": "Old phone", "scopes": ["read"], "protocols": ["jmap"] })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "lowering needs no fresh sign-in: {}",
        body_text(&body)
    );
    let shown: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(shown["name"], "Old phone");
    assert_eq!(shown["scopes"], json!(["read"]));
    assert_eq!(shown["protocols"], json!(["jmap"]));

    let (status, _, body) = f
        .send(patch(json!({ "scopes": ["read", "send:mail"] })))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let kinds: Vec<Value> = f
        .json("/api/audit")
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        ["device_changed", "device_changed", "device_created"],
        "the newest entry comes first"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn clears_the_mail_archive_only_with_its_protocols() {
    let f = fixture().await;
    let archive = f.archive.clone();
    let made = f
        .create(json!({ "name": "Mac", "protocols": ["imap"], "mail_archive": archive }))
        .await;
    let uri = format!("/api/devices/{}", made["device"]["id"].as_str().unwrap());
    let (status, _, _) = f
        .send(request("PATCH", &uri, json!({ "mail_archive": null })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "mail_archive": null, "protocols": ["jmap"] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let shown: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(shown["mail_archive"], Value::Null);
    let (status, _, body) = f
        .send(request("PATCH", &uri, json!({ "archives": [archive] })))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let shown: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(shown["archives"], json!([archive]));
}

#[tokio::test(flavor = "multi_thread")]
async fn members_make_their_own_devices_unless_the_config_says_no() {
    let f = fixture().await;
    let (grace, member) = f.add_user("grace@example.org", "member").await;
    let archive: ArchiveId = f.archive.parse().unwrap();
    f.store
        .set_grants(grace.parse().unwrap(), &[archive])
        .await
        .unwrap();
    let fields =
        json!({ "name": "Grace's phone", "protocols": ["imap"], "mail_archive": f.archive });
    let (status, _, body) = f
        .send_as(&member, post_json("/api/devices", fields.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
    let made: Value = serde_json::from_slice(&body).unwrap();
    let uri = format!("/api/devices/{}", made["device"]["id"].as_str().unwrap());

    let closed = f.with_serve(ServeOptions {
        members: false,
        ..ServeOptions::default()
    });
    let (status, _, _) = f
        .on(
            &closed,
            Some(&member),
            post_json("/api/devices", fields.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f
        .on(
            &closed,
            Some(&member),
            request("PATCH", &uri, json!({ "scopes": ["read", "write:mail"] })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a raise is making a device");
    let (status, _, _) = f
        .on(
            &closed,
            Some(&member),
            request("PATCH", &uri, json!({ "name": "Phone" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "a member can still rename");
    let (status, _, _) = f
        .on(
            &closed,
            None,
            post_json(
                "/api/devices",
                json!({ "name": "Ada's", "protocols": ["jmap"] }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "the owner still can");

    let (status, _, body) = f
        .send_as(
            &member,
            Request::get("/api/devices").body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mine: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(mine.as_array().unwrap().len(), 1, "only their own");

    let foreign =
        json!({ "name": "A", "protocols": ["imap"], "mail_archive": ArchiveId::new().to_string() });
    let (status, _, _) = f.send_as(&member, post_json("/api/devices", foreign)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an archive they cannot read");

    let (status, _, _) = f
        .send(request("PATCH", &uri, json!({ "name": "Mine now" })))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another user's device");
    let (status, _, _) = f
        .send(Request::delete(&uri).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another user's device");
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_is_permanent_audited_and_reaches_the_feed() {
    let f = fixture().await;
    let made = f
        .create(json!({ "name": "Phone", "protocols": ["jmap"] }))
        .await;
    let secret = made["secret"].as_str().unwrap().to_owned();
    let uri = format!("/api/devices/{}", made["device"]["id"].as_str().unwrap());
    let mut feed = f.state.devices.revocations();

    let (status, _, _) = f
        .send(Request::delete(&uri).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(feed.next().await.to_string(), made["device"]["id"]);
    assert!(matches!(
        f.state
            .devices
            .verify("ada@example.com", &secret, PEER, DeviceProtocol::Jmap)
            .await,
        Err(DeviceAuthError::Revoked)
    ));
    let listed = f.json("/api/devices").await;
    assert!(listed[0]["revoked_at"].is_number(), "{listed}");

    let (status, _, _) = f
        .send(Request::delete(&uri).body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "revoking twice is fine");
    let kinds = f.json("/api/audit").await;
    assert_eq!(
        kinds
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "device_revoked")
            .count(),
        1
    );
    let (status, _, body) = f
        .send(request("PATCH", &uri, json!({ "name": "Back" })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body_text(&body));
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_user_revokes_their_devices_first() {
    let f = fixture().await;
    let (grace, member) = f.add_user("grace@example.org", "member").await;
    let (status, _, body) = f
        .send_as(
            &member,
            post_json(
                "/api/devices",
                json!({ "name": "Phone", "protocols": ["jmap"] }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
    let made: Value = serde_json::from_slice(&body).unwrap();
    let mut feed = f.state.devices.revocations();
    let (status, _, _) = f
        .send(
            Request::delete(format!("/api/users/{grace}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(feed.next().await.to_string(), made["device"]["id"]);
    let audit = f.json("/api/audit").await;
    let revoked = audit
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "device_revoked")
        .expect("an audit entry");
    assert_eq!(revoked["targets"][0], made["device"]["id"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_a_right_ends_live_sessions_and_a_rename_does_not() {
    let f = fixture().await;
    let made = f
        .create(json!({ "name": "Phone", "protocols": ["jmap"], "scopes": ["read", "write:mail"] }))
        .await;
    let uri = format!("/api/devices/{}", made["device"]["id"].as_str().unwrap());
    let mut feed = f.state.devices.revocations();
    let wait = Duration::from_millis(100);

    let (status, _, _) = f
        .send(request("PATCH", &uri, json!({ "name": "Tablet" })))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tokio::time::timeout(wait, feed.next()).await.is_err(),
        "a rename ends nothing"
    );

    let (status, _, _) = f
        .send(request("PATCH", &uri, json!({ "scopes": ["read"] })))
        .await;
    assert_eq!(status, StatusCode::OK);
    let ended = tokio::time::timeout(wait, feed.next()).await.expect("feed");
    assert_eq!(ended.to_string(), made["device"]["id"]);
}

fn login_from(address: &str) -> Request<Body> {
    let mut request = post_json(
        "/api/login",
        json!({ "email": "ada@example.com", "password": "wrong horse battery" }),
    );
    request
        .headers_mut()
        .insert("x-forwarded-for", address.parse().unwrap());
    request
}

#[tokio::test(flavor = "multi_thread")]
async fn believes_forwarded_addresses_only_from_trusted_proxies() {
    let f = fixture().await;
    let trusted = f.with_serve(ServeOptions {
        trusted_proxies: TrustedProxies::parse(&["192.0.2.0/24"]).unwrap(),
        ..ServeOptions::default()
    });
    for _ in 0..3 {
        let (status, _, _) = f.on(&trusted, None, login_from("203.0.113.9")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = f.on(&trusted, None, login_from("203.0.113.9")).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "the client is limited"
    );
    let (status, _, _) = f.on(&trusted, None, login_from("203.0.113.10")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "another client is not");
    let (status, _, _) = f
        .on(&trusted, None, login_from("203.0.113.9, 192.0.2.40"))
        .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a chain of trusted proxies is walked to the client"
    );

    for _ in 0..3 {
        let (status, _, _) = f.send(login_from("203.0.113.20")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = f.send(login_from("203.0.113.21")).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "without a trusted proxy the header means nothing"
    );
}

#[test]
fn walks_forwarded_hops_right_to_left() {
    use crate::session::forwarded_client;
    let trusted = TrustedProxies::parse(&["10.0.0.0/8", "2001:db8::/32"]).unwrap();
    let client = |values: &[&str]| {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("x-forwarded-for", value.parse().unwrap());
        }
        forwarded_client(&headers, &trusted).map(|ip| ip.to_string())
    };
    let ip = |text: &str| Some(text.to_owned());
    assert_eq!(client(&[]), None);
    assert_eq!(client(&["203.0.113.5"]), ip("203.0.113.5"));
    assert_eq!(
        client(&["198.51.100.1, 203.0.113.5, 10.0.0.2"]),
        ip("203.0.113.5")
    );
    assert_eq!(
        client(&["198.51.100.1", "203.0.113.5, 10.0.0.2"]),
        ip("203.0.113.5"),
        "repeated headers are one list"
    );
    assert_eq!(
        client(&["10.0.0.3, 10.0.0.2"]),
        ip("10.0.0.3"),
        "only proxies"
    );
    assert_eq!(
        client(&["nonsense, 10.0.0.2"]),
        ip("10.0.0.2"),
        "nothing left of a bad hop"
    );
    assert_eq!(client(&["nonsense"]), None);
    assert_eq!(client(&["203.0.113.5:4711"]), ip("203.0.113.5"));
    assert_eq!(client(&["[2001:db9::1]:4711"]), ip("2001:db9::1"));
    assert_eq!(client(&["::ffff:203.0.113.5"]), ip("203.0.113.5"));
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_says_what_this_server_offers() {
    let f = fixture().await;
    assert_eq!(
        f.json("/api/devices/setup").await,
        json!({ "can_create": true, "servers": [] })
    );
    let (_, member) = f.add_user("grace@example.org", "member").await;
    let app = f.with_serve(ServeOptions {
        members: false,
        servers: vec![crate::ServeEndpoint {
            protocol: DeviceProtocol::Smtp,
            host: Some("mail.example.com".into()),
            port: 465,
            implicit_tls: true,
        }],
        ..ServeOptions::default()
    });
    let get = || {
        Request::get("/api/devices/setup")
            .body(Body::empty())
            .unwrap()
    };
    let (status, _, body) = f.on(&app, Some(&member), get()).await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    let shown: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        shown,
        json!({
            "can_create": false,
            "servers": [{ "protocol": "smtp", "host": "mail.example.com", "port": 465, "security": "tls" }],
        })
    );
    let (_, _, body) = f.on(&app, None, get()).await;
    let shown: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(shown["can_create"], true, "the owner always can");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_raise_is_remembered_and_a_lowering_is_not() {
    let f = fixture().await;
    let made = f
        .create(json!({ "name": "Phone", "protocols": ["jmap"], "scopes": ["read"] }))
        .await;
    assert_eq!(made["device"]["raised_at"], Value::Null);
    let uri = format!("/api/devices/{}", made["device"]["id"].as_str().unwrap());
    let (_, _, body) = f
        .send(request("PATCH", &uri, json!({ "name": "Tablet" })))
        .await;
    let renamed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(renamed["raised_at"], Value::Null);
    let (_, _, body) = f
        .send(request(
            "PATCH",
            &uri,
            json!({ "scopes": ["read", "send:mail"] }),
        ))
        .await;
    let raised: Value = serde_json::from_slice(&body).unwrap();
    assert!(raised["raised_at"].is_i64(), "{raised}");
    let (_, _, body) = f
        .send(request("PATCH", &uri, json!({ "scopes": ["read"] })))
        .await;
    let lowered: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(lowered["raised_at"], raised["raised_at"]);
}
