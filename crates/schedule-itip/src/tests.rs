use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::{ArchiveId, OutboxId};
use holtmail_route::{Route, Strategy};
use holtmail_schedule::testing::{Harness, ManualClock, Recorder, conformance, line_of, unfolded};
use holtmail_schedule::{
    Arrival, AuthFacts, Change, ClientPut, Clock, Host, HostCaps, Incoming, IncomingItip, Origin,
    ReplyAs, ReplyOutcome, Role, ScheduleError, ScheduleStatus, Scheduler, Unapplied, Way,
};
use holtmail_submission::testing::{FakeSubmission, Setup};
use holtmail_submission::{
    Actor, SendIdentity, Submission, SubmissionError, SubmissionLimits, SubmissionState, Submit,
    Submitted,
};

use crate::ItipScheduler;

struct Itip;

impl Harness for Itip {
    fn scheduler(&self, submission: Arc<dyn Submission>, clock: Clock) -> Arc<dyn Scheduler> {
        Arc::new(ItipScheduler::new(submission, clock))
    }
}

#[tokio::test]
async fn keeps_the_contract() {
    conformance(&Itip).await;
}

const ADA: &str = "ada@example.com";

fn cal(lines: &[&str]) -> Bytes {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\r\n");
    }
    Bytes::from(out)
}

fn weekly(summary: &str, exdate: Option<&str>, instance: &[&str]) -> Bytes {
    let summary = format!("SUMMARY:{summary}");
    let exdate = exdate.map(|e| format!("EXDATE:{e}"));
    let mut lines = vec![
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Example//Calendar//EN",
        "BEGIN:VEVENT",
        "UID:standup-7@example.org",
        "DTSTAMP:20261101T080000Z",
        "DTSTART:20261207T090000Z",
        "DTEND:20261207T091500Z",
        "RRULE:FREQ=WEEKLY;COUNT=10",
        "SEQUENCE:1",
        &summary,
        "ORGANIZER;CN=Bob:mailto:bob@example.org",
        "ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:bob@example.org",
        "ATTENDEE;CN=Ada;PARTSTAT=ACCEPTED:mailto:ada@example.com",
    ];
    if let Some(e) = &exdate {
        lines.push(e);
    }
    lines.push("END:VEVENT");
    lines.extend_from_slice(instance);
    lines.push("END:VCALENDAR");
    cal(&lines)
}

const DECLINED_INSTANCE: &[&str] = &[
    "BEGIN:VEVENT",
    "UID:standup-7@example.org",
    "DTSTAMP:20261101T080000Z",
    "RECURRENCE-ID:20261214T090000Z",
    "DTSTART:20261214T090000Z",
    "DTEND:20261214T091500Z",
    "SEQUENCE:1",
    "SUMMARY:Standup",
    "ORGANIZER;CN=Bob:mailto:bob@example.org",
    "ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:bob@example.org",
    "ATTENDEE;CN=Ada;PARTSTAT=DECLINED:mailto:ada@example.com",
    "END:VEVENT",
];

struct Setting {
    scheduler: ItipScheduler,
    recorder: Arc<Recorder>,
    actor: Actor,
    archive: ArchiveId,
    identity: String,
}

fn setting() -> Setting {
    let world = FakeSubmission::world(Setup::default());
    let recorder = Arc::new(Recorder::new(world.submission));
    let clock = ManualClock::new(holtmail_core::Timestamp::from_secs(1_793_520_000));
    Setting {
        scheduler: ItipScheduler::new(recorder.clone(), clock.clock()),
        recorder,
        actor: world.sender,
        archive: world.archive,
        identity: world.identity.to_string(),
    }
}

