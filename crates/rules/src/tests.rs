use std::collections::BTreeSet;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;

use crate::cron::Cron;
use crate::template::{self, TemplateError};
use crate::*;

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn flow(v: serde_json::Value) -> Flow {
    serde_json::from_value(v).unwrap()
}

fn codes(f: &Flow) -> Vec<(Option<String>, IssueCode)> {
    validate(f)
        .unwrap_err()
        .into_iter()
        .map(|i| (i.node, i.code))
        .collect()
}

fn archived(id: &str) -> Event {
    Event {
        id: id.to_owned(),
        trigger: TriggerKind::MessageArchived,
        at: at("2026-11-10T10:00:00Z"),
        live: true,
    }
}

fn sheet_with(headers: &[(&str, &str)], address: &str) -> FactSheet {
    FactSheet {
        message: Some(MessageFacts {
            id: "m1".into(),
            sender: Some(Mailbox {
                name: None,
                address: address.into(),
            }),
            subject: Some("Hello".into()),
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }),
        own_addresses: vec!["ada@example.com".into()],
        ..Default::default()
    }
}

fn no_queries(_: &str) -> Result<bool, FactsError> {
    Err(FactsError::Unavailable("query".into()))
}

fn reply_flow() -> Flow {
    flow(json!({
        "trigger": { "type": "message.archived", "next": "r" },
        "nodes": [{ "id": "r", "type": "reply", "params": { "identity": "me", "body": "Away" } }]
    }))
}

fn skip_of(sheet: FactSheet) -> Option<Skip> {
    let plan = evaluate(
        &reply_flow(),
        &archived("e"),
        &WithMatcher(sheet, no_queries),
    )
    .unwrap();
    match &plan.path[0].outcome {
        Outcome::Skipped { reason } => Some(reason.clone()),
        _ => None,
    }
}

#[test]
fn catalogue_is_complete_and_documented() {
    let mut ids = BTreeSet::new();
    for t in catalogue() {
        assert!(ids.insert(t.id), "duplicate {}", t.id);
        for text in [t.docs.en, t.docs.ru] {
            assert!(
                !text.title.is_empty() && text.summary.len() > 10,
                "{}",
                t.id
            );
        }
        assert_ne!(t.docs.en.summary, t.docs.ru.summary);
        let schema = serde_json::to_value(t.schema()).unwrap();
        assert_eq!(schema["type"], "object", "{}", t.id);
        for field in t.templates {
            assert_eq!(
                schema["properties"][field]["x-holtmail"], "template",
                "{} {field}",
                t.id
            );
        }
        match t.role {
            Role::Condition => assert_eq!(t.ports, ["yes", "no"]),
            Role::Trigger => assert!(TriggerKind::from_id(t.id).is_some()),
            Role::Action if t.id == "stop" => assert!(t.ports.is_empty()),
            Role::Action => assert_eq!(t.ports, ["next"]),
        }
        assert!(!t.triggers.is_empty());
    }
    for kind in TriggerKind::ALL {
        assert_eq!(node_type(kind.id()).unwrap().role, Role::Trigger);
    }
    let described = serde_json::to_value(describe()).unwrap();
    assert_eq!(described.as_array().unwrap().len(), catalogue().len());
    assert_eq!(described[0]["id"], "message.archived");
}

