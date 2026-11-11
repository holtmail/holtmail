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
        json!({ "can_create": true, "apple_profile": false, "davx5": null, "autoconfig": [], "servers": [] })
    );
    let (_, member) = f.add_user("grace@example.org", "member").await;
    let app = f.with_serve(ServeOptions {
        members: false,
        servers: vec![
            crate::ServeEndpoint {
                protocol: DeviceProtocol::Smtp,
                host: Some("mail.example.com".into()),
                port: 465,
                implicit_tls: true,
                url: None,
            },
            crate::ServeEndpoint {
                protocol: DeviceProtocol::Dav,
                host: Some("mail.example.com".into()),
                port: 443,
                implicit_tls: true,
                url: Some("https://mail.example.com/dav/".into()),
            },
        ],
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
            "apple_profile": false,
            "davx5": null,
            "autoconfig": [],
            "servers": [
                { "protocol": "smtp", "host": "mail.example.com", "port": 465, "security": "tls", "url": null },
                { "protocol": "dav", "host": "mail.example.com", "port": 443, "security": "tls", "url": "https://mail.example.com/dav/" },
            ],
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

fn client_setup(https: bool, dav: bool) -> crate::ClientSetup {
    use holtmail_client_setup::{Ports, Server};
    crate::ClientSetup {
        server: Server {
            name: "Holtmail".into(),
            mail_host: "mail.example.com".into(),
            web_host: "mail.example.com".into(),
            web_port: if https { 443 } else { 8080 },
            web_tls: https,
            imap: Ports {
                tls: Some(993),
                starttls: None,
            },
            submission: Ports {
                tls: Some(465),
                starttls: Some(587),
            },
            jmap: true,
            dav,
        },
        instance: "instance-one".into(),
        mail_domains: vec!["example.com".into()],
    }
}

fn with_client(https: bool, dav: bool) -> ServeOptions {
    ServeOptions {
        client: Some(client_setup(https, dav)),
        ..ServeOptions::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_link_works_once_and_carries_a_fresh_app_password() {
    let f = fixture().await;
    let app = f.with_serve(with_client(true, true));
    let made = f
        .create(json!({ "name": "Ada's iPhone", "protocols": ["imap", "smtp", "dav"], "mail_archive": f.archive, "scopes": ["read", "send:mail"] }))
        .await;
    let old = made["secret"].as_str().unwrap().to_owned();
    let id = made["device"]["id"].as_str().unwrap();

    let setup = f
        .on(
            &app,
            None,
            Request::get("/api/devices/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let setup: Value = serde_json::from_slice(&setup.2).unwrap();
    assert_eq!(setup["apple_profile"], true);
    assert_eq!(
        setup["davx5"],
        "davx5://ada%40example.com@mail.example.com/dav/"
    );
    assert_eq!(setup["autoconfig"], json!(["example.com"]));

    let (status, headers, body) = f
        .on(
            &app,
            None,
            post_json(&format!("/api/devices/{id}/profile"), json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body_text(&body));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let link: Value = serde_json::from_slice(&body).unwrap();
    let url = link["url"].as_str().unwrap();
    let path = url
        .strip_prefix("https://mail.example.com")
        .expect("an absolute link to this server");
    assert!(path.starts_with("/api/devices/profile/"), "{url}");
    assert!(link["expires_at"].as_i64().unwrap() > holtmail_core::Timestamp::now().as_millis());

    for method in ["GET", "HEAD", "GET"] {
        let (status, headers, body) = f
            .on(
                &app,
                Some(""),
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{method}: {}", body_text(&body));
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html"),
            "{method} shows the confirmation page"
        );
        if method == "GET" {
            assert!(body_text(&body).contains("<form method=\"post\">"));
        }
    }
    assert!(
        f.state
            .devices
            .verify("ada@example.com", &old, PEER, DeviceProtocol::Imap)
            .await
            .is_ok(),
        "looking at the link changes nothing"
    );

    let (status, headers, body) = f
        .on(
            &app,
            Some(""),
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body_text(&body));
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/x-apple-aspen-config"
    );
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains(".mobileconfig")
    );
    let profile = body_text(&body);
    for part in [
        "com.apple.mail.managed",
        "com.apple.caldav.account",
        "com.apple.carddav.account",
        "<string>People</string>",
        "<string>ada@example.com</string>",
    ] {
        assert!(profile.contains(part), "{part} missing");
    }
    assert!(
        !profile.contains(&old),
        "the old app password is in the profile"
    );
    let start = profile.find("<string>hm_").expect("an app password") + "<string>".len();
    let fresh = &profile[start..start + 35];
    let verified = f
        .state
        .devices
        .verify("ada@example.com", fresh, PEER, DeviceProtocol::Imap)
        .await
        .unwrap();
    assert_eq!(verified.device.to_string(), id);
    assert!(matches!(
        f.state
            .devices
            .verify("ada@example.com", &old, PEER, DeviceProtocol::Imap)
            .await,
        Err(DeviceAuthError::Invalid)
    ));

    for request in [
        Request::post(path).body(Body::empty()).unwrap(),
        Request::get(path).body(Body::empty()).unwrap(),
    ] {
        let (status, _, _) = f.on(&app, Some(""), request).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "a link works once");
    }
    let audit = f.json("/api/audit").await;
    assert_eq!(audit[0]["kind"], "device_secret_issued");
    assert_eq!(audit[0]["targets"][0], id);
    assert!(!audit.to_string().contains(fresh));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_link_is_refused_when_it_cannot_work() {
    let f = fixture().await;
    let jmap = f
        .create(json!({ "name": "Laptop", "protocols": ["jmap"] }))
        .await;
    let dav = f
        .create(json!({ "name": "Tablet", "protocols": ["dav"] }))
        .await;
    let link = |made: &Value| {
        post_json(
            &format!(
                "/api/devices/{}/profile",
                made["device"]["id"].as_str().unwrap()
            ),
            json!({}),
        )
    };
    let none = f.with_serve(ServeOptions::default());
    assert_eq!(f.on(&none, None, link(&dav)).await.0, StatusCode::CONFLICT);
    let app = f.with_serve(with_client(true, false));
    let (status, _, body) = f.on(&app, None, link(&jmap)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        message(&body).contains("imap and smtp"),
        "{}",
        message(&body)
    );
    let (status, _, body) = f.on(&app, None, link(&dav)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(message(&body).contains("dav"), "{}", message(&body));

    let app = f.with_serve(ServeOptions {
        recent_sign_in: Duration::ZERO,
        ..with_client(true, true)
    });
    assert_eq!(
        f.on(&app, None, link(&dav)).await.0,
        StatusCode::PRECONDITION_REQUIRED
    );
    let app = f.with_serve(with_client(true, true));
    let (status, _, body) = f.on(&app, None, link(&dav)).await;
    assert_eq!(status, StatusCode::CREATED);
    let url: Value = serde_json::from_slice(&body).unwrap();
    let path = url["url"]
        .as_str()
        .unwrap()
        .trim_start_matches("https://mail.example.com")
        .to_owned();
    let id = dav["device"]["id"].as_str().unwrap();
    let answer = f
        .on(
            &app,
            None,
            Request::delete(format!("/api/devices/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(answer.0, StatusCode::NO_CONTENT);
    let (status, _, _) = f
        .on(
            &app,
            Some(""),
            Request::get(&path).body(Body::empty()).unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a revoked device gets no profile"
    );
    assert_eq!(
        f.on(&app, None, link(&dav)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, _, _) = f
        .on(
            &app,
            Some(""),
            Request::post("/api/devices/profile/0000")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn autoconfig_answers_only_for_the_own_mail_domains() {
    let f = fixture().await;
    let app = f.with_serve(with_client(true, true));
    let get = |uri: &str, host: &str| {
        Request::get(uri)
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap()
    };
    let well_known = "/.well-known/autoconfig/mail/config-v1.1.xml";
    for request in [
        get(
            &format!("{well_known}?emailaddress=ada%40example.com"),
            "example.com",
        ),
        get(well_known, "example.com"),
        get("/mail/config-v1.1.xml", "autoconfig.example.com"),
        get(
            "/mail/config-v1.1.xml?emailaddress=ada%40EXAMPLE.com",
            "autoconfig.example.com:443",
        ),
    ] {
        let uri = request.uri().to_string();
        let response = app.clone().oneshot(request).await.unwrap();
        let (status, headers) = (response.status(), response.headers().clone());
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("application/xml")
        );
        let xml = body_text(&body);
        assert!(xml.contains("<domain>example.com</domain>"), "{xml}");
        assert!(
            xml.contains("<hostname>mail.example.com</hostname>"),
            "{xml}"
        );
        assert!(!xml.contains("hm_"));
    }
    for request in [
        get(
            &format!("{well_known}?emailaddress=ada%40example.org"),
            "example.com",
        ),
        get(well_known, "example.org"),
        get("/mail/config-v1.1.xml", "autoconfig.example.org"),
    ] {
        let uri = request.uri().to_string();
        let status = app.clone().oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let off = f.with_serve(ServeOptions::default());
    let status = off
        .oneshot(get(well_known, "example.com"))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_davx5_link_without_https() {
    let f = fixture().await;
    let app = f.with_serve(with_client(false, true));
    let setup = f
        .on(
            &app,
            None,
            Request::get("/api/devices/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let setup: Value = serde_json::from_slice(&setup.2).unwrap();
    assert_eq!(setup["davx5"], Value::Null);
    assert_eq!(setup["apple_profile"], true);
}