fn change(s: &Setting, previous: Option<Bytes>, next: Option<Bytes>) -> Change {
    Change {
        actor: s.actor.clone(),
        archive: s.archive,
        origin: Origin::Device,
        host: Host::CalDav {
            auto_schedule: false,
        },
        caps: HostCaps::default(),
        own: vec![ADA.into()],
        previous,
        next,
        reply_as: None,
        references: Vec::new(),
        comment: None,
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test]
async fn an_added_exdate_becomes_a_declined_instance() {
    let s = setting();
    let previous = weekly("Standup", None, &[]);
    let body = weekly("Standup moved by Ada", Some("20261214T090000Z"), &[]);
    let prepared = s
        .scheduler
        .prepare(&ClientPut {
            previous: Some(previous),
            body,
            own: vec![ADA.into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert!(prepared.reduced);
    let out = text(&prepared.body);
    assert!(out.contains("SUMMARY:Standup\r\n"), "{out}");
    assert!(!out.contains("EXDATE"), "{out}");
    assert!(out.contains("RECURRENCE-ID:20261214T090000Z"), "{out}");
    let lines = unfolded(&prepared.body);
    let declined = lines
        .iter()
        .filter(|l| l.contains("mailto:ada@example.com") && l.contains("PARTSTAT=DECLINED"))
        .count();
    assert_eq!(declined, 1, "{out}");
}

#[tokio::test]
async fn declining_one_occurrence_answers_that_instance() {
    let s = setting();
    let previous = weekly("Standup", None, &[]);
    let body = weekly("Standup", None, DECLINED_INSTANCE);
    let prepared = s
        .scheduler
        .prepare(&ClientPut {
            previous: Some(previous.clone()),
            body: body.clone(),
            own: vec![ADA.into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert!(
        !prepared.reduced,
        "Apple's decline of one occurrence is allowed"
    );
    assert_eq!(prepared.body, body);

    let d = s
        .scheduler
        .deliver(&change(&s, Some(previous), Some(body)))
        .await
        .expect("deliver");
    assert_eq!(d.role, Role::Attendee);
    assert_eq!(d.way, Way::Mail);
    let queued = s.recorder.queued();
    assert_eq!(queued.len(), 1, "one REPLY, for the instance only");
    let message = text(&queued[0].message);
    assert!(
        message.contains("RECURRENCE-ID:20261214T090000Z"),
        "{message}"
    );
    assert!(message.contains("PARTSTAT=DECLINED"), "{message}");
    assert!(!message.contains("RRULE"), "{message}");
}

#[tokio::test]
async fn moving_an_event_raises_sequence_and_cancels_removed_attendees() {
    let s = setting();
    let event = |start: &str, extra: &[&str]| {
        let start = format!("DTSTART:{start}");
        let mut lines = vec![
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "PRODID:-//Example//Calendar//EN",
            "BEGIN:VEVENT",
            "UID:offsite-3@example.com",
            "DTSTAMP:20261101T080000Z",
            &start,
            "DURATION:PT2H",
            "SEQUENCE:0",
            "SUMMARY:Offsite",
            "ORGANIZER:mailto:ada@example.com",
            "ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org",
        ];
        lines.extend_from_slice(extra);
        lines.extend(["END:VEVENT", "END:VCALENDAR"]);
        cal(&lines)
    };
    let previous = event(
        "20261210T130000Z",
        &["ATTENDEE;PARTSTAT=ACCEPTED;SCHEDULE-STATUS=1.1:mailto:erin@example.org"],
    );
    let next = event("20261211T130000Z", &[]);
    let d = s
        .scheduler
        .deliver(&change(&s, Some(previous), Some(next)))
        .await
        .expect("deliver");
    let queued = s.recorder.queued();
    assert_eq!(queued.len(), 2, "a CANCEL to Erin, a REQUEST to Bob");
    assert_eq!(queued[0].recipients, vec!["erin@example.org".to_owned()]);
    assert_eq!(queued[1].recipients, vec!["bob@example.org".to_owned()]);
    let body = d.body.expect("copy");
    let out = text(&body);
    assert!(out.contains("SEQUENCE:1"), "{out}");
    let bob = line_of(&body, "ATTENDEE", "bob@example.org").expect("Bob");
    assert!(
        bob.contains("PARTSTAT=NEEDS-ACTION"),
        "answers reset: {bob}"
    );
    assert!(bob.contains("SCHEDULE-STATUS=1.1"), "{bob}");
    assert_eq!(
        d.statuses
            .iter()
            .map(|s| (s.address.as_str(), s.status))
            .collect::<Vec<_>>(),
        vec![
            ("bob@example.org", ScheduleStatus::Sent),
            ("erin@example.org", ScheduleStatus::Sent)
        ]
    );
}

#[tokio::test]
async fn an_answer_through_an_alias_leaves_as_the_alias() {
    let s = setting();
    let invite = |answer: &str| {
        let ada = format!("ATTENDEE;PARTSTAT={answer}:mailto:ada+events@example.com");
        cal(&[
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "PRODID:-//Example//Calendar//EN",
            "BEGIN:VEVENT",
            "UID:fair-1@example.org",
            "DTSTAMP:20261101T080000Z",
            "DTSTART:20261215T100000Z",
            "SEQUENCE:0",
            "SUMMARY:Book fair",
            "ORGANIZER:mailto:frank@example.org",
            &ada,
            "END:VEVENT",
            "END:VCALENDAR",
        ])
    };
    let mut c = change(&s, Some(invite("NEEDS-ACTION")), Some(invite("TENTATIVE")));
    c.own = vec!["ada+events@example.com".into()];
    c.origin = Origin::Web;
    c.references = vec!["invite-1@example.org".into()];
    c.comment = Some("Maybe".into());
    c.reply_as = Some(ReplyAs {
        attendee: "ada+events@example.com".into(),
        route: Route {
            identity: s.identity.clone(),
            from: "ada+events@example.com".into(),
            name: Some("Ada".into()),
            transport: "fake".into(),
            strategy: Strategy::Direct,
            reasons: Vec::new(),
        },
        to: None,
    });
    let d = s.scheduler.deliver(&c).await.expect("deliver");
    assert_eq!(d.way, Way::Mail);
    let queued = s.recorder.queued();
    assert_eq!(queued.len(), 1, "{:?}", d.statuses);
    let message = text(&queued[0].message);
    assert!(message.contains("ada+events@example.com"), "{message}");
    assert!(
        message.contains("In-Reply-To: <invite-1@example.org>"),
        "{message}"
    );
    assert!(message.contains("COMMENT:Maybe"), "{message}");
    assert!(message.contains("PARTSTAT=TENTATIVE"), "{message}");
}

struct Flaky {
    inner: Arc<dyn Submission>,
    calls: std::sync::atomic::AtomicU32,
}

#[async_trait]
impl Submission for Flaky {
    async fn identities(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<Vec<SendIdentity>, SubmissionError> {
        self.inner.identities(actor, archive).await
    }
    async fn limits(
        &self,
        actor: &Actor,
        archive: ArchiveId,
    ) -> Result<SubmissionLimits, SubmissionError> {
        self.inner.limits(actor, archive).await
    }
    async fn sender(
        &self,
        actor: &Actor,
        archive: ArchiveId,
        address: &str,
    ) -> Result<SendIdentity, SubmissionError> {
        self.inner.sender(actor, archive, address).await
    }
    async fn queue(&self, request: Submit) -> Result<Submitted, SubmissionError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            return Err(SubmissionError::Unavailable("store down".into()));
        }
        self.inner.queue(request).await
    }
    async fn undo(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.undo(actor, id).await
    }
    async fn release(
        &self,
        actor: &Actor,
        id: OutboxId,
        undo_window: Option<std::time::Duration>,
    ) -> Result<Submitted, SubmissionError> {
        self.inner.release(actor, id, undo_window).await
    }
    async fn status(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.status(actor, id).await
    }
}

#[tokio::test]
async fn an_unavailable_outbox_leaves_nothing_queued() {
    let world = FakeSubmission::world(Setup::default());
    let recorder = Arc::new(Recorder::new(world.submission));
    let flaky = Arc::new(Flaky {
        inner: recorder.clone(),
        calls: Default::default(),
    });
    let clock = ManualClock::new(holtmail_core::Timestamp::from_secs(1_793_520_000));
    let scheduler = ItipScheduler::new(flaky, clock.clock());
    let previous = cal(&[
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Example//Calendar//EN",
        "BEGIN:VEVENT",
        "UID:dinner-2@example.com",
        "DTSTAMP:20261101T080000Z",
        "DTSTART:20261220T180000Z",
        "SEQUENCE:0",
        "SUMMARY:Dinner",
        "ORGANIZER:mailto:ada@example.com",
        "ATTENDEE:mailto:bob@example.org",
        "ATTENDEE:mailto:erin@example.org",
        "END:VEVENT",
        "END:VCALENDAR",
    ]);
    let next = Bytes::from(
        text(&previous)
            .replace("ATTENDEE:mailto:erin@example.org\r\n", "")
            .replace("SUMMARY:Dinner", "SUMMARY:Dinner at eight"),
    );
    let s = Setting {
        scheduler,
        recorder: recorder.clone(),
        actor: world.sender.clone(),
        archive: world.archive,
        identity: world.identity.to_string(),
    };
    let result = s
        .scheduler
        .deliver(&change(&s, Some(previous), Some(next)))
        .await;
    assert!(
        matches!(result, Err(ScheduleError::Unavailable(_))),
        "{result:?}"
    );
    let queued = recorder.queued();
    assert_eq!(queued.len(), 1, "the first message was queued");
    let status = recorder
        .status(&world.sender, queued[0].id)
        .await
        .expect("status");
    assert_eq!(status.state, SubmissionState::Cancelled, "and taken back");
}

#[tokio::test]
async fn folded_client_lines_are_stripped_and_refolded() {
    let s = setting();
    let long = format!(
        "ATTENDEE;CN=\"{}\";SCHEDULE-STATUS=2.0;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org",
        "Bob Example ".repeat(8).trim()
    );
    let folded: String = {
        let mut out = String::new();
        for (i, c) in long.chars().enumerate() {
            if i > 0 && i % 70 == 0 {
                out.push_str("\r\n ");
            }
            out.push(c);
        }
        out
    };
    let body = cal(&[
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Example//Calendar//EN",
        "BEGIN:VEVENT",
        "UID:long-1@example.com",
        "DTSTAMP:20261101T080000Z",
        "DTSTART:20261220T180000Z",
        "SUMMARY:Long names",
        "ORGANIZER:mailto:ada@example.com",
        &folded,
        "END:VEVENT",
        "END:VCALENDAR",
    ]);
    let prepared = s
        .scheduler
        .prepare(&ClientPut {
            previous: None,
            body,
            own: vec![ADA.into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert!(prepared.stripped);
    let out = text(&prepared.body);
    assert!(!out.contains("SCHEDULE-STATUS"), "{out}");
    assert!(out.split("\r\n").all(|l| l.len() <= 75), "{out}");
    let bob = line_of(&prepared.body, "ATTENDEE", "bob@example.org").expect("Bob");
    assert!(bob.contains("Bob Example Bob Example"), "{bob}");
}

fn reply_mail(attendees: &[&str]) -> IncomingItip {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".into(),
        "PRODID:-//Example//Calendar//EN".into(),
        "METHOD:REPLY".into(),
        "BEGIN:VEVENT".into(),
        "UID:planning-1@example.com".into(),
        "DTSTAMP:20261105T090000Z".into(),
        "SEQUENCE:0".into(),
        "ORGANIZER:mailto:ada@example.com".into(),
    ];
    lines.extend(
        attendees
            .iter()
            .map(|a| format!("ATTENDEE;PARTSTAT=DECLINED:mailto:{a}")),
    );
    lines.extend(["END:VEVENT".into(), "END:VCALENDAR".into()]);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    IncomingItip {
        calendar: cal(&refs),
        from: vec!["reverse-123@alias.example.net".into()],
        signed_sender: Some("bob@example.org".into()),
        recipients: vec![ADA.into()],
        own: vec![ADA.into()],
        arrival: Arrival {
            live: true,
            ..Arrival::default()
        },
        auth: AuthFacts::default(),
        upstream_holds_uid: false,
        organizer_copy: Some(holtmail_schedule::testing::planning(
            "Planning", "ACCEPTED", "",
        )),
        force: false,
    }
}

#[tokio::test]
async fn replies_through_a_signing_alias_service_apply() {
    let s = setting();
    let got = s
        .scheduler
        .incoming(&reply_mail(&["bob@example.org"]))
        .await
        .expect("incoming");
    match got {
        Incoming::Reply {
            outcome: ReplyOutcome::Merged { body },
            ..
        } => {
            let bob = line_of(&body, "ATTENDEE", "bob@example.org").expect("Bob");
            assert!(bob.contains("PARTSTAT=DECLINED"), "{bob}");
        }
        other => panic!("{other:?}"),
    }
    let two = s
        .scheduler
        .incoming(&reply_mail(&["bob@example.org", "carol@example.org"]))
        .await
        .expect("incoming");
    assert_eq!(
        two,
        Incoming::Reply {
            uid: "planning-1@example.com".into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::NotFromAttendee),
        },
        "a reply speaks for one attendee"
    );
    assert!(s.recorder.queued().is_empty());
}

#[tokio::test]
async fn answers_alone_on_a_scheduling_server_are_written_through() {
    let s = setting();
    let mut c = change(
        &s,
        Some(weekly("Standup", None, &[])),
        Some(Bytes::from(text(&weekly("Standup", None, &[])).replace(
            "PARTSTAT=ACCEPTED:mailto:ada",
            "PARTSTAT=TENTATIVE:mailto:ada",
        ))),
    );
    c.host = Host::Graph;
    c.caps = HostCaps {
        respond: true,
        ..HostCaps::default()
    };
    let d = s.scheduler.deliver(&c).await.expect("deliver");
    assert_eq!(d.way, Way::Server);
    assert!(s.recorder.queued().is_empty());
    let body = d.body.expect("copy");
    let organizer = line_of(&body, "ORGANIZER", "bob@example.org").expect("organizer");
    assert!(organizer.contains("SCHEDULE-STATUS=1.1"), "{organizer}");
}

fn fair(answer: &str) -> Bytes {
    let ada = format!("ATTENDEE;PARTSTAT={answer}:mailto:ada@example.com");
    cal(&[
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Example//Calendar//EN",
        "BEGIN:VEVENT",
        "UID:fair-2@example.org",
        "DTSTAMP:20261101T080000Z",
        "DTSTART:20261216T100000Z",
        "SEQUENCE:0",
        "SUMMARY:Book fair",
        "ORGANIZER:mailto:frank@example.org",
        &ada,
        "END:VEVENT",
        "END:VCALENDAR",
    ])
}

#[tokio::test]
async fn an_answer_through_a_reverse_alias_goes_to_the_alias_service() {
    let s = setting();
    let reverse = "frank_at_example.org_abc@alias.example.net";
    let mut c = change(&s, Some(fair("NEEDS-ACTION")), Some(fair("ACCEPTED")));
    c.origin = Origin::Web;
    c.reply_as = Some(ReplyAs {
        attendee: ADA.into(),
        route: Route {
            identity: s.identity.clone(),
            from: ADA.into(),
            name: Some("Ada".into()),
            transport: "fake".into(),
            strategy: Strategy::ReverseAlias,
            reasons: Vec::new(),
        },
        to: Some(reverse.into()),
    });
    let d = s.scheduler.deliver(&c).await.expect("deliver");
    assert_eq!(d.way, Way::Mail);
    assert_eq!(d.statuses.len(), 1);
    assert_eq!(d.statuses[0].address, "frank@example.org");
    let queued = s.recorder.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].recipients, vec![reverse.to_owned()]);
    let message = text(&queued[0].message);
    assert!(!message.contains("To: frank@example.org"), "{message}");
}

#[tokio::test]
async fn a_long_or_broken_reference_chain_never_blocks_an_answer() {
    let s = setting();
    let mut c = change(&s, Some(fair("NEEDS-ACTION")), Some(fair("DECLINED")));
    c.references = vec!["broken id@example.org".into()];
    c.references
        .extend((0..2000).map(|i| format!("thread-{i}@example.org")));
    c.references.push("invite-2@example.org".into());
    let d = s.scheduler.deliver(&c).await.expect("deliver");
    assert_eq!(d.queued.len(), 1, "{:?}", d.statuses);
    let message = text(&s.recorder.queued()[0].message);
    assert!(
        message.contains("In-Reply-To: <invite-2@example.org>"),
        "{message}"
    );
    assert!(!message.contains("broken id"), "{message}");
    assert!(!message.contains("thread-5@example.org"), "{message}");
}

#[tokio::test]
async fn a_withdrawn_message_is_sent_anew_after_a_restart() {
    let world = FakeSubmission::world(Setup::default());
    let recorder = Arc::new(Recorder::new(world.submission));
    let clock = ManualClock::new(holtmail_core::Timestamp::from_secs(1_793_520_000));
    let setting = |scheduler| Setting {
        scheduler,
        recorder: recorder.clone(),
        actor: world.sender.clone(),
        archive: world.archive,
        identity: world.identity.to_string(),
    };
    let first = setting(ItipScheduler::new(recorder.clone(), clock.clock()));
    let c = change(&first, Some(fair("NEEDS-ACTION")), Some(fair("ACCEPTED")));
    let d = first.scheduler.deliver(&c).await.expect("deliver");
    first
        .scheduler
        .withdraw(&first.actor, &d)
        .await
        .expect("withdraw");
    let restarted = setting(ItipScheduler::new(recorder.clone(), clock.clock()));
    let again = restarted.scheduler.deliver(&c).await.expect("deliver");
    assert_eq!(again.queued.len(), 1);
    assert_ne!(again.queued[0], d.queued[0], "not the cancelled message");
    let status = recorder
        .status(&world.sender, again.queued[0])
        .await
        .expect("status");
    assert_eq!(status.state, SubmissionState::Queued);
}

#[tokio::test]
async fn a_stale_organizer_write_keeps_each_instance_answer() {
    let s = setting();
    let served = weekly("Standup", None, DECLINED_INSTANCE);
    let stale = Bytes::from(
        text(&served)
            .replace("SUMMARY:Standup\r\n", "SUMMARY:Standup at ten\r\n")
            .replace("PARTSTAT=DECLINED", "PARTSTAT=NEEDS-ACTION")
            .replace(
                "ATTENDEE;CN=Ada;PARTSTAT=ACCEPTED",
                "ATTENDEE;CN=Ada;PARTSTAT=NEEDS-ACTION",
            ),
    );
    let prepare = |keep_answers| ClientPut {
        previous: Some(served.clone()),
        body: stale.clone(),
        own: vec!["bob@example.org".into()],
        keep_answers,
    };
    let prepared = s.scheduler.prepare(&prepare(true)).await.expect("prepare");
    assert_eq!(prepared.role, Role::Organizer);
    let out = text(&prepared.body);
    assert!(out.contains("SUMMARY:Standup at ten"), "{out}");
    assert!(!out.contains("NEEDS-ACTION"), "{out}");
    assert!(
        out.contains("ATTENDEE;CN=Ada;PARTSTAT=DECLINED:mailto:ada@example.com"),
        "{out}"
    );
    assert!(
        out.contains("ATTENDEE;CN=Ada;PARTSTAT=ACCEPTED:mailto:ada@example.com"),
        "{out}"
    );
    let plain = s.scheduler.prepare(&prepare(false)).await.expect("prepare");
    assert_eq!(
        plain.body, stale,
        "without a matching tag the write is kept"
    );
}
