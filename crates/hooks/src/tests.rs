use super::*;

#[test]
fn writes_payloads_without_mail_content() {
    let archive = ArchiveId::new();
    let body = payload(
        "d1",
        Timestamp::from_millis(1_741_600_000_000),
        &Event::MessagesDeleted {
            archive_id: archive,
            messages: 3,
            by: None,
            ledger_seq: Some(9),
            ledger_hash: Some("ab".repeat(32)),
        },
    );
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["id"], "d1");
    assert_eq!(value["event"], "messages.deleted");
    assert_eq!(value["at"], 1_741_600_000_000i64);
    assert_eq!(value["data"]["archive_id"], archive.to_string());
    assert_eq!(value["data"]["messages"], 3);
    assert!(value["data"]["by"].is_null());
    let test = payload("d2", Timestamp::from_millis(0), &Event::Test {});
    let value: serde_json::Value = serde_json::from_slice(&test).unwrap();
    assert_eq!(value["event"], "webhook.test");
    assert_eq!(value["data"], serde_json::json!({}));
}

#[test]
fn signs_like_receivers_check() {
    let secret = b"whsec_example";
    let body = br#"{"id":"d1"}"#;
    let header = sign(secret, 1_741_600_000, body);
    assert!(header.starts_with("t=1741600000,v1="));
    assert_eq!(header.len(), "t=1741600000,v1=".len() + 64);
    let tolerance = Duration::from_secs(300);
    assert!(verify(secret, &header, body, 1_741_600_100, tolerance));
    assert!(
        !verify(secret, &header, body, 1_741_601_000, tolerance),
        "too old"
    );
    assert!(!verify(b"other", &header, body, 1_741_600_000, tolerance));
    assert!(!verify(secret, &header, b"{}", 1_741_600_000, tolerance));
    assert!(!verify(secret, "v1=00", body, 1_741_600_000, tolerance));
    let rotated = format!("{header},v1={}", "0".repeat(64));
    assert!(verify(secret, &rotated, body, 1_741_600_000, tolerance));
}

#[test]
fn tries_again_five_times_then_gives_up() {
    let waits: Vec<_> = (1..=MAX_ATTEMPTS).map(retry_after).collect();
    assert_eq!(
        waits,
        [
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(120)),
            Some(Duration::from_secs(600)),
            Some(Duration::from_secs(3_600)),
            Some(Duration::from_secs(21_600)),
            None
        ]
    );
    assert!(delivered(200) && delivered(204) && !delivered(301) && !delivered(500));
}

#[test]
fn makes_distinct_secrets() {
    let (a, b) = (new_secret(), new_secret());
    assert_ne!(a, b);
    assert!(a.starts_with("whsec_") && a.len() == 70);
}
