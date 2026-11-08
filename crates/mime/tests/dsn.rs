//! Reading the invented delivery status notifications in `tests/fixtures`.

use holtmail_mime::{Dsn, DsnAction, DsnRecipient, parse_dsn, parse_unstructured_bounce};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn recipient(
    final_recipient: &str,
    action: DsnAction,
    status: &str,
    diagnostic_code: Option<&str>,
) -> DsnRecipient {
    DsnRecipient {
        final_recipient: Some(final_recipient.to_owned()),
        original_recipient: None,
        action: Some(action),
        status: Some(status.to_owned()),
        diagnostic_code: diagnostic_code.map(str::to_owned),
    }
}

#[test]
fn reads_a_postfix_bounce_with_attached_headers() {
    let dsn = parse_dsn(&fixture("dsn-postfix.eml")).expect("a report");
    assert_eq!(dsn.reporting_mta.as_deref(), Some("mx.example.com"));
    assert_eq!(dsn.original_envelope_id, None);
    assert_eq!(
        dsn.original_message_id.as_deref(),
        Some("figures-2@example.com")
    );
    assert_eq!(
        dsn.recipients,
        vec![DsnRecipient {
            original_recipient: Some("nobody@example.org".to_owned()),
            ..recipient(
                "nobody@example.org",
                DsnAction::Failed,
                "5.1.1",
                Some("550 5.1.1 <nobody@example.org>: Recipient address rejected: User unknown"),
            )
        }]
    );
}

#[test]
fn reads_an_exchange_bounce_with_the_attached_message() {
    let dsn = parse_dsn(&fixture("dsn-exchange.eml")).expect("a report");
    assert_eq!(
        dsn.reporting_mta.as_deref(),
        Some("AM0PR01MB0001.eurprd01.prod.contoso.example.com")
    );
    assert_eq!(
        dsn.original_message_id.as_deref(),
        Some("budget-7@example.com")
    );
    assert_eq!(
        dsn.recipients,
        vec![DsnRecipient {
            original_recipient: Some("carol@contoso.example.com".to_owned()),
            ..recipient(
                "carol@contoso.example.com",
                DsnAction::Failed,
                "5.1.10",
                Some(
                    "550 5.1.10 RESOLVER.ADR.RecipientNotFound; Recipient not found by SMTP address lookup"
                ),
            )
        }]
    );
}

#[test]
fn reads_a_gmail_shaped_bounce() {
    let dsn = parse_dsn(&fixture("dsn-gmail.eml")).expect("a report");
    assert_eq!(dsn.reporting_mta.as_deref(), Some("googlemail.example.com"));
    assert_eq!(
        dsn.original_message_id.as_deref(),
        Some("picnic-3@example.com")
    );
    assert_eq!(
        dsn.recipients,
        vec![recipient(
            "dave@example.org",
            DsnAction::Failed,
            "5.1.1",
            Some(
                "550-5.1.1 The email account that you tried to reach does not exist. 550 5.1.1 Please check the address."
            ),
        )]
    );
}

#[test]
fn falls_back_to_the_envelope_id_for_a_delay_without_attachment() {
    let dsn = parse_dsn(&fixture("dsn-delayed.eml")).expect("a report");
    assert_eq!(
        dsn.original_envelope_id.as_deref(),
        Some("minutes-4@example.com")
    );
    assert_eq!(
        dsn.original_message_id.as_deref(),
        Some("minutes-4@example.com")
    );
    assert_eq!(
        dsn.recipients,
        vec![recipient(
            "erin@slow.example.org",
            DsnAction::Delayed,
            "4.4.1",
            Some("connect to slow.example.org[192.0.2.30]:25: Connection timed out"),
        )]
    );
}

#[test]
fn reads_every_recipient_of_a_global_report() {
    let dsn = parse_dsn(&fixture("dsn-multiple.eml")).expect("a report");
    assert_eq!(dsn.reporting_mta.as_deref(), Some("relay.example.net"));
    assert_eq!(
        dsn.original_envelope_id.as_deref(),
        Some("newsletter-5@example.com")
    );
    assert_eq!(
        dsn.original_message_id.as_deref(),
        Some("newsletter-5@example.com")
    );
    assert_eq!(
        dsn.recipients,
        vec![
            recipient("frank@example.org", DsnAction::Delivered, "2.0.0", None),
            DsnRecipient {
                original_recipient: Some("team@example.org".to_owned()),
                ..recipient("grace@example.org", DsnAction::Expanded, "2.0.0", None)
            },
            recipient(
                "jörg@example.org",
                DsnAction::Failed,
                "5.2.2",
                Some("552 5.2.2 Mailbox full"),
            ),
            recipient("heidi@example.org", DsnAction::Relayed, "2.0.0", None),
        ]
    );
}

#[test]
fn reads_what_it_can_from_a_malformed_report() {
    let dsn = parse_dsn(&fixture("dsn-malformed.eml")).expect("a report");
    assert_eq!(dsn.reporting_mta, None);
    assert_eq!(dsn.original_message_id, None);
    let last = dsn.recipients.last().expect("a recipient");
    assert_eq!(
        last,
        &recipient(
            "ivan@example.org",
            DsnAction::Failed,
            "5.7.1",
            Some("554 5.7.1 rejected"),
        )
    );
    assert!(
        dsn.recipients.iter().all(|r| r.final_recipient.is_none()
            || r.final_recipient.as_deref() == Some("ivan@example.org"))
    );
}

#[test]
fn ignores_messages_that_are_not_reports() {
    for name in [
        "plain.eml",
        "alternative-with-attachment.eml",
        "journal-report.eml",
        "not-a-message.eml",
        "bounce-unstructured.eml",
    ] {
        assert_eq!(parse_dsn(&fixture(name)), None, "{name}");
    }
}

#[test]
fn recognises_an_unstructured_bounce_from_postmaster() {
    let dsn = parse_unstructured_bounce(&fixture("bounce-unstructured.eml")).expect("a bounce");
    assert_eq!(
        dsn,
        Dsn {
            reporting_mta: None,
            original_envelope_id: None,
            original_message_id: Some("old-friends-6@example.com".to_owned()),
            recipients: vec![DsnRecipient {
                final_recipient: None,
                original_recipient: None,
                action: Some(DsnAction::Failed),
                status: None,
                diagnostic_code: None,
            }],
        }
    );
}

#[test]
fn leaves_reports_and_ordinary_mail_to_others() {
    for name in [
        "dsn-postfix.eml",
        "dsn-delayed.eml",
        "plain.eml",
        "journal-report.eml",
        "not-a-message.eml",
    ] {
        assert_eq!(parse_unstructured_bounce(&fixture(name)), None, "{name}");
    }
}

#[test]
fn never_panics_on_truncated_or_garbled_input() {
    for name in [
        "dsn-postfix.eml",
        "dsn-exchange.eml",
        "dsn-gmail.eml",
        "dsn-delayed.eml",
        "dsn-multiple.eml",
        "dsn-malformed.eml",
        "bounce-unstructured.eml",
    ] {
        let raw = fixture(name);
        for end in 0..raw.len() {
            let _ = parse_dsn(&raw[..end]);
            let _ = parse_unstructured_bounce(&raw[..end]);
        }
        let flipped: Vec<u8> = raw.iter().map(|b| b ^ 0x80).collect();
        let _ = parse_dsn(&flipped);
        let _ = parse_unstructured_bounce(&flipped);
    }
}