#[test]
fn flow_round_trips_through_json() {
    let f = flow(json!({
        "trigger": { "type": "schedule", "params": { "cron": "0 8 * * 1-5" }, "next": "n" },
        "nodes": [{ "id": "n", "type": "notify", "params": { "title": "Morning" }, "position": { "x": 1.5, "y": 2.0 } }]
    }));
    assert_eq!(f.format, FLOW_FORMAT);
    let back = Flow::from_json(&f.to_json()).unwrap();
    assert_eq!(back, f);
    validate(&f).unwrap();
    assert!(Flow::from_json(r#"{"trigger":{"type":"manual"},"extra":1}"#).is_err());
}

#[test]
fn validation_reports_each_problem() {
    let f = flow(json!({
        "trigger": { "type": "message.archived", "next": "missing" },
        "nodes": [
            { "id": "a", "type": "query", "params": { "query": " " }, "next": { "maybe": "b" } },
            { "id": "a", "type": "label", "params": { "name": "x" } },
            { "id": "trigger", "type": "stop" },
            { "id": "b", "type": "no_such_type" },
            { "id": "c", "type": "set_identity", "params": { "identity": "x" } },
            { "id": "d", "type": "label", "params": { "name": "x" }, "confirm": true },
            { "id": "e", "type": "label", "version": 2, "params": { "name": "x" } },
            { "id": "f", "type": "label", "params": { "name": "x", "colour": "red" } },
            { "id": "g", "type": "manual" },
            { "id": "h", "type": "notify", "params": { "title": "{% include 'x' %}" } },
            { "id": "i", "type": "time_window", "params": { "from": "8:00", "to": "17:00", "timezone": "Mars/Base" } }
        ]
    }));
    let got = codes(&f);
    let want = [
        (Some("trigger"), IssueCode::UnknownTarget),
        (Some("a"), IssueCode::DuplicateId),
        (Some("trigger"), IssueCode::BadId),
        (Some("a"), IssueCode::UnknownPort),
        (Some("a"), IssueCode::BadParams),
        (Some("b"), IssueCode::UnknownType),
        (Some("c"), IssueCode::NotForTrigger),
        (Some("d"), IssueCode::ConfirmNotAllowed),
        (Some("e"), IssueCode::UnsupportedVersion),
        (Some("f"), IssueCode::BadParams),
        (Some("g"), IssueCode::TriggerInGraph),
        (Some("h"), IssueCode::BadTemplate),
        (Some("i"), IssueCode::BadTime),
        (Some("i"), IssueCode::BadTimezone),
    ];
    for (node, code) in want {
        assert!(
            got.contains(&(node.map(str::to_owned), code)),
            "missing {node:?} {code:?} in {got:?}"
        );
    }
}

#[test]
fn validation_refuses_cycles_formats_and_bad_triggers() {
    let cyclic = flow(json!({
        "trigger": { "type": "manual", "next": "a" },
        "nodes": [
            { "id": "a", "type": "sender_is_person", "next": { "yes": "b" } },
            { "id": "b", "type": "notify", "params": { "title": "t" }, "next": { "next": "a" } }
        ]
    }));
    assert!(codes(&cyclic).iter().any(|(_, c)| *c == IssueCode::Cycle));

    let newer = flow(json!({ "format": 2, "trigger": { "type": "manual" } }));
    assert_eq!(codes(&newer), vec![(None, IssueCode::Format)]);

    let not_trigger = flow(json!({ "trigger": { "type": "label" } }));
    assert_eq!(
        codes(&not_trigger),
        vec![(Some("trigger".into()), IssueCode::NotATrigger)]
    );

    let bad_cron = flow(json!({
        "trigger": { "type": "schedule", "params": { "cron": "61 * * * *", "timezone": "Nowhere" } }
    }));
    let got = codes(&bad_cron);
    assert!(got.contains(&(Some("trigger".into()), IssueCode::BadCron)));
    assert!(got.contains(&(Some("trigger".into()), IssueCode::BadTimezone)));

    let mut many = flow(json!({ "trigger": { "type": "manual" } }));
    for i in 0..=MAX_NODES {
        many.nodes.push(Node {
            id: format!("n{i}"),
            kind: "stop".into(),
            version: 1,
            params: json!({}),
            next: Default::default(),
            confirm: false,
            position: None,
        });
    }
    assert_eq!(codes(&many), vec![(None, IssueCode::TooManyNodes)]);
}

#[test]
fn capabilities_queries_and_counters_are_listed() {
    let f = flow(json!({
        "trigger": { "type": "message.archived", "next": "q" },
        "nodes": [
            { "id": "q", "type": "query", "params": { "query": "has:attachment" }, "next": { "yes": "m" } },
            { "id": "m", "type": "move", "params": { "folder": "Receipts" }, "next": { "next": "c" } },
            { "id": "c", "type": "counter", "params": { "scope": "sender", "window": "week", "above": 1 }, "next": { "yes": "r" } },
            { "id": "r", "type": "reply", "params": { "identity": "me", "body": "b" } }
        ]
    }));
    validate(&f).unwrap();
    assert_eq!(
        capabilities(&f).into_iter().collect::<Vec<_>>(),
        vec![Capability::MailMove, Capability::MailSend]
    );
    assert_eq!(f.queries(), vec![("q", "has:attachment")]);
    assert_eq!(
        counters_used(&f).into_iter().collect::<Vec<_>>(),
        vec![(params::CounterScope::Sender, params::CounterWindow::Week)]
    );
}

#[test]
fn templates_render_in_a_sandbox() {
    let ctx = json!({ "sender": { "name": "Bob" }, "subject": "Hi" });
    assert_eq!(
        template::render("{{ sender.name }}: {{ subject }}{{ missing }}", &ctx).unwrap(),
        "Bob: Hi"
    );
    assert_eq!(
        template::render("<b>{{ subject }}</b>", &json!({ "subject": "<x>" })).unwrap(),
        "<b><x></b>"
    );
    assert_eq!(
        template::render("[{{ subject }}]", &json!({ "subject": null })).unwrap(),
        "[]",
        "a missing value prints as empty, not as none"
    );
    assert!(matches!(
        template::check("{% include 'other' %}"),
        Err(TemplateError::Syntax(_))
    ));
    assert!(matches!(
        template::check("{% extends 'base' %}"),
        Err(TemplateError::Syntax(_))
    ));
    assert!(matches!(
        template::check("{% macro m() %}{% endmacro %}"),
        Err(TemplateError::Syntax(_))
    ));
    assert!(matches!(
        template::check("{{ unclosed"),
        Err(TemplateError::Syntax(_))
    ));
    assert_eq!(
        template::check(&"x".repeat(template::MAX_SOURCE + 1)),
        Err(TemplateError::SourceTooLarge)
    );
    assert_eq!(
        template::render(
            "{% for a in range(1000) %}{% for b in range(1000) %}{% endfor %}{% endfor %}",
            &ctx
        ),
        Err(TemplateError::TooSlow)
    );
    assert_eq!(
        template::render("{{ 'x' * 70000 }}", &ctx),
        Err(TemplateError::OutputTooLarge)
    );
}

#[test]
fn cron_parses_and_finds_the_next_minute() {
    let utc = chrono_tz::UTC;
    let c = Cron::parse("30 8 * * 1-5").unwrap();
    assert_eq!(
        c.next_after(at("2026-11-06T09:00:00Z"), utc),
        Some(at("2026-11-09T08:30:00Z"))
    );
    assert!(c.matches(at("2026-11-09T08:30:59Z"), utc));
    let every = Cron::parse("*/15 * * * *").unwrap();
    assert_eq!(
        every.next_after(at("2026-11-06T09:00:00Z"), utc),
        Some(at("2026-11-06T09:15:00Z"))
    );
    let either = Cron::parse("0 0 1 * 0").unwrap();
    assert_eq!(
        either.next_after(at("2026-11-02T00:00:00Z"), utc),
        Some(at("2026-11-08T00:00:00Z"))
    );
    let sunday = Cron::parse("0 12 * * 7").unwrap();
    assert_eq!(
        sunday.next_after(at("2026-11-02T00:00:00Z"), utc),
        Some(at("2026-11-08T12:00:00Z"))
    );
    let berlin = Cron::parse("0 9 * * *").unwrap();
    assert_eq!(
        berlin.next_after(at("2026-11-06T09:00:00Z"), chrono_tz::Europe::Berlin),
        Some(at("2026-11-07T08:00:00Z"))
    );
    assert_eq!(
        berlin.next_after(at("2026-03-28T09:00:00Z"), chrono_tz::Europe::Berlin),
        Some(at("2026-03-29T07:00:00Z"))
    );
    assert_eq!(
        Cron::parse("0 0 30 2 *")
            .unwrap()
            .next_after(at("2026-01-01T00:00:00Z"), utc),
        None
    );
    for bad in [
        "",
        "* * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "*/0 * * * *",
        "5-1 * * * *",
        "a * * * *",
    ] {
        assert!(Cron::parse(bad).is_err(), "{bad}");
    }
    assert!(Cron::parse("1,2,10-20/5 0-23/6 1-31 1-12 0-7").is_ok());
}

#[test]
fn trigger_and_liveness_gate_a_run() {
    let f = reply_flow();
    let facts = WithMatcher(sheet_with(&[], "bob@example.org"), no_queries);
    let mut e = archived("e");
    e.trigger = TriggerKind::Manual;
    let plan = evaluate(&f, &e, &facts).unwrap();
    assert_eq!(
        (plan.ran, plan.ignored),
        (false, Some(Ignored::OtherTrigger))
    );
    let mut e = archived("e");
    e.live = false;
    assert_eq!(
        evaluate(&f, &e, &facts).unwrap().ignored,
        Some(Ignored::NotLive)
    );
    let invalid = flow(json!({ "trigger": { "type": "nope" } }));
    assert!(matches!(
        evaluate(&invalid, &archived("e"), &facts),
        Err(EvalError::Invalid(_))
    ));
}

#[test]
fn auto_replies_follow_rfc_3834() {
    let ok = "bob@example.org";
    assert_eq!(skip_of(sheet_with(&[("Auto-Submitted", "no")], ok)), None);
    for (headers, want) in [
        (
            vec![("Auto-Submitted", "auto-replied")],
            Skip::AutoSubmitted,
        ),
        (
            vec![("auto-submitted", "auto-generated; owner=x")],
            Skip::AutoSubmitted,
        ),
        (
            vec![("X-Auto-Response-Suppress", "OOF, AutoReply")],
            Skip::AutoSubmitted,
        ),
        (vec![("List-Id", "<news.example.org>")], Skip::MailingList),
        (
            vec![("List-Unsubscribe", "<mailto:u@example.org>")],
            Skip::MailingList,
        ),
        (vec![("Precedence", "Bulk")], Skip::BulkPrecedence),
        (vec![("Return-Path", "<>")], Skip::NullReturnPath),
    ] {
        assert_eq!(skip_of(sheet_with(&headers, ok)), Some(want));
    }
    for robot in [
        "noreply@example.org",
        "no-reply@example.org",
        "do_not_reply@example.org",
        "MAILER-DAEMON@example.org",
        "bounces+123@example.org",
        "owner-list@example.org",
        "list-request@example.org",
    ] {
        assert_eq!(
            skip_of(sheet_with(&[], robot)),
            Some(Skip::NoReplySender),
            "{robot}"
        );
    }
    assert_eq!(
        skip_of(sheet_with(&[], "ADA@example.com")),
        Some(Skip::OwnAddress)
    );
    let mut no_sender = sheet_with(&[], ok);
    no_sender.message.as_mut().unwrap().sender = None;
    assert_eq!(skip_of(no_sender), Some(Skip::NoSender));
    assert_eq!(skip_of(FactSheet::default()), Some(Skip::NoMessage));

    let mut recent = sheet_with(&[], ok);
    recent.last_reply = Some(at("2026-11-04T10:00:00Z"));
    assert_eq!(
        skip_of(recent.clone()),
        Some(Skip::RepliedRecently {
            until: at("2026-11-11T10:00:00Z")
        })
    );
    recent.last_reply = Some(at("2026-11-03T10:00:00Z"));
    assert_eq!(skip_of(recent), None);
}

#[test]
fn conditions_decide_on_facts() {
    let f = flow(json!({
        "trigger": { "type": "message.archived", "next": "q" },
        "nodes": [
            { "id": "q", "type": "query", "params": { "query": "from:bob" }, "next": { "yes": "h" } },
            { "id": "h", "type": "header", "params": { "name": "x-tag", "op": "starts_with", "value": "ABC" }, "next": { "yes": "c" } },
            { "id": "c", "type": "counter", "params": { "scope": "flow", "window": "hour", "above": 2 }, "next": { "no": "t" } },
            { "id": "t", "type": "trust", "params": { "any_of": ["unknown"] }, "next": { "yes": "l" } },
            { "id": "l", "type": "label", "params": { "name": " Bob " } }
        ]
    }));
    let mut sheet = sheet_with(&[("X-Tag", "abc-123")], "bob@example.org");
    let facts = WithMatcher(sheet.clone(), |q: &str| Ok(q == "from:bob"));
    assert!(matches!(
        evaluate(&f, &archived("e"), &facts),
        Err(EvalError::MissingCounter { node }) if node == "c"
    ));
    sheet.counters.push(CounterFact {
        scope: params::CounterScope::Flow,
        window: params::CounterWindow::Hour,
        count: 2,
    });
    let facts = WithMatcher(sheet.clone(), |q: &str| Ok(q == "from:bob"));
    let plan = evaluate(&f, &archived("e"), &facts).unwrap();
    assert_eq!(plan.path.len(), 5);
    assert_eq!(
        plan.actions[0].action,
        Action::Label {
            message: "m1".into(),
            name: "Bob".into()
        }
    );
    let failing = WithMatcher(sheet, |_: &str| Err(FactsError::Query("bad".into())));
    assert!(matches!(
        evaluate(&f, &archived("e"), &failing),
        Err(EvalError::Facts { node, .. }) if node == "q"
    ));
    let empty = WithMatcher(FactSheet::default(), |_: &str| Ok(true));
    let plan = evaluate(&f, &archived("e"), &empty).unwrap();
    assert_eq!(plan.path[0].outcome, Outcome::Branch { port: "no".into() });
}

#[test]
fn time_windows_wrap_past_midnight() {
    let f = flow(json!({
        "trigger": { "type": "schedule", "params": { "cron": "* * * * *" }, "next": "w" },
        "nodes": [
            { "id": "w", "type": "time_window", "params": { "days": ["fri"], "from": "22:00", "to": "06:00", "timezone": "Europe/Berlin" } }
        ]
    }));
    let facts = WithMatcher(FactSheet::default(), no_queries);
    let port = |when: &str| {
        let e = Event {
            id: "s".into(),
            trigger: TriggerKind::Schedule,
            at: at(when),
            live: false,
        };
        match &evaluate(&f, &e, &facts).unwrap().path[0].outcome {
            Outcome::Branch { port } => port.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(port("2026-11-06T21:30:00Z"), "yes");
    assert_eq!(port("2026-11-07T04:00:00Z"), "yes");
    assert_eq!(port("2026-11-07T05:30:00Z"), "no");
    assert_eq!(port("2026-11-07T21:30:00Z"), "no");
    assert_eq!(port("2026-11-06T12:00:00Z"), "no");
}

#[test]
fn actions_skip_without_what_they_need() {
    let f = flow(json!({
        "trigger": { "type": "message.archived", "next": "r" },
        "nodes": [
            { "id": "r", "type": "rsvp", "params": { "answer": "declined" }, "next": { "next": "e" } },
            { "id": "e", "type": "create_event", "params": { "title": "Call {{ sender.address }}", "starts": "{{ insights.when }}" }, "next": { "next": "k" } },
            { "id": "k", "type": "create_contact", "next": { "next": "n" } },
            { "id": "n", "type": "notify", "params": { "title": "{{ subject|upper }}" } }
        ]
    }));
    let mut sheet = sheet_with(&[], "Bob@Example.org");
    let facts = WithMatcher(sheet.clone(), no_queries);
    let plan = evaluate(&f, &archived("e"), &facts).unwrap();
    let outcomes: Vec<_> = plan.path.iter().map(|s| s.outcome.clone()).collect();
    assert_eq!(
        outcomes,
        vec![
            Outcome::Skipped {
                reason: Skip::NoInvitation
            },
            Outcome::Skipped {
                reason: Skip::BadDate
            },
            Outcome::Planned { action: 0 },
            Outcome::Planned { action: 1 },
        ]
    );
    assert_eq!(
        plan.actions[1].action,
        Action::Notify {
            title: "HELLO".into(),
            body: String::new()
        }
    );
    sheet
        .message
        .as_mut()
        .unwrap()
        .insights
        .insert("when".into(), "2026-11-12T15:00:00+01:00".into());
    sheet.message.as_mut().unwrap().sender_is_person = true;
    let plan = evaluate(&f, &archived("e"), &WithMatcher(sheet, no_queries)).unwrap();
    assert_eq!(
        plan.actions[0].action,
        Action::CreateEvent {
            calendar: None,
            title: "Call Bob@Example.org".into(),
            starts: Utc.with_ymd_and_hms(2026, 11, 12, 14, 0, 0).unwrap(),
            duration_minutes: 60
        }
    );
    assert_eq!(
        plan.path[2].outcome,
        Outcome::Skipped {
            reason: Skip::AlreadyKnown
        }
    );
}

#[test]
fn forwards_never_loop_through_own_addresses() {
    let f = flow(json!({
        "trigger": { "type": "message.archived", "next": "f" },
        "nodes": [{ "id": "f", "type": "forward", "params": { "to": ["a@example.org"], "identity": "me" } }]
    }));
    let facts = WithMatcher(sheet_with(&[], "ada@example.com"), no_queries);
    let plan = evaluate(&f, &archived("e"), &facts).unwrap();
    assert_eq!(
        plan.path[0].outcome,
        Outcome::Skipped {
            reason: Skip::OwnAddress
        }
    );
    let facts = WithMatcher(
        sheet_with(&[("Auto-Submitted", "auto-generated")], "bob@example.org"),
        no_queries,
    );
    let plan = evaluate(&f, &archived("e"), &facts).unwrap();
    assert_eq!(
        plan.path[0].outcome,
        Outcome::Skipped {
            reason: Skip::AutoSubmitted
        },
        "another archive's rule forward is not forwarded back"
    );
    let facts = WithMatcher(
        sheet_with(&[("Auto-Submitted", "no")], "bob@example.org"),
        no_queries,
    );
    let plan = evaluate(&f, &archived("e"), &facts).unwrap();
    assert_eq!(plan.actions.len(), 1);
}

#[test]
fn template_context_is_whitelisted() {
    let mut sheet = sheet_with(&[("X-Secret", "s")], "bob@example.org");
    sheet.invitation = Some(EventFacts {
        uid: "u@example.org".into(),
        summary: Some("Sync".into()),
        starts: Some(at("2026-11-12T10:00:00Z")),
        ..Default::default()
    });
    let ctx = serde_json::to_value(TemplateContext::new(&archived("e"), &sheet)).unwrap();
    let keys: Vec<_> = ctx.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "alias",
            "date",
            "event",
            "insights",
            "invitation",
            "now",
            "sender",
            "subject"
        ]
    );
    assert_eq!(ctx["invitation"]["starts"], "2026-11-12T10:00:00Z");
    assert_eq!(ctx["now"], "2026-11-10T10:00:00Z");
}
