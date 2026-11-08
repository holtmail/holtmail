//! Fixture and round-trip tests for `holtmail-compose`.

use std::path::PathBuf;

use holtmail_compose::{
    Answer, Attachment, CalendarPart, ComposeError, Draft, ExtraHeader, ForwardKind, ImipKind,
    Invitation, MAX_REFERENCES, Mailbox, Original, ReplyKind, build, forward, imip, imip_subject,
    reply, shorten_references,
};
use holtmail_core::Timestamp;
use mail_parser::MessageParser;
use proptest::prelude::*;

const DATE: i64 = 1_793_007_000;

type Change = Box<dyn Fn(&mut Draft)>;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn normalize(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).expect("utf-8 output");
    let mut boundaries: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(start) = line.find("boundary=\"") {
            let rest = &line[start + 10..];
            boundaries.push(rest[..rest.find('"').unwrap()].to_owned());
        }
    }
    let mut out = text
        .lines()
        .map(|line| {
            if line.starts_with("Message-ID: <") {
                "Message-ID: <ID>".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    for (index, boundary) in boundaries.iter().enumerate() {
        out = out.replace(boundary, &format!("BOUNDARY-{index}"));
    }
    out
}

fn check(name: &str, draft: &Draft) -> mail_parser::Message<'static> {
    let built = build(draft).expect("builds");
    assert!(
        built
            .bytes
            .windows(2)
            .all(|w| w[1] != b'\n' || w[0] == b'\r'),
        "{name}: bare LF"
    );
    let actual = normalize(&built.bytes);
    let path = fixture(name);
    if std::env::var_os("HOLTMAIL_BLESS").is_some() {
        std::fs::write(&path, actual.replace('\n', "\r\n") + "\r\n").unwrap();
    }
    let expected = std::fs::read(&path).expect("fixture exists");
    assert_eq!(actual, normalize(&expected), "{name}");
    let message = MessageParser::new()
        .parse(&built.bytes)
        .expect("parses back")
        .into_owned();
    assert_eq!(message.message_id(), Some(built.message_id.as_str()));
    message
}

fn ada() -> Mailbox {
    Mailbox::named("Ada Lovelace", "ada@example.com")
}

fn me() -> Mailbox {
    Mailbox::named("Me Myself", "me@example.net")
}

fn original() -> (Original, Vec<u8>) {
    let raw = std::fs::read(fixture("original.eml")).unwrap();
    (Original::parse(&raw).expect("original parses"), raw)
}

#[test]
fn plain_text_draft() {
    let draft = Draft {
        from: Mailbox::named("Jürgen Müller", "juergen@example.com"),
        to: vec![ada(), Mailbox::new("bob@example.org")],
        cc: vec![Mailbox::new("Carol@Example.org")],
        bcc: vec![
            Mailbox::new("secret@example.org"),
            Mailbox::new("BOB@example.org"),
        ],
        subject: "Grüße aus Köln".to_owned(),
        text: "Hello Ada,\n\nsee you on Friday.\n".to_owned(),
        date: Some(Timestamp::from_secs(DATE)),
        ..Draft::default()
    };
    let message = check("plain.eml", &draft);
    assert_eq!(message.subject(), Some("Grüße aus Köln"));
    assert_eq!(
        message.from().unwrap().first().unwrap().name(),
        Some("Jürgen Müller")
    );
    assert!(message.bcc().is_none());
    assert_eq!(
        message.body_text(0).unwrap(),
        "Hello Ada,\r\n\r\nsee you on Friday.\r\n"
    );
    let built = build(&draft).unwrap();
    assert!(
        !String::from_utf8_lossy(&built.bytes).contains("secret"),
        "Bcc leaked"
    );
    assert_eq!(
        built.recipients,
        [
            "ada@example.com",
            "bob@example.org",
            "Carol@example.org",
            "secret@example.org"
        ]
    );
    let (uuid, domain) = built.message_id.split_once('@').unwrap();
    assert_eq!(domain, "example.com");
    assert_eq!(uuid::Uuid::parse_str(uuid).unwrap().get_version_num(), 7);
}

#[test]
fn html_draft_with_inline_image_and_attachment() {
    let draft = Draft {
        from: ada(),
        to: vec![Mailbox::new("bob@example.org")],
        reply_to: vec![Mailbox::new("replies@example.com")],
        subject: "Logo".to_owned(),
        html: Some(
            "<p>Our <b>new</b> logo: <img src=\"cid:logo@example.com\" alt=\"logo\"></p>\
             <p><a href=\"https://example.com/brand\" onclick=\"track()\">Brand page</a></p>\
             <script>steal()</script>"
                .to_owned(),
        ),
        attachments: vec![
            Attachment {
                name: "logo.png".to_owned(),
                content_type: "image/png".to_owned(),
                bytes: b"\x89PNG\r\n\x1a\nnot really".to_vec(),
                content_id: Some("logo@example.com".to_owned()),
            },
            Attachment {
                name: "Übersicht 2026.pdf".to_owned(),
                content_type: "application/pdf".to_owned(),
                bytes: b"%PDF-1.4 invented".to_vec(),
                content_id: None,
            },
        ],
        headers: vec![ExtraHeader {
            name: "auto-submitted".to_owned(),
            value: "auto-generated".to_owned(),
        }],
        message_id_domain: Some("mail.example.com".to_owned()),
        date: Some(Timestamp::from_secs(DATE)),
        ..Draft::default()
    };
    let message = check("html-inline.eml", &draft);
    let html = message.body_html(0).unwrap();
    assert!(html.contains("cid:logo@example.com"));
    assert!(!html.contains("steal") && !html.contains("onclick"));
    assert!(message.body_text(0).unwrap().contains("Brand page"));
    assert_eq!(message.attachments().len(), 2);
    let built = build(&draft).unwrap();
    assert!(
        String::from_utf8_lossy(&built.bytes).contains("\r\nAuto-Submitted: auto-generated\r\n")
    );
    assert!(built.message_id.ends_with("@mail.example.com"));
}

#[test]
fn reply_all_to_the_original() {
    let (original, _) = original();
    let own = vec!["me-alias@example.net".to_owned()];
    let mut draft = reply(&original, ReplyKind::All, &own, me()).unwrap();
    assert_eq!(
        draft.to,
        vec![Mailbox::new("dev@lists.example.org")],
        "Mail-Followup-To wins"
    );
    let mut without_followup = original.clone();
    without_followup.mail_followup_to.clear();
    draft = reply(&without_followup, ReplyKind::All, &own, me()).unwrap();
    assert_eq!(
        draft.to,
        vec![
            Mailbox::named("Ada Replies", "replies@example.com"),
            Mailbox::named("Team", "team@example.org"),
            Mailbox::named("Bob Builder", "bob@example.org"),
        ]
    );
    assert_eq!(draft.cc, vec![Mailbox::named("Carol", "carol@example.org")]);
    draft.text = format!("Friday works.{}", draft.text);
    draft.html = draft
        .html
        .map(|html| html.replacen("<p><br></p>", "<p>Friday works.</p>", 1));
    draft.date = Some(Timestamp::from_secs(DATE));
    let message = check("reply-all.eml", &draft);
    assert_eq!(message.subject(), Some("Re: Plans for the café"));
    assert_eq!(
        message.references().unwrap().iter().collect::<Vec<_>>(),
        [
            "plans-1@example.com",
            "plans-2@example.com",
            "plans-3@example.com"
        ]
    );
    assert_eq!(
        message.in_reply_to().unwrap().iter().collect::<Vec<_>>(),
        ["plans-3@example.com"]
    );
    let html = message.body_html(0).unwrap();
    assert!(html.contains("<blockquote type=\"cite\">"));
    assert!(html.contains("https://images.example.com/map.png"));
    assert!(!html.contains("steal") && !html.contains("alert"));
    assert!(
        message
            .body_text(0)
            .unwrap()
            .contains("> Shall we meet at the cafe on Friday?\r\n>\r\n>> Earlier thoughts")
    );
}

#[test]
fn reply_to_sender_and_list() {
    let (original, _) = original();
    let draft = reply(&original, ReplyKind::Sender, &[], me()).unwrap();
    assert_eq!(
        draft.to,
        vec![Mailbox::named("Ada Replies", "replies@example.com")]
    );
    assert!(draft.cc.is_empty());
    let draft = reply(&original, ReplyKind::List, &[], me()).unwrap();
    assert_eq!(draft.to, vec![Mailbox::new("dev@lists.example.org")]);

    let mut no_list = original.clone();
    no_list.list_post = None;
    assert_eq!(
        reply(&no_list, ReplyKind::List, &[], me()),
        Err(ComposeError::NoListAddress)
    );

    let mut no_reply_to = original.clone();
    no_reply_to.reply_to.clear();
    let draft = reply(&no_reply_to, ReplyKind::Sender, &[], me()).unwrap();
    assert_eq!(draft.to, vec![ada()]);
}

#[test]
fn reply_to_own_message_goes_to_its_recipients() {
    let sent = Original {
        from: vec![me()],
        to: vec![ada(), Mailbox::new("ME@example.net")],
        cc: vec![
            Mailbox::new("bob@example.org"),
            Mailbox::new("ada@EXAMPLE.com"),
        ],
        subject: "Notes".to_owned(),
        message_id: Some("notes@example.net".to_owned()),
        ..Original::default()
    };
    let draft = reply(&sent, ReplyKind::Sender, &[], me()).unwrap();
    assert_eq!(draft.to, sent.to);
    let draft = reply(&sent, ReplyKind::All, &[], me()).unwrap();
    assert_eq!(draft.to, vec![ada()]);
    assert_eq!(draft.cc, vec![Mailbox::new("bob@example.org")]);
    assert_eq!(draft.references, ["notes@example.net"]);
    assert_eq!(draft.subject, "Re: Notes");
    assert!(draft.html.is_none());
}

#[test]
fn reply_all_moves_cc_to_to_when_to_is_empty() {
    let message = Original {
        from: vec![Mailbox::new("me@example.net")],
        to: vec![Mailbox::new("me@example.net")],
        cc: vec![Mailbox::new("carol@example.org")],
        ..Original::default()
    };
    let draft = reply(&message, ReplyKind::All, &[], me()).unwrap();
    assert_eq!(draft.to, vec![Mailbox::new("carol@example.org")]);
    assert!(draft.cc.is_empty());
}

#[test]
fn threading_uses_in_reply_to_when_references_are_missing() {
    let message = Original {
        from: vec![ada()],
        message_id: Some("b@example.com".to_owned()),
        in_reply_to: vec!["a@example.com".to_owned()],
        ..Original::default()
    };
    let draft = reply(&message, ReplyKind::Sender, &[], me()).unwrap();
    assert_eq!(draft.references, ["a@example.com", "b@example.com"]);
    assert_eq!(draft.in_reply_to.as_deref(), Some("b@example.com"));
}

#[test]
fn forward_as_attachment() {
    let (original, raw) = original();
    let mut draft = forward(&original, &raw, ForwardKind::Attachment, me());
    draft.to = vec![Mailbox::new("dana@example.org")];
    draft.text = "FYI\n".to_owned();
    draft.date = Some(Timestamp::from_secs(DATE));
    let message = check("forward-attachment.eml", &draft);
    assert_eq!(message.subject(), Some("Fwd: Plans for the café"));
    let attached = message.attachments().next().unwrap();
    let inner = attached.nested().expect("attached message parses");
    assert_eq!(inner.message_id(), Some("plans-3@example.com"));
    assert_eq!(attached.attachment_name(), Some("Plans for the café.eml"));
}

#[test]
fn forward_inline() {
    let (original, raw) = original();
    let mut draft = forward(&original, &raw, ForwardKind::Inline, me());
    draft.to = vec![Mailbox::new("dana@example.org")];
    draft.date = Some(Timestamp::from_secs(DATE));
    assert!(draft.to.len() == 1 && draft.cc.is_empty() && draft.references.is_empty());
    let message = check("forward-inline.eml", &draft);
    let text = message.body_text(0).unwrap();
    assert!(text.contains("---------- Forwarded message ----------"));
    assert!(text.contains("From: Ada Lovelace <ada@example.com>"));
    assert!(text.contains("Shall we meet at the cafe on Friday?"));
    assert_eq!(message.attachments().len(), 1);
    assert_eq!(
        message.attachments().next().unwrap().attachment_name(),
        Some("agenda.txt")
    );
}

#[test]
fn bcc_only_draft_has_undisclosed_recipients() {
    let draft = Draft {
        from: ada(),
        bcc: vec![Mailbox::new("hidden@example.org")],
        text: "Hi".to_owned(),
        ..Draft::default()
    };
    let built = build(&draft).unwrap();
    let text = String::from_utf8(built.bytes).unwrap();
    assert!(text.contains("To: undisclosed-recipients:;\r\n"));
    assert!(!text.contains("hidden"));
    assert_eq!(built.recipients, ["hidden@example.org"]);
}

#[test]
fn refuses_header_injection_and_bad_input() {
    let base = Draft {
        from: ada(),
        to: vec![Mailbox::new("bob@example.org")],
        text: "Hi".to_owned(),
        ..Draft::default()
    };
    let cases: Vec<(Change, ComposeError)> = vec![
        (
            Box::new(|d| d.subject = "Hi\r\nBcc: eve@example.org".to_owned()),
            ComposeError::Injection("subject"),
        ),
        (
            Box::new(|d| d.subject = "Hi\nX: y".to_owned()),
            ComposeError::Injection("subject"),
        ),
        (
            Box::new(|d| d.subject = "Hi\0".to_owned()),
            ComposeError::Injection("subject"),
        ),
        (
            Box::new(|d| d.to[0].name = Some("Bob\r\nBcc: eve@example.org".to_owned())),
            ComposeError::Injection("to"),
        ),
        (
            Box::new(|d| d.from.address = "ada@example.com\nBcc: eve@example.org".to_owned()),
            ComposeError::Injection("from"),
        ),
        (
            Box::new(|d| d.cc = vec![Mailbox::new("not an address")]),
            ComposeError::Address("cc"),
        ),
        (
            Box::new(|d| d.in_reply_to = Some("a@b\r\nX: y".to_owned())),
            ComposeError::Injection("in_reply_to"),
        ),
        (
            Box::new(|d| d.references = vec!["has space@example.com".to_owned()]),
            ComposeError::MessageId("references"),
        ),
        (
            Box::new(|d| {
                d.headers = vec![ExtraHeader {
                    name: "List-Unsubscribe".to_owned(),
                    value: "<https://example.com>".to_owned(),
                }]
            }),
            ComposeError::Header("List-Unsubscribe".to_owned()),
        ),
        (
            Box::new(|d| {
                d.headers = vec![ExtraHeader {
                    name: "Auto-Submitted".to_owned(),
                    value: "sometimes".to_owned(),
                }]
            }),
            ComposeError::Header("Auto-Submitted".to_owned()),
        ),
        (
            Box::new(|d| {
                d.headers = vec![ExtraHeader {
                    name: "Precedence".to_owned(),
                    value: "bulk\r\nBcc: eve@example.org".to_owned(),
                }]
            }),
            ComposeError::Injection("headers"),
        ),
        (
            Box::new(|d| {
                d.attachments = vec![Attachment {
                    name: "a\r\nb".to_owned(),
                    content_type: "text/plain".to_owned(),
                    ..Attachment::default()
                }]
            }),
            ComposeError::Injection("attachments"),
        ),
        (
            Box::new(|d| {
                d.attachments = vec![Attachment {
                    name: "a.txt".to_owned(),
                    content_type: "text/plain\r\nX: y".to_owned(),
                    ..Attachment::default()
                }]
            }),
            ComposeError::Attachment(0),
        ),
        (
            Box::new(|d| d.message_id_domain = Some("bad domain".to_owned())),
            ComposeError::Address("message_id_domain"),
        ),
        (Box::new(|d| d.to.clear()), ComposeError::NoRecipients),
    ];
    for (change, expected) in cases {
        let mut draft = base.clone();
        change(&mut draft);
        assert_eq!(build(&draft), Err(expected));
    }
    assert!(build(&base).is_ok());
}

#[test]
fn long_lines_in_an_attached_message_fall_back_to_base64() {
    let raw = format!(
        "From: a@example.com\nSubject: long\n\n{}\n",
        "x".repeat(1200)
    );
    let draft = Draft {
        from: ada(),
        to: vec![Mailbox::new("bob@example.org")],
        attachments: vec![Attachment {
            name: "long.eml".to_owned(),
            content_type: "message/rfc822".to_owned(),
            bytes: raw.into_bytes(),
            content_id: None,
        }],
        ..Draft::default()
    };
    let built = build(&draft).unwrap();
    let text = String::from_utf8(built.bytes.clone()).unwrap();
    assert!(text.contains("Content-Transfer-Encoding: base64"));
    assert!(text.lines().all(|line| line.len() <= 998));
    let message = MessageParser::new().parse(&built.bytes).unwrap();
    assert!(message.attachments().next().unwrap().nested().is_some());
}

#[test]
fn shortens_references_from_the_middle() {
    let ids: Vec<String> = (0..30).map(|i| format!("{i}@example.com")).collect();
    let short = shorten_references(&ids, MAX_REFERENCES);
    assert_eq!(short.len(), MAX_REFERENCES);
    assert_eq!(short[0], "0@example.com");
    assert_eq!(short[1], "11@example.com");
    assert_eq!(short.last().unwrap(), "29@example.com");
}

proptest! {
    #[test]
    fn references_keep_root_and_latest(
        ids in prop::collection::vec("[a-z0-9]{1,8}@example\\.com", 0..60),
        max in 0usize..30,
    ) {
        let short = shorten_references(&ids, max);
        let mut unique = ids.clone();
        let mut seen = std::collections::HashSet::new();
        unique.retain(|id| seen.insert(id.clone()));
        prop_assert!(short.len() <= max.max(2));
        prop_assert_eq!(short.len(), unique.len().min(max.max(2)));
        if let Some(first) = unique.first() {
            prop_assert_eq!(&short[0], first);
            prop_assert_eq!(short.last(), unique.last());
        }
        let tail = short.len().saturating_sub(1);
        prop_assert_eq!(&short[short.len() - tail..], &unique[unique.len() - tail..]);
        prop_assert_eq!(shorten_references(&short, max), short);
    }
}

const REPLY_ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//iMIP//EN\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:weekly-sync@example.org\r\nDTSTAMP:20261121T093000Z\r\nSEQUENCE:0\r\nORGANIZER;CN=Cleo Vance:mailto:cleo@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:ada.alias@example.com\r\nCOMMENT:See you there\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

fn accepted() -> Invitation {
    Invitation {
        kind: ImipKind::Reply(Answer::Accepted),
        calendar: REPLY_ICS.as_bytes().to_vec(),
        summary: Some("Weekly sync".into()),
        when: Some("2026-12-01 09:00 UTC".into()),
        location: None,
        comment: Some("See you there".into()),
    }
}

#[test]
fn imip_reply_answers_the_invitation_mail() {
    let raw = std::fs::read(fixture("invitation.eml")).unwrap();
    let invitation = Original::parse(&raw).expect("invitation parses");
    let mut draft = imip(
        Mailbox::named("Ada Quill", "ada.alias@example.com"),
        vec![Mailbox::named("Cleo Vance", "cleo@example.org")],
        &accepted(),
        Some(&invitation),
    );
    draft.date = Some(Timestamp::from_secs(DATE));
    let message = check("imip-reply.eml", &draft);
    let calendar = message
        .parts()
        .find(|part| {
            part.content_type()
                .is_some_and(|t| t.ctype() == "text" && t.subtype() == Some("calendar"))
        })
        .expect("calendar part");
    assert_eq!(
        calendar.content_type().unwrap().attribute("method"),
        Some("REPLY")
    );
    let mut text = String::new();
    assert!(calendar.text_into(&mut text));
    assert_eq!(text, REPLY_ICS);
}

#[test]
fn imip_subjects_and_calendar_checks() {
    let mut invitation = accepted();
    for (kind, subject) in [
        (ImipKind::Invitation, "Invitation: Weekly sync"),
        (ImipKind::Update, "Updated invitation: Weekly sync"),
        (
            ImipKind::Reply(Answer::Tentative),
            "Tentatively accepted: Weekly sync",
        ),
        (ImipKind::Reply(Answer::Declined), "Declined: Weekly sync"),
        (ImipKind::Cancel, "Cancelled: Weekly sync"),
    ] {
        invitation.kind = kind;
        assert_eq!(imip_subject(&invitation), subject);
    }
    invitation.summary = Some("Line\r\nBcc: x@example.com".into());
    assert_eq!(
        imip_subject(&invitation),
        "Cancelled: Line Bcc: x@example.com"
    );
    invitation.summary = None;
    assert_eq!(imip_subject(&invitation), "Cancelled: (no title)");

    let draft = imip(
        ada(),
        vec![Mailbox::new("bob@example.org")],
        &invitation,
        None,
    );
    assert_eq!(draft.in_reply_to, None);
    assert!(draft.references.is_empty());
    assert_eq!(draft.calendar.as_ref().unwrap().method, "CANCEL");
    let built = build(&draft).unwrap();
    let text = String::from_utf8(built.bytes).unwrap();
    assert!(text.contains("Ada Lovelace cancelled \"(no title)\"."));

    let bad = |calendar: CalendarPart| Draft {
        calendar: Some(calendar),
        ..draft.clone()
    };
    assert_eq!(
        build(&bad(CalendarPart {
            method: "SEND".into(),
            bytes: REPLY_ICS.into()
        })),
        Err(ComposeError::Calendar)
    );
    assert_eq!(
        build(&bad(CalendarPart {
            method: "reply".into(),
            bytes: vec![0xff, 0xfe]
        })),
        Err(ComposeError::Calendar)
    );
    assert!(
        build(&bad(CalendarPart {
            method: "reply".into(),
            bytes: REPLY_ICS.into()
        }))
        .is_ok()
    );
}
