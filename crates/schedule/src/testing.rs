//! Conformance suite every [`Scheduler`] implementation runs, and an in-memory fake.
//!
//! The suite runs a scheduler over [`FakeSubmission`] (wrapped in a [`Recorder`] that keeps
//! every message queued) and a clock it moves. Fixtures are single events of invented people:
//! Ada (`ada@example.com`, the sending identity of the fake submission) organizes `planning`
//! with Bob, Carol (`SCHEDULE-AGENT=CLIENT`) and Dave (`SCHEDULE-AGENT=NONE`); Bob organizes
//! `review` with Ada as attendee.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::{ArchiveId, IdentityId, OutboxId, Timestamp};
use holtmail_route::{Route, Strategy};
use holtmail_submission::testing::{
    Driver, FakeSubmission, IDENTITY_ADDRESS, Setup as SubmissionSetup,
};
use holtmail_submission::{
    Actor, SendIdentity, Submission, SubmissionError, SubmissionLimits, SubmissionState, Submit,
    Submitted,
};

use crate::{
    Arrival, AuthFacts, Change, ClientPut, Clock, Delivery, FromMail, Host, HostCaps, Ignored,
    Incoming, IncomingItip, ItipKey, ItipMethod, Origin, Prepared, RECENT, RecipientStatus,
    ReplyAs, ReplyOutcome, Role, ScheduleError, ScheduleStatus, Scheduler, Unapplied, Way,
};

/// Bob, an attendee of Ada's event and the organizer of `review`.
pub const BOB: &str = "bob@example.org";
/// Carol, an attendee whose client schedules for her (`SCHEDULE-AGENT=CLIENT`).
pub const CAROL: &str = "carol@example.org";
/// Dave, an attendee nobody tells (`SCHEDULE-AGENT=NONE`).
pub const DAVE: &str = "dave@example.org";
/// The `UID` of Ada's event.
pub const PLANNING: &str = "planning-1@example.com";
/// The `UID` of Bob's event.
pub const REVIEW: &str = "review-1@example.org";

/// What a suite instance allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    /// Messages the sender may submit per hour.
    pub messages_per_hour: u32,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            messages_per_hour: 1000,
        }
    }
}

/// Builds the scheduler under test.
pub trait Harness: Send + Sync {
    /// A scheduler that submits through `submission` and reads time from `clock`.
    fn scheduler(&self, submission: Arc<dyn Submission>, clock: Clock) -> Arc<dyn Scheduler>;
}

/// A message a [`Recorder`] saw queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    /// Its outbox id.
    pub id: OutboxId,
    /// The envelope recipients.
    pub recipients: Vec<String>,
    /// The bytes.
    pub message: Bytes,
}

impl Queued {
    /// The iTIP method of its `text/calendar` part, from the media type's `method` parameter.
    pub fn method(&self) -> Option<ItipMethod> {
        let text = String::from_utf8_lossy(&self.message).to_ascii_lowercase();
        let at = text.find("text/calendar")?;
        let rest = &text[at..];
        let start = rest.find("method=")? + 7;
        let value: String = rest[start..]
            .trim_start_matches('"')
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        ItipMethod::parse(&value)
    }
}

/// A [`Submission`] that hands everything to an inner one and records each new message queued.
pub struct Recorder {
    inner: Arc<dyn Submission>,
    queued: Mutex<Vec<Queued>>,
}

impl Recorder {
    /// Records what is queued through `inner`.
    pub fn new(inner: Arc<dyn Submission>) -> Self {
        Self {
            inner,
            queued: Mutex::new(Vec::new()),
        }
    }

    /// Every new message queued so far, in order.
    pub fn queued(&self) -> Vec<Queued> {
        self.queued.lock().expect("recorder").clone()
    }
}

#[async_trait]
impl Submission for Recorder {
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
        let recipients = request.recipients.clone();
        let message = request.message.clone();
        let submitted = self.inner.queue(request).await?;
        let mut queued = self.queued.lock().expect("recorder");
        if !queued.iter().any(|q| q.id == submitted.id) {
            queued.push(Queued {
                id: submitted.id,
                recipients,
                message,
            });
        }
        Ok(submitted)
    }

    async fn undo(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.undo(actor, id).await
    }

    async fn release(
        &self,
        actor: &Actor,
        id: OutboxId,
        undo_window: Option<Duration>,
    ) -> Result<Submitted, SubmissionError> {
        self.inner.release(actor, id, undo_window).await
    }

    async fn status(&self, actor: &Actor, id: OutboxId) -> Result<Submitted, SubmissionError> {
        self.inner.status(actor, id).await
    }
}

/// A clock the suite moves by hand.
pub struct ManualClock(AtomicI64);

impl ManualClock {
    /// A clock standing at `start`.
    pub fn new(start: Timestamp) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(start.as_millis())))
    }

    /// Moves it forward.
    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_millis() as i64, Ordering::SeqCst);
    }

    /// The [`Clock`] reading it.
    pub fn clock(self: &Arc<Self>) -> Clock {
        let me = self.clone();
        Arc::new(move || Timestamp::from_millis(me.0.load(Ordering::SeqCst)))
    }
}

/// One suite instance.
pub struct World {
    /// The scheduler under test.
    pub scheduler: Arc<dyn Scheduler>,
    /// What it queued.
    pub recorder: Arc<Recorder>,
    /// Its clock.
    pub clock: Arc<ManualClock>,
    /// The archive of the calendars.
    pub archive: ArchiveId,
    /// Ada on a device, who may send as `ada@example.com`.
    pub ada: Actor,
    /// Someone who may not send as anyone.
    pub stranger: Actor,
    /// The id of Ada's identity.
    pub identity: IdentityId,
    /// Hands a released message to its transport, as the real queue would.
    pub driver: Arc<dyn Driver>,
}

impl World {
    /// Starts an instance of `harness`'s scheduler.
    pub fn start(harness: &dyn Harness, setup: Setup) -> Self {
        let submission = FakeSubmission::world(SubmissionSetup {
            messages_per_hour: setup.messages_per_hour,
            ..SubmissionSetup::default()
        });
        let recorder = Arc::new(Recorder::new(submission.submission));
        let clock = ManualClock::new(Timestamp::from_secs(1_793_520_000));
        let scheduler = harness.scheduler(recorder.clone(), clock.clock());
        Self {
            scheduler,
            recorder,
            clock,
            archive: submission.archive,
            ada: submission.sender,
            stranger: submission.stranger,
            identity: submission.identity,
            driver: submission.driver,
        }
    }

    fn change(&self, previous: Option<Bytes>, next: Option<Bytes>) -> Change {
        Change {
            actor: self.ada.clone(),
            archive: self.archive,
            origin: Origin::Device,
            host: PLAIN,
            caps: ALL,
            own: vec![IDENTITY_ADDRESS.to_uppercase()],
            previous,
            next,
            reply_as: None,
            references: Vec::new(),
            comment: None,
        }
    }

    fn incoming(&self, calendar: Bytes, from: &str) -> IncomingItip {
        IncomingItip {
            calendar,
            from: vec![from.into()],
            signed_sender: None,
            recipients: vec![IDENTITY_ADDRESS.into()],
            own: vec![IDENTITY_ADDRESS.into()],
            arrival: Arrival {
                live: true,
                ..Arrival::default()
            },
            auth: AuthFacts::default(),
            upstream_holds_uid: false,
            organizer_copy: None,
            force: false,
        }
    }
}

const PLAIN: Host = Host::CalDav {
    auto_schedule: false,
};
const SCHEDULING: Host = Host::CalDav {
    auto_schedule: true,
};
const ALL: HostCaps = HostCaps {
    create: true,
    update: true,
    delete: true,
    respond: true,
};

fn ics(lines: &[String]) -> Bytes {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\r\n");
    }
    Bytes::from(out)
}

/// Ada's event: `summary`, Bob's answer `bob`, and `extra` lines on Bob's attendee entry.
pub fn planning(summary: &str, bob: &str, bob_extra: &str) -> Bytes {
    ics(&[
        "BEGIN:VCALENDAR".into(),
        "VERSION:2.0".into(),
        "PRODID:-//Example//Calendar//EN".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{PLANNING}"),
        "DTSTAMP:20261101T090000Z".into(),
        "DTSTART:20261201T100000Z".into(),
        "DTEND:20261201T110000Z".into(),
        "SEQUENCE:0".into(),
        format!("SUMMARY:{summary}"),
        format!("ORGANIZER;CN=Ada:mailto:{IDENTITY_ADDRESS}"),
        format!("ATTENDEE;CN=Ada;PARTSTAT=ACCEPTED:mailto:{IDENTITY_ADDRESS}"),
        format!("ATTENDEE;CN=Bob;PARTSTAT={bob}{bob_extra}:mailto:{BOB}"),
        format!("ATTENDEE;CN=Carol;SCHEDULE-AGENT=CLIENT;PARTSTAT=NEEDS-ACTION:mailto:{CAROL}"),
        format!("ATTENDEE;CN=Dave;SCHEDULE-AGENT=NONE;PARTSTAT=NEEDS-ACTION:mailto:{DAVE}"),
        "END:VEVENT".into(),
        "END:VCALENDAR".into(),
    ])
}

/// Bob's event inviting Ada: `summary`, Ada's answer `ada`, parameters `organizer_extra` on
/// the organizer, and `personal` lines (such as an alarm) at the end of the event.
pub fn review(summary: &str, ada: &str, organizer_extra: &str, personal: &[&str]) -> Bytes {
    let mut lines: Vec<String> = vec![
        "BEGIN:VCALENDAR".into(),
        "VERSION:2.0".into(),
        "PRODID:-//Example//Calendar//EN".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{REVIEW}"),
        "DTSTAMP:20261102T090000Z".into(),
        "DTSTART:20261203T140000Z".into(),
        "DTEND:20261203T150000Z".into(),
        "SEQUENCE:2".into(),
        format!("SUMMARY:{summary}"),
        format!("ORGANIZER;CN=Bob{organizer_extra}:mailto:{BOB}"),
        format!("ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:{BOB}"),
        format!("ATTENDEE;CN=Ada;PARTSTAT={ada};RSVP=TRUE:mailto:{IDENTITY_ADDRESS}"),
    ];
    lines.extend(personal.iter().map(|l| (*l).to_owned()));
    lines.extend(["END:VEVENT".into(), "END:VCALENDAR".into()]);
    ics(&lines)
}

fn reply_from(attendee: &str, answer: &str, method: &str) -> Bytes {
    ics(&[
        "BEGIN:VCALENDAR".into(),
        "VERSION:2.0".into(),
        "PRODID:-//Example//Calendar//EN".into(),
        format!("METHOD:{method}"),
        "BEGIN:VEVENT".into(),
        format!("UID:{PLANNING}"),
        "DTSTAMP:20261105T090000Z".into(),
        "DTSTART:20261201T100000Z".into(),
        "SEQUENCE:0".into(),
        format!("ORGANIZER:mailto:{IDENTITY_ADDRESS}"),
        format!("ATTENDEE;PARTSTAT={answer}:mailto:{attendee}"),
        "END:VEVENT".into(),
        "END:VCALENDAR".into(),
    ])
}

fn with_method(raw: &Bytes, method: &str) -> Bytes {
    let text = String::from_utf8_lossy(raw).replacen(
        "VERSION:2.0\r\n",
        &format!("VERSION:2.0\r\nMETHOD:{method}\r\n"),
        1,
    );
    Bytes::from(text)
}

/// The unfolded content lines of an iCalendar object.
pub fn unfolded(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.push_str(rest);
            }
        } else if !line.is_empty() {
            out.push(line.to_owned());
        }
    }
    out
}

/// The unfolded line of the `name` property (`ATTENDEE` or `ORGANIZER`) for `address`.
pub fn line_of(raw: &[u8], name: &str, address: &str) -> Option<String> {
    unfolded(raw).into_iter().find(|l| {
        let upper = l.to_ascii_uppercase();
        (upper.starts_with(&format!("{name};")) || upper.starts_with(&format!("{name}:")))
            && upper.ends_with(&format!("MAILTO:{}", address.to_ascii_uppercase()))
    })
}

fn status_of(raw: &[u8], name: &str, address: &str) -> Option<String> {
    let line = line_of(raw, name, address)?.to_ascii_uppercase();
    let at = line.find("SCHEDULE-STATUS=")? + 16;
    Some(
        line[at..]
            .trim_start_matches('"')
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect(),
    )
}

fn statuses(pairs: &[(&str, ScheduleStatus)]) -> Vec<RecipientStatus> {
    pairs
        .iter()
        .map(|(a, s)| RecipientStatus {
            address: (*a).to_owned(),
            status: *s,
        })
        .collect()
}

fn delivered(result: Result<Delivery, ScheduleError>, what: &str) -> Delivery {
    match result {
        Ok(d) => d,
        Err(e) => panic!("{what}: {e}"),
    }
}

/// Checks that `harness`'s schedulers keep the [`Scheduler`] contract. Panics on the first
/// violation.
pub async fn conformance(harness: &dyn Harness) {
    prepare_strips(harness).await;
    prepare_attendee(harness).await;
    organizer_mails(harness).await;
    duplicates(harness).await;
    no_identity(harness).await;
    rate_limited(harness).await;
    scheduling_servers(harness).await;
    quiet_changes(harness).await;
    organizer_delete(harness).await;
    attendee_answers(harness).await;
    withdraw(harness).await;
    held_until_confirmed(harness).await;
    answers_through_an_alias(harness).await;
    prepare_keeps_answers(harness).await;
    incoming_ignored(harness).await;
    incoming_invitations(harness).await;
    incoming_replies(harness).await;
}

async fn prepare_strips(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let body = planning(
        "Planning",
        "NEEDS-ACTION",
        ";SCHEDULE-STATUS=2.0;SCHEDULE-FORCE-SEND=REQUEST",
    );
    let prepared = world
        .scheduler
        .prepare(&ClientPut {
            previous: None,
            body,
            own: vec![IDENTITY_ADDRESS.into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert_eq!(prepared.role, Role::Organizer, "organizer's role");
    assert!(
        prepared.stripped,
        "client scheduling parameters are stripped"
    );
    assert!(!prepared.reduced, "an organizer's write is not reduced");
    let text = String::from_utf8_lossy(&prepared.body).to_ascii_uppercase();
    assert!(
        !text.contains("SCHEDULE-STATUS"),
        "no client SCHEDULE-STATUS"
    );
    assert!(
        !text.contains("SCHEDULE-FORCE-SEND"),
        "no SCHEDULE-FORCE-SEND"
    );
    assert!(
        text.contains("SCHEDULE-AGENT=CLIENT"),
        "SCHEDULE-AGENT stays"
    );

    let clean = planning("Planning", "NEEDS-ACTION", "");
    let same = world
        .scheduler
        .prepare(&ClientPut {
            previous: None,
            body: clean.clone(),
            own: vec![IDENTITY_ADDRESS.into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert_eq!(same.body, clean, "nothing to strip keeps the bytes");
    assert!(!same.stripped);

    let stranger = world
        .scheduler
        .prepare(&ClientPut {
            previous: None,
            body: clean,
            own: vec!["eve@example.net".into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert_eq!(stranger.role, Role::None, "not the user's event");

    let broken = world
        .scheduler
        .prepare(&ClientPut {
            previous: None,
            body: Bytes::from_static(b"not a calendar"),
            own: vec![IDENTITY_ADDRESS.into()],
            keep_answers: false,
        })
        .await;
    assert!(
        matches!(broken, Err(ScheduleError::Invalid(_))),
        "unreadable bytes: {broken:?}"
    );
}

async fn prepare_attendee(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let previous = review("Review", "NEEDS-ACTION", "", &[]);
    let alarm = [
        "BEGIN:VALARM",
        "ACTION:DISPLAY",
        "DESCRIPTION:Reminder",
        "TRIGGER:-PT15M",
        "END:VALARM",
    ];
    let allowed = review("Review", "ACCEPTED", "", &alarm);
    let prepared = world
        .scheduler
        .prepare(&ClientPut {
            previous: Some(previous.clone()),
            body: allowed.clone(),
            own: vec![IDENTITY_ADDRESS.into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert_eq!(prepared.role, Role::Attendee);
    assert!(!prepared.reduced, "an answer and an alarm are allowed");
    assert_eq!(prepared.body, allowed, "an allowed write is kept as sent");

    let renamed = review("Renamed by Ada", "ACCEPTED", "", &alarm);
    let reduced = world
        .scheduler
        .prepare(&ClientPut {
            previous: Some(previous),
            body: renamed,
            own: vec![IDENTITY_ADDRESS.into()],
            keep_answers: false,
        })
        .await
        .expect("prepare");
    assert!(reduced.reduced, "a new title is not the attendee's to make");
    let text = String::from_utf8_lossy(&reduced.body);
    assert!(text.contains("SUMMARY:Review"), "the title stays: {text}");
    assert!(!text.contains("Renamed by Ada"), "the new title is dropped");
    let mine = line_of(&reduced.body, "ATTENDEE", IDENTITY_ADDRESS).expect("Ada's entry");
    assert!(
        mine.contains("PARTSTAT=ACCEPTED"),
        "the answer stays: {mine}"
    );
    assert!(text.contains("TRIGGER:-PT15M"), "the alarm stays: {text}");
}

async fn organizer_mails(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let next = planning("Planning", "NEEDS-ACTION", "");
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(None, Some(next.clone())))
            .await,
        "create",
    );
    assert_eq!(d.role, Role::Organizer);
    assert_eq!(d.way, Way::Mail, "a calendar without a scheduling server");
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::Sent)]));
    assert_eq!(d.queued.len(), 1, "one REQUEST");
    let queued = world.recorder.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].recipients, vec![BOB.to_owned()], "only Bob");
    assert_eq!(queued[0].method(), Some(ItipMethod::Request));
    let body = d.body.expect("a copy to record");
    assert!(d.rewritten, "SCHEDULE-STATUS was written");
    assert_eq!(
        status_of(&body, "ATTENDEE", BOB).as_deref(),
        Some("1.1"),
        "Bob's status"
    );
    assert_eq!(
        status_of(&body, "ATTENDEE", CAROL),
        None,
        "Carol's client tells her"
    );
    assert_eq!(
        status_of(&body, "ATTENDEE", DAVE),
        None,
        "nobody tells Dave"
    );

    let mut web = world.change(None, Some(next));
    web.origin = Origin::Itip;
    let d = delivered(world.scheduler.deliver(&web).await, "itip origin");
    assert_eq!(
        d.way,
        Way::Nothing,
        "the scheduler's own changes send nothing"
    );
    assert!(d.queued.is_empty() && !d.rewritten);
    assert_eq!(world.recorder.queued().len(), 1);
}

async fn duplicates(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let change = world.change(None, Some(planning("Planning", "NEEDS-ACTION", "")));
    delivered(world.scheduler.deliver(&change).await, "first");
    let key = planning_key();
    let bob = [BOB.to_uppercase()];
    assert!(
        world
            .scheduler
            .just_sent(world.archive, &key, &bob)
            .await
            .expect("just_sent"),
        "the REQUEST was just sent"
    );
    assert!(
        !world
            .scheduler
            .just_sent(ArchiveId::new(), &key, &bob)
            .await
            .expect("just_sent"),
        "per archive"
    );
    assert!(
        !world
            .scheduler
            .just_sent(world.archive, &key, &[BOB.into(), CAROL.into()])
            .await
            .expect("just_sent"),
        "Carol was not sent it"
    );
    assert!(
        !world
            .scheduler
            .just_sent(world.archive, &key, &[])
            .await
            .expect("just_sent"),
        "nobody"
    );
    let instance = ItipKey {
        recurrence_id: Some("20261208T100000Z".into()),
        ..key.clone()
    };
    assert!(
        !world
            .scheduler
            .just_sent(world.archive, &instance, &bob)
            .await
            .expect("just_sent"),
        "another instance"
    );
    let again = delivered(world.scheduler.deliver(&change).await, "again");
    assert!(
        again.queued.is_empty(),
        "the same message is not queued twice"
    );
    assert_eq!(again.suppressed, 1);
    assert_eq!(again.statuses, statuses(&[(BOB, ScheduleStatus::Sent)]));
    assert_eq!(world.recorder.queued().len(), 1);

    world.clock.advance(RECENT + Duration::from_secs(1));
    assert!(
        !world
            .scheduler
            .just_sent(world.archive, &key, &bob)
            .await
            .expect("just_sent"),
        "only within the window"
    );
    let later = delivered(world.scheduler.deliver(&change).await, "later");
    assert_eq!(later.queued.len(), 1, "sent again after the window");
    assert_eq!(world.recorder.queued().len(), 2);

    world
        .recorder
        .undo(&world.ada, later.queued[0])
        .await
        .expect("undone in the outbox");
    let anew = delivered(world.scheduler.deliver(&change).await, "after an undo");
    assert_eq!(anew.suppressed, 0, "an undone message was not sent");
    assert_eq!(anew.queued.len(), 1, "queued anew");
    assert_ne!(anew.queued[0], later.queued[0]);
}

fn planning_key() -> ItipKey {
    ItipKey {
        uid: PLANNING.into(),
        sequence: 0,
        method: ItipMethod::Request,
        recurrence_id: None,
        partstat: None,
    }
}

async fn held_until_confirmed(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(None, Some(planning("Planning", "NEEDS-ACTION", ""))))
            .await,
        "create",
    );
    let id = d.queued[0];
    world.driver.deliver(id).await;
    assert_eq!(
        world
            .recorder
            .status(&world.ada, id)
            .await
            .expect("status")
            .state,
        SubmissionState::Queued,
        "nothing leaves before the change is confirmed"
    );
    world
        .scheduler
        .confirm(&world.ada, &d)
        .await
        .expect("confirm");
    world
        .scheduler
        .confirm(&world.ada, &d)
        .await
        .expect("confirm twice");
    world.driver.deliver(id).await;
    assert_eq!(
        world
            .recorder
            .status(&world.ada, id)
            .await
            .expect("status")
            .state,
        SubmissionState::Sent,
        "a confirmed message leaves"
    );
    world
        .scheduler
        .withdraw(&world.ada, &d)
        .await
        .expect("withdrawing what left");
    assert!(
        world
            .scheduler
            .just_sent(world.archive, &planning_key(), &[BOB.into()])
            .await
            .expect("just_sent"),
        "a message that left still counts as sent"
    );
}

async fn answers_through_an_alias(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let reverse = "bob.example.org_reply@alias.example.net";
    let mut change = world.change(
        Some(review("Review", "NEEDS-ACTION", "", &[])),
        Some(review("Review", "ACCEPTED", "", &[])),
    );
    change.origin = Origin::Web;
    change.reply_as = Some(ReplyAs {
        attendee: IDENTITY_ADDRESS.into(),
        route: Route {
            identity: world.identity.to_string(),
            from: IDENTITY_ADDRESS.into(),
            name: Some("Ada".into()),
            transport: "transport".into(),
            strategy: Strategy::ReverseAlias,
            reasons: Vec::new(),
        },
        to: Some(reverse.into()),
    });
    let d = delivered(world.scheduler.deliver(&change).await, "alias answer");
    assert_eq!(d.way, Way::Mail);
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::Sent)]));
    let queued = world.recorder.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].recipients,
        vec![reverse.to_owned()],
        "the answer goes to the reverse alias, never straight to the organizer"
    );
    assert_eq!(queued[0].method(), Some(ItipMethod::Reply));
}

async fn prepare_keeps_answers(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let served = planning("Planning", "ACCEPTED", "");
    let stale = planning("Planning moved", "NEEDS-ACTION", "");
    let put = |keep_answers| ClientPut {
        previous: Some(served.clone()),
        body: stale.clone(),
        own: vec![IDENTITY_ADDRESS.into()],
        keep_answers,
    };
    let kept = world.scheduler.prepare(&put(true)).await.expect("prepare");
    let bob = line_of(&kept.body, "ATTENDEE", BOB).expect("Bob");
    assert!(
        bob.to_ascii_uppercase().contains("PARTSTAT=ACCEPTED"),
        "a matching Schedule-Tag keeps Bob's merged answer: {bob}"
    );
    assert!(
        String::from_utf8_lossy(&kept.body).contains("SUMMARY:Planning moved"),
        "the organizer's edit stays"
    );
    let plain = world.scheduler.prepare(&put(false)).await.expect("prepare");
    let bob = line_of(&plain.body, "ATTENDEE", BOB).expect("Bob");
    assert!(
        bob.to_ascii_uppercase().contains("PARTSTAT=NEEDS-ACTION"),
        "without the tag the client's copy is kept: {bob}"
    );
}

async fn no_identity(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let mut change = world.change(None, Some(planning("Planning", "NEEDS-ACTION", "")));
    change.actor = world.stranger.clone();
    let d = delivered(world.scheduler.deliver(&change).await, "stranger");
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::NoRoute)]));
    assert!(d.queued.is_empty());
    assert!(world.recorder.queued().is_empty(), "nothing queued");
    let body = d.body.expect("copy");
    assert_eq!(status_of(&body, "ATTENDEE", BOB).as_deref(), Some("3.7"));
}

async fn rate_limited(harness: &dyn Harness) {
    let world = World::start(
        harness,
        Setup {
            messages_per_hour: 0,
        },
    );
    let change = world.change(None, Some(planning("Planning", "NEEDS-ACTION", "")));
    let d = delivered(world.scheduler.deliver(&change).await, "limited");
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::Failed)]));
    assert!(world.recorder.queued().is_empty(), "the limits hold");
}

async fn scheduling_servers(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let next = planning("Planning", "NEEDS-ACTION", "");
    let mut change = world.change(None, Some(next.clone()));
    change.host = SCHEDULING;
    let d = delivered(world.scheduler.deliver(&change).await, "server");
    assert_eq!(d.way, Way::Server);
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::Sent)]));
    assert!(world.recorder.queued().is_empty(), "the server sends");

    change.caps = HostCaps::default();
    let d = delivered(world.scheduler.deliver(&change).await, "deferred");
    assert_eq!(d.way, Way::Deferred);
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::Pending)]));
    assert!(world.recorder.queued().is_empty(), "the server sends later");

    change.caps = ALL;
    change.origin = Origin::Web;
    let d = delivered(world.scheduler.deliver(&change).await, "web");
    assert_eq!(
        d.statuses,
        statuses(&[(BOB, ScheduleStatus::Sent), (CAROL, ScheduleStatus::Sent)]),
        "the web app mails Carol, whom the server never tells"
    );
    let queued = world.recorder.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].recipients, vec![CAROL.to_owned()]);

    let mut graph = world.change(None, Some(next));
    graph.host = Host::Graph;
    let d = delivered(world.scheduler.deliver(&graph).await, "graph");
    assert_eq!(d.way, Way::Server);
}

async fn quiet_changes(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let previous = planning("Planning", "NEEDS-ACTION", "");
    let answered = planning("Planning", "ACCEPTED", "");
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(Some(previous), Some(answered.clone())))
            .await,
        "answers",
    );
    assert_eq!(d.way, Way::Nothing, "answers alone send nothing");
    assert!(d.statuses.is_empty());
    assert_eq!(d.body.as_ref(), Some(&answered), "the copy is kept");
    assert!(!d.rewritten);

    let with_status = planning("Planning", "ACCEPTED", ";SCHEDULE-STATUS=1.1");
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(Some(with_status), Some(answered)))
            .await,
        "status kept",
    );
    let body = d.body.expect("copy");
    assert_eq!(
        status_of(&body, "ATTENDEE", BOB).as_deref(),
        Some("1.1"),
        "an attendee the change does not concern keeps its status"
    );
    assert!(world.recorder.queued().is_empty());
}

async fn organizer_delete(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let previous = planning("Planning", "ACCEPTED", "");
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(Some(previous), None))
            .await,
        "delete",
    );
    assert_eq!(d.body, None);
    assert_eq!(d.way, Way::Mail);
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::Sent)]));
    let queued = world.recorder.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].method(), Some(ItipMethod::Cancel));
    assert_eq!(queued[0].recipients, vec![BOB.to_owned()]);
}

async fn attendee_answers(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let previous = review("Review", "NEEDS-ACTION", "", &[]);
    let next = review("Review", "ACCEPTED", "", &[]);
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(Some(previous.clone()), Some(next)))
            .await,
        "answer",
    );
    assert_eq!(d.role, Role::Attendee);
    assert_eq!(d.way, Way::Mail);
    assert_eq!(d.statuses, statuses(&[(BOB, ScheduleStatus::Sent)]));
    let queued = world.recorder.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].method(), Some(ItipMethod::Reply));
    assert_eq!(queued[0].recipients, vec![BOB.to_owned()]);
    let body = d.body.expect("copy");
    assert_eq!(status_of(&body, "ORGANIZER", BOB).as_deref(), Some("1.1"));

    let quiet_prev = review("Review", "NEEDS-ACTION", ";SCHEDULE-AGENT=NONE", &[]);
    let quiet_next = review("Review", "DECLINED", ";SCHEDULE-AGENT=NONE", &[]);
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(Some(quiet_prev), Some(quiet_next)))
            .await,
        "quiet",
    );
    assert_eq!(d.way, Way::Quiet);
    assert!(d.statuses.is_empty() && d.queued.is_empty());

    let mut server = world.change(
        Some(previous.clone()),
        Some(review("Review", "TENTATIVE", "", &[])),
    );
    server.host = SCHEDULING;
    let d = delivered(world.scheduler.deliver(&server).await, "server answer");
    assert_eq!(d.way, Way::Server, "the server tells the organizer");
    assert!(d.queued.is_empty());

    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(Some(previous), None))
            .await,
        "attendee delete",
    );
    assert_eq!(d.way, Way::Mail, "deleting an invitation declines it");
    let queued = world.recorder.queued();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[1].method(), Some(ItipMethod::Reply));
}

async fn withdraw(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let d = delivered(
        world
            .scheduler
            .deliver(&world.change(None, Some(planning("Planning", "NEEDS-ACTION", ""))))
            .await,
        "create",
    );
    world
        .scheduler
        .withdraw(&world.ada, &d)
        .await
        .expect("withdraw");
    world
        .scheduler
        .withdraw(&world.ada, &d)
        .await
        .expect("withdraw twice");
    for id in &d.queued {
        let status = world
            .recorder
            .status(&world.ada, *id)
            .await
            .expect("status");
        assert_eq!(status.state, SubmissionState::Cancelled, "withdrawn");
    }
    assert!(
        !world
            .scheduler
            .just_sent(world.archive, &planning_key(), &[BOB.into()])
            .await
            .expect("just_sent"),
        "a withdrawn message was not sent"
    );
}

async fn incoming_ignored(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let request = with_method(&review("Review", "NEEDS-ACTION", "", &[]), "REQUEST");
    type Edit = Box<dyn Fn(&mut IncomingItip)>;
    let cases: [(&str, Edit, Ignored); 6] = [
        (
            "backfill",
            Box::new(|m| m.arrival.live = false),
            Ignored::NotLive,
        ),
        (
            "sent folder",
            Box::new(|m| m.arrival.sent_folder = true),
            Ignored::SentFolder,
        ),
        (
            "outbox copy",
            Box::new(|m| m.arrival.outbox_copy = true),
            Ignored::OutboxCopy,
        ),
        (
            "own sender",
            Box::new(|m| m.from = vec![IDENTITY_ADDRESS.to_uppercase()]),
            Ignored::OwnSender,
        ),
        (
            "not itip",
            Box::new(|m| {
                m.calendar = with_method(&review("Review", "NEEDS-ACTION", "", &[]), "PUBLISH")
            }),
            Ignored::NotItip,
        ),
        (
            "not addressed",
            Box::new(|m| m.recipients = vec!["list@example.net".into()]),
            Ignored::NotAddressed,
        ),
    ];
    for (what, edit, expected) in cases {
        let mut mail = world.incoming(request.clone(), BOB);
        edit(&mut mail);
        let got = world.scheduler.incoming(&mail).await.expect("incoming");
        assert_eq!(got, Incoming::Ignored(expected), "{what}");
    }
}

async fn incoming_invitations(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let request = with_method(&review("Review", "NEEDS-ACTION", "", &[]), "REQUEST");
    let mail = world.incoming(request.clone(), BOB);
    assert_eq!(
        world.scheduler.incoming(&mail).await.expect("incoming"),
        Incoming::Invitation {
            method: ItipMethod::Request,
            uid: REVIEW.into(),
            from_mail: FromMail::Update,
        }
    );
    let mut held = mail.clone();
    held.upstream_holds_uid = true;
    assert_eq!(
        world.scheduler.incoming(&held).await.expect("incoming"),
        Incoming::Invitation {
            method: ItipMethod::Request,
            uid: REVIEW.into(),
            from_mail: FromMail::UpstreamHolds,
        }
    );
    let cancel = world.incoming(
        with_method(&review("Review", "NEEDS-ACTION", "", &[]), "CANCEL"),
        BOB,
    );
    assert!(matches!(
        world.scheduler.incoming(&cancel).await.expect("incoming"),
        Incoming::Invitation {
            method: ItipMethod::Cancel,
            from_mail: FromMail::Update,
            ..
        }
    ));
    let mut alias = world.incoming(
        with_method(&planning("Planning", "NEEDS-ACTION", ""), "REQUEST"),
        "relay@alias.example.net",
    );
    alias.recipients = vec![IDENTITY_ADDRESS.into()];
    assert_eq!(
        world.scheduler.incoming(&alias).await.expect("incoming"),
        Incoming::Invitation {
            method: ItipMethod::Request,
            uid: PLANNING.into(),
            from_mail: FromMail::OwnOrganizer,
        },
        "an invitation to an own alias is not applied back"
    );
    assert!(world.recorder.queued().is_empty(), "incoming sends nothing");
}

async fn incoming_replies(harness: &dyn Harness) {
    let world = World::start(harness, Setup::default());
    let copy = planning("Planning", "NEEDS-ACTION", "");
    let mut mail = world.incoming(reply_from(BOB, "ACCEPTED", "REPLY"), BOB);
    mail.organizer_copy = Some(copy.clone());

    let unsigned = world.scheduler.incoming(&mail).await.expect("incoming");
    assert_eq!(
        unsigned,
        Incoming::Reply {
            uid: PLANNING.into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::Unauthenticated),
        },
        "a REPLY without DKIM or DMARC stays unapplied"
    );

    mail.auth.dkim_pass = vec!["example.org".into()];
    match world.scheduler.incoming(&mail).await.expect("incoming") {
        Incoming::Reply {
            outcome: ReplyOutcome::Merged { body },
            ..
        } => {
            let bob = line_of(&body, "ATTENDEE", BOB).expect("Bob's entry");
            assert!(bob.contains("PARTSTAT=ACCEPTED"), "merged: {bob}");
        }
        other => panic!("signed reply: {other:?}"),
    }

    let mut forced = mail.clone();
    forced.auth = AuthFacts::default();
    forced.force = true;
    assert!(matches!(
        world.scheduler.incoming(&forced).await.expect("incoming"),
        Incoming::Reply {
            outcome: ReplyOutcome::Merged { .. },
            ..
        }
    ));

    let mut dmarc = mail.clone();
    dmarc.auth = AuthFacts {
        dkim_pass: vec!["mailer.example.net".into()],
        dmarc_pass: vec!["example.org".into()],
    };
    assert!(matches!(
        world.scheduler.incoming(&dmarc).await.expect("incoming"),
        Incoming::Reply {
            outcome: ReplyOutcome::Merged { .. },
            ..
        }
    ));

    let mut foreign_dmarc = mail.clone();
    foreign_dmarc.from = vec!["me@attacker.example.net".into(), BOB.into()];
    foreign_dmarc.auth = AuthFacts {
        dkim_pass: Vec::new(),
        dmarc_pass: vec!["attacker.example.net".into()],
    };
    assert_eq!(
        world
            .scheduler
            .incoming(&foreign_dmarc)
            .await
            .expect("incoming"),
        Incoming::Reply {
            uid: PLANNING.into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::Unauthenticated),
        },
        "DMARC for another domain, and several From addresses, prove nothing"
    );

    let mut foreign_dkim = mail.clone();
    foreign_dkim.auth.dkim_pass = vec!["example.net".into()];
    assert_eq!(
        world
            .scheduler
            .incoming(&foreign_dkim)
            .await
            .expect("incoming"),
        Incoming::Reply {
            uid: PLANNING.into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::Unauthenticated),
        },
        "a signature of another domain proves nothing"
    );

    let mut spoof = mail.clone();
    spoof.from = vec![CAROL.into()];
    assert_eq!(
        world.scheduler.incoming(&spoof).await.expect("incoming"),
        Incoming::Reply {
            uid: PLANNING.into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::NotFromAttendee),
        }
    );

    let mut no_copy = mail.clone();
    no_copy.organizer_copy = None;
    assert_eq!(
        world.scheduler.incoming(&no_copy).await.expect("incoming"),
        Incoming::Reply {
            uid: PLANNING.into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::NoCopy),
        }
    );

    let mut foreign_copy = mail.clone();
    foreign_copy.organizer_copy = Some(review("Review", "NEEDS-ACTION", "", &[]));
    assert_eq!(
        world
            .scheduler
            .incoming(&foreign_copy)
            .await
            .expect("incoming"),
        Incoming::Reply {
            uid: PLANNING.into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::NoCopy),
        },
        "only the user's own organizer copy"
    );

    let mut done = mail.clone();
    done.organizer_copy = Some(planning("Planning", "ACCEPTED", ""));
    assert_eq!(
        world.scheduler.incoming(&done).await.expect("incoming"),
        Incoming::Reply {
            uid: PLANNING.into(),
            outcome: ReplyOutcome::Unapplied(Unapplied::Unchanged),
        }
    );
    assert!(
        world.recorder.queued().is_empty(),
        "an applied REPLY sends nothing"
    );
}

fn split(line: &str) -> (String, Vec<(String, String)>, String) {
    let mut quoted = false;
    let mut colon = line.len();
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = i;
                break;
            }
            _ => {}
        }
    }
    let head = &line[..colon];
    let value = line.get(colon + 1..).unwrap_or("").to_owned();
    let mut parts = head.split(';');
    let name = parts.next().unwrap_or("").to_ascii_uppercase();
    let params = parts
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (k.to_ascii_uppercase(), v.to_owned())
        })
        .collect();
    (name, params, value)
}

fn join(name: &str, params: &[(String, String)], value: &str) -> String {
    let mut out = name.to_owned();
    for (k, v) in params {
        out.push(';');
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out.push(':');
    out.push_str(value);
    out
}

fn norm(address: &str) -> String {
    let a = address.trim();
    let a = if a.len() >= 7 && a[..7].eq_ignore_ascii_case("mailto:") {
        &a[7..]
    } else {
        a
    };
    a.to_ascii_lowercase()
}

fn param<'p>(params: &'p [(String, String)], key: &str) -> Option<&'p str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn value_of(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .map(|l| split(l))
        .find(|(n, _, _)| n == name)
        .map(|(_, _, v)| v)
}

fn organizer_of(lines: &[String]) -> Option<String> {
    value_of(lines, "ORGANIZER").map(|v| norm(&v))
}

struct Person {
    address: String,
    agent: String,
}

fn attendees(lines: &[String]) -> Vec<Person> {
    lines
        .iter()
        .map(|l| split(l))
        .filter(|(n, _, _)| n == "ATTENDEE")
        .map(|(_, p, v)| Person {
            address: norm(&v),
            agent: param(&p, "SCHEDULE-AGENT")
                .unwrap_or("SERVER")
                .to_ascii_uppercase(),
        })
        .collect()
}

fn partstat(lines: &[String], address: &str) -> Option<String> {
    lines
        .iter()
        .map(|l| split(l))
        .find(|(n, _, v)| n == "ATTENDEE" && norm(v) == address)
        .map(|(_, p, _)| param(&p, "PARTSTAT").unwrap_or("NEEDS-ACTION").to_owned())
}

fn readable(lines: &[String]) -> bool {
    lines.first().map(String::as_str) == Some("BEGIN:VCALENDAR")
        && lines.iter().any(|l| l == "BEGIN:VEVENT")
        && value_of(lines, "UID").is_some()
}

fn edit_params(
    lines: &[String],
    f: impl Fn(&str, &str, &mut Vec<(String, String)>),
) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            let (name, mut params, value) = split(l);
            if name == "ATTENDEE" || name == "ORGANIZER" {
                f(&name, &norm(&value), &mut params);
                join(&name, &params, &value)
            } else {
                l.clone()
            }
        })
        .collect()
}

fn significant_answers(lines: &[String]) -> Vec<(String, Option<String>)> {
    attendees(lines)
        .into_iter()
        .map(|p| {
            let answer = partstat(lines, &p.address);
            (p.address, answer)
        })
        .collect()
}

fn significant(lines: &[String]) -> Vec<String> {
    let mut alarm = false;
    let mut out = Vec::new();
    for l in lines {
        match l.as_str() {
            "BEGIN:VALARM" => alarm = true,
            "END:VALARM" => alarm = false,
            _ if alarm => {}
            _ => {
                let (name, params, value) = split(l);
                if matches!(name.as_str(), "DTSTAMP" | "SEQUENCE" | "TRANSP")
                    || name.starts_with("X-")
                {
                    continue;
                }
                if name == "ATTENDEE" || name == "ORGANIZER" {
                    let kept: Vec<(String, String)> = params
                        .into_iter()
                        .filter(|(k, _)| {
                            !matches!(k.as_str(), "PARTSTAT" | "RSVP" | "SCHEDULE-STATUS")
                        })
                        .collect();
                    out.push(join(&name, &kept, &value));
                } else {
                    out.push(l.clone());
                }
            }
        }
    }
    out
}

#[derive(Default)]
struct Memory {
    recent: HashMap<ArchiveId, Vec<Sent>>,
}

struct Sent {
    digest: u64,
    key: ItipKey,
    recipients: Vec<String>,
    at: i64,
    id: OutboxId,
}

/// An in-memory [`Scheduler`] over a [`Submission`] that keeps the contract for single events
/// without recurrence or folded lines, for tests of protocol front ends. It does not raise
/// `SEQUENCE` and sends whole events as its iTIP bodies.
pub struct FakeScheduler {
    submission: Arc<dyn Submission>,
    clock: Clock,
    memory: Mutex<Memory>,
}

impl FakeScheduler {
    /// A fake that submits through `submission`.
    pub fn new(submission: Arc<dyn Submission>, clock: Clock) -> Self {
        Self {
            submission,
            clock,
            memory: Mutex::default(),
        }
    }

    fn now(&self) -> i64 {
        (self.clock)().as_millis()
    }

    fn role(lines: &[String], own: &[String]) -> Role {
        let own: Vec<String> = own.iter().map(|a| norm(a)).collect();
        match organizer_of(lines) {
            Some(o) if own.contains(&o) => Role::Organizer,
            Some(_) if attendees(lines).iter().any(|p| own.contains(&p.address)) => Role::Attendee,
            _ => Role::None,
        }
    }

    async fn recent(&self, actor: &Actor, archive: ArchiveId, digest: u64) -> bool {
        let now = self.now();
        let window = RECENT.as_millis() as i64;
        let ids: Vec<OutboxId> = self
            .memory
            .lock()
            .expect("fake scheduler")
            .recent
            .get(&archive)
            .map(|all| {
                all.iter()
                    .filter(|s| s.digest == digest && now - s.at < window)
                    .map(|s| s.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            match self.submission.status(actor, id).await {
                Ok(s) if s.state == SubmissionState::Cancelled => self.forget(&[id]),
                _ => return true,
            }
        }
        false
    }

    fn forget(&self, ids: &[OutboxId]) {
        let mut memory = self.memory.lock().expect("fake scheduler");
        for all in memory.recent.values_mut() {
            all.retain(|s| !ids.contains(&s.id));
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        change: &Change,
        from: &str,
        method: ItipMethod,
        partstat: Option<String>,
        event: &[String],
        recipients: &[String],
        out: &mut Vec<RecipientStatus>,
        delivery: &mut Delivery,
    ) {
        let mut mark = |status| {
            for r in recipients {
                out.push(RecipientStatus {
                    address: r.clone(),
                    status,
                });
            }
        };
        let uid = value_of(event, "UID").unwrap_or_default();
        let sequence: u32 = value_of(event, "SEQUENCE")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let mut hasher = DefaultHasher::new();
        let stable: Vec<&String> = event.iter().filter(|l| !l.starts_with("DTSTAMP")).collect();
        (method, &uid, sequence, recipients, stable).hash(&mut hasher);
        let digest = hasher.finish();
        if self.recent(&change.actor, change.archive, digest).await {
            delivery.suppressed += 1;
            mark(ScheduleStatus::Sent);
            return;
        }
        let identity = match self
            .submission
            .sender(&change.actor, change.archive, from)
            .await
        {
            Ok(identity) => identity,
            Err(SubmissionError::Forbidden(_)) => return mark(ScheduleStatus::NoRoute),
            Err(_) => return mark(ScheduleStatus::Failed),
        };
        let mut calendar = String::new();
        for l in event {
            calendar.push_str(l);
            calendar.push_str("\r\n");
            if l == "VERSION:2.0" {
                calendar.push_str(&format!("METHOD:{}\r\n", method.as_str()));
            }
        }
        let message = format!(
            "From: <{}>\r\nTo: {}\r\nDate: Mon, 09 Nov 2026 10:00:00 +0000\r\n\
             Message-ID: <{digest}.{}@example.com>\r\nSubject: {}\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/calendar; method={}; charset=utf-8\r\n\r\n{calendar}",
            identity.address,
            recipients.join(", "),
            self.now(),
            method.as_str(),
            method.as_str(),
        );
        let request = Submit {
            actor: change.actor.clone(),
            archive: change.archive,
            identity: identity.id,
            mail_from: identity.address.clone(),
            recipients: recipients.to_vec(),
            message: Bytes::from(message),
            send_at: None,
            undo_window: None,
            idempotency_key: None,
            allow_leaks: false,
            route: change
                .reply_as
                .as_ref()
                .filter(|r| r.route.from.eq_ignore_ascii_case(from))
                .map(|r| r.route.clone()),
            hold: true,
        };
        match self.submission.queue(request).await {
            Ok(submitted) => {
                delivery.queued.push(submitted.id);
                self.memory
                    .lock()
                    .expect("fake scheduler")
                    .recent
                    .entry(change.archive)
                    .or_default()
                    .push(Sent {
                        digest,
                        key: ItipKey {
                            uid,
                            sequence,
                            method,
                            recurrence_id: value_of(event, "RECURRENCE-ID")
                                .map(|r| r.trim().to_owned()),
                            partstat,
                        },
                        recipients: recipients.iter().map(|r| norm(r)).collect(),
                        at: self.now(),
                        id: submitted.id,
                    });
                mark(ScheduleStatus::Sent);
            }
            Err(SubmissionError::Forbidden(_) | SubmissionError::FromMismatch) => {
                mark(ScheduleStatus::NoRoute)
            }
            Err(_) => mark(ScheduleStatus::Failed),
        }
    }

    fn write_statuses(
        next: &[String],
        previous: Option<&[String]>,
        name: &str,
        statuses: &[RecipientStatus],
    ) -> Vec<String> {
        let before: HashMap<String, String> = previous
            .unwrap_or_default()
            .iter()
            .map(|l| split(l))
            .filter(|(n, _, _)| n == name)
            .filter_map(|(_, p, v)| Some((norm(&v), param(&p, "SCHEDULE-STATUS")?.to_owned())))
            .collect();
        next.iter()
            .map(|l| {
                let (n, mut params, value) = split(l);
                if n != name {
                    return l.clone();
                }
                let address = norm(&value);
                let code = statuses
                    .iter()
                    .find(|s| s.address == address)
                    .map(|s| s.status.as_str().to_owned())
                    .or_else(|| before.get(&address).cloned());
                params.retain(|(k, _)| k != "SCHEDULE-STATUS");
                if let Some(code) = code {
                    params.push(("SCHEDULE-STATUS".into(), code));
                }
                join(&n, &params, &value)
            })
            .collect()
    }
}

fn to_bytes(lines: &[String]) -> Bytes {
    ics(lines)
}

fn schedules(host: Host) -> bool {
    match host {
        Host::CalDav { auto_schedule } => auto_schedule,
        Host::Graph => true,
        Host::Jmap { scheduling } => scheduling,
        Host::Mail => false,
    }
}

#[async_trait]
impl Scheduler for FakeScheduler {
    async fn prepare(&self, put: &ClientPut) -> Result<Prepared, ScheduleError> {
        let lines = unfolded(&put.body);
        if !readable(&lines) {
            return Err(ScheduleError::Invalid("not a readable event".into()));
        }
        let mut stripped = false;
        let clean: Vec<String> = lines
            .iter()
            .map(|l| {
                let (name, mut params, value) = split(l);
                if name != "ATTENDEE" && name != "ORGANIZER" {
                    return l.clone();
                }
                let before = params.len();
                params.retain(|(k, _)| k != "SCHEDULE-STATUS" && k != "SCHEDULE-FORCE-SEND");
                if params.len() == before {
                    return l.clone();
                }
                stripped = true;
                join(&name, &params, &value)
            })
            .collect();
        let role = Self::role(&clean, &put.own);
        let mut reduced = false;
        let mut kept_answers = false;
        let clean = match (role, &put.previous, put.keep_answers) {
            (Role::Organizer, Some(previous), true) => {
                let previous = unfolded(previous);
                let own: Vec<String> = put.own.iter().map(|a| norm(a)).collect();
                let merged = edit_params(&clean, |name, address, params| {
                    if name != "ATTENDEE" || own.iter().any(|o| o == address) {
                        return;
                    }
                    if let Some(answer) = partstat(&previous, address) {
                        params.retain(|(k, _)| k != "PARTSTAT");
                        params.push(("PARTSTAT".into(), answer));
                    }
                });
                kept_answers = significant_answers(&merged) != significant_answers(&clean);
                merged
            }
            _ => clean,
        };
        let mut body = clean.clone();
        if let (Role::Attendee, Some(previous)) = (role, &put.previous) {
            let previous = unfolded(previous);
            if significant(&previous) != significant(&clean) {
                reduced = true;
                let own: Vec<String> = put.own.iter().map(|a| norm(a)).collect();
                let answers: HashMap<String, String> = own
                    .iter()
                    .filter_map(|a| Some((a.clone(), partstat(&clean, a)?)))
                    .collect();
                let mut kept = edit_params(&previous, |name, address, params| {
                    if name == "ATTENDEE"
                        && let Some(answer) = answers.get(address)
                    {
                        params.retain(|(k, _)| k != "RSVP");
                        match params.iter_mut().find(|(k, _)| k == "PARTSTAT") {
                            Some(p) => p.1 = answer.clone(),
                            None => params.push(("PARTSTAT".into(), answer.clone())),
                        }
                    }
                });
                let mut alarm = false;
                let personal: Vec<String> = clean
                    .iter()
                    .filter(|l| {
                        if l.as_str() == "BEGIN:VALARM" {
                            alarm = true;
                        }
                        let keep = alarm;
                        if l.as_str() == "END:VALARM" {
                            alarm = false;
                        }
                        keep
                    })
                    .cloned()
                    .collect();
                if let Some(end) = kept.iter().position(|l| l == "END:VEVENT") {
                    kept.splice(end..end, personal);
                }
                body = kept;
            }
        }
        let body = if !stripped && !reduced && !kept_answers {
            put.body.clone()
        } else {
            to_bytes(&body)
        };
        Ok(Prepared {
            body,
            role,
            stripped,
            reduced,
        })
    }

    async fn deliver(&self, change: &Change) -> Result<Delivery, ScheduleError> {
        let previous = change.previous.as_deref().map(unfolded);
        let next = change.next.as_deref().map(unfolded);
        let event = next
            .as_ref()
            .or(previous.as_ref())
            .cloned()
            .unwrap_or_default();
        let role = Self::role(&event, &change.own);
        let mut delivery = Delivery {
            body: change.next.clone(),
            rewritten: false,
            role,
            way: Way::Nothing,
            statuses: Vec::new(),
            queued: Vec::new(),
            suppressed: 0,
        };
        if change.origin == Origin::Itip || role == Role::None {
            return Ok(delivery);
        }
        let own: Vec<String> = change.own.iter().map(|a| norm(a)).collect();
        let organizer = organizer_of(&event).unwrap_or_default();
        let mut out = Vec::new();
        if role == Role::Organizer {
            let concerned = |lines: &[String]| -> Vec<Person> {
                attendees(lines)
                    .into_iter()
                    .filter(|p| p.address != organizer && p.agent != "NONE")
                    .collect()
            };
            let (method, people, allowed) = match (&previous, &next) {
                (None, Some(n)) => (ItipMethod::Request, concerned(n), change.caps.create),
                (Some(p), None) => (ItipMethod::Cancel, concerned(p), change.caps.delete),
                (Some(p), Some(n)) if significant(p) != significant(n) => {
                    (ItipMethod::Request, concerned(n), change.caps.update)
                }
                _ => (ItipMethod::Request, Vec::new(), true),
            };
            if !people.is_empty() {
                let server = schedules(change.host);
                delivery.way = match (server, allowed) {
                    (false, _) => Way::Mail,
                    (true, true) => Way::Server,
                    (true, false) => Way::Deferred,
                };
                let by_server = if allowed {
                    ScheduleStatus::Sent
                } else {
                    ScheduleStatus::Pending
                };
                let mut mail = Vec::new();
                for p in &people {
                    match (server, p.agent.as_str(), change.origin) {
                        (_, "CLIENT", Origin::Device) => {}
                        (false, _, _) | (true, "CLIENT", _) => mail.push(p.address.clone()),
                        _ => out.push(RecipientStatus {
                            address: p.address.clone(),
                            status: by_server,
                        }),
                    }
                }
                if !mail.is_empty() {
                    self.send(
                        change,
                        &organizer,
                        method,
                        None,
                        &event,
                        &mail,
                        &mut out,
                        &mut delivery,
                    )
                    .await;
                }
            }
            let order: Vec<String> = people.iter().map(|p| p.address.clone()).collect();
            out.sort_by_key(|s| order.iter().position(|a| *a == s.address));
            if let Some(n) = &next {
                let written = Self::write_statuses(n, previous.as_deref(), "ATTENDEE", &out);
                if written != *n {
                    delivery.body = Some(to_bytes(&written));
                    delivery.rewritten = true;
                }
            }
        } else {
            let me = attendees(&event)
                .into_iter()
                .find(|p| own.contains(&p.address))
                .map(|p| p.address)
                .unwrap_or_default();
            let before = previous.as_ref().and_then(|p| partstat(p, &me));
            let after = match &next {
                Some(n) => partstat(n, &me),
                None => Some("DECLINED".into()),
            };
            if before != after && after.as_deref() != Some("NEEDS-ACTION") {
                let agent = event
                    .iter()
                    .map(|l| split(l))
                    .find(|(n, _, _)| n == "ORGANIZER")
                    .and_then(|(_, p, _)| param(&p, "SCHEDULE-AGENT").map(str::to_ascii_uppercase))
                    .unwrap_or_else(|| "SERVER".into());
                if agent == "NONE" {
                    delivery.way = Way::Quiet;
                } else if agent == "SERVER" && schedules(change.host) && change.caps.respond {
                    delivery.way = Way::Server;
                    out.push(RecipientStatus {
                        address: organizer.clone(),
                        status: ScheduleStatus::Sent,
                    });
                } else {
                    delivery.way = Way::Mail;
                    let from = change
                        .reply_as
                        .as_ref()
                        .map(|r| r.route.from.clone())
                        .unwrap_or_else(|| me.clone());
                    let answered = edit_params(&event, |name, address, params| {
                        if name == "ATTENDEE" && address == me {
                            params.retain(|(k, _)| k != "PARTSTAT" && k != "RSVP");
                            params.push(("PARTSTAT".into(), after.clone().unwrap_or_default()));
                        }
                    });
                    let to = change
                        .reply_as
                        .as_ref()
                        .and_then(|r| r.to.clone())
                        .unwrap_or_else(|| organizer.clone());
                    self.send(
                        change,
                        &from,
                        ItipMethod::Reply,
                        after.clone().map(|a| a.to_ascii_uppercase()),
                        &answered,
                        std::slice::from_ref(&to),
                        &mut out,
                        &mut delivery,
                    )
                    .await;
                    for status in &mut out {
                        if status.address == to {
                            status.address = organizer.clone();
                        }
                    }
                }
                if let Some(n) = &next {
                    let written = Self::write_statuses(n, previous.as_deref(), "ORGANIZER", &out);
                    if written != *n {
                        delivery.body = Some(to_bytes(&written));
                        delivery.rewritten = true;
                    }
                }
            }
        }
        delivery.statuses = out;
        Ok(delivery)
    }

    async fn confirm(&self, actor: &Actor, delivery: &Delivery) -> Result<(), ScheduleError> {
        for id in &delivery.queued {
            match self.submission.release(actor, *id, None).await {
                Ok(_) | Err(SubmissionError::NotFound(_)) => {}
                Err(e) => return Err(ScheduleError::Unavailable(e.to_string())),
            }
        }
        Ok(())
    }

    async fn withdraw(&self, actor: &Actor, delivery: &Delivery) -> Result<(), ScheduleError> {
        let mut undone = Vec::new();
        let mut failure = None;
        for id in &delivery.queued {
            match self.submission.undo(actor, *id).await {
                Ok(_) => undone.push(*id),
                Err(SubmissionError::TooLate(_) | SubmissionError::NotFound(_)) => {}
                Err(e) => failure = Some(ScheduleError::Unavailable(e.to_string())),
            }
        }
        self.forget(&undone);
        failure.map_or(Ok(()), Err)
    }

    async fn incoming(&self, mail: &IncomingItip) -> Result<Incoming, ScheduleError> {
        let own: Vec<String> = mail.own.iter().map(|a| norm(a)).collect();
        if !mail.arrival.live {
            return Ok(Incoming::Ignored(Ignored::NotLive));
        }
        if mail.arrival.sent_folder {
            return Ok(Incoming::Ignored(Ignored::SentFolder));
        }
        if mail.arrival.outbox_copy {
            return Ok(Incoming::Ignored(Ignored::OutboxCopy));
        }
        if mail.from.iter().any(|f| own.contains(&norm(f))) {
            return Ok(Incoming::Ignored(Ignored::OwnSender));
        }
        let lines = unfolded(&mail.calendar);
        let method = value_of(&lines, "METHOD").and_then(|m| ItipMethod::parse(&m));
        let (Some(method), true) = (method, readable(&lines)) else {
            return Ok(Incoming::Ignored(Ignored::NotItip));
        };
        if !mail.recipients.iter().any(|r| own.contains(&norm(r))) {
            return Ok(Incoming::Ignored(Ignored::NotAddressed));
        }
        let uid = value_of(&lines, "UID").unwrap_or_default();
        if method != ItipMethod::Reply {
            let from_mail = if organizer_of(&lines).is_some_and(|o| own.contains(&o)) {
                FromMail::OwnOrganizer
            } else if mail.upstream_holds_uid {
                FromMail::UpstreamHolds
            } else {
                FromMail::Update
            };
            return Ok(Incoming::Invitation {
                method,
                uid,
                from_mail,
            });
        }
        let unapplied = |why| {
            Ok(Incoming::Reply {
                uid: uid.clone(),
                outcome: ReplyOutcome::Unapplied(why),
            })
        };
        let named = attendees(&lines);
        let mut senders: Vec<String> = mail.from.iter().map(|f| norm(f)).collect();
        senders.extend(mail.signed_sender.iter().map(|s| norm(s)));
        let [attendee] = named.as_slice() else {
            return unapplied(Unapplied::NotFromAttendee);
        };
        if !senders.contains(&attendee.address) {
            return unapplied(Unapplied::NotFromAttendee);
        }
        let domain = attendee.address.rsplit('@').next().unwrap_or_default();
        let signed = mail
            .signed_sender
            .as_deref()
            .is_some_and(|s| norm(s) == attendee.address);
        let one_from = mail.from.len() == 1;
        let aligned = one_from
            && mail.auth.dkim_pass.iter().any(|d| {
                let d = d.trim().to_ascii_lowercase();
                domain == d || domain.ends_with(&format!(".{d}"))
            });
        let dmarc = one_from
            && mail
                .auth
                .dmarc_pass
                .iter()
                .any(|d| d.trim().eq_ignore_ascii_case(domain));
        if !(mail.force || signed || aligned || dmarc) {
            return unapplied(Unapplied::Unauthenticated);
        }
        let Some(copy) = &mail.organizer_copy else {
            return unapplied(Unapplied::NoCopy);
        };
        let copy = unfolded(copy);
        if value_of(&copy, "UID").as_deref() != Some(uid.as_str())
            || !organizer_of(&copy).is_some_and(|o| own.contains(&o))
        {
            return unapplied(Unapplied::NoCopy);
        }
        let answer = param(
            &split(
                lines
                    .iter()
                    .find(|l| split(l).0 == "ATTENDEE")
                    .map(String::as_str)
                    .unwrap_or_default(),
            )
            .1,
            "PARTSTAT",
        )
        .unwrap_or("NEEDS-ACTION")
        .to_owned();
        if partstat(&copy, &attendee.address).as_deref() == Some(answer.as_str()) {
            return unapplied(Unapplied::Unchanged);
        }
        let merged = edit_params(&copy, |name, address, params| {
            if name == "ATTENDEE" && address == attendee.address {
                params.retain(|(k, _)| k != "PARTSTAT" && k != "RSVP");
                params.push(("PARTSTAT".into(), answer.clone()));
            }
        });
        Ok(Incoming::Reply {
            uid,
            outcome: ReplyOutcome::Merged {
                body: to_bytes(&merged),
            },
        })
    }

    async fn just_sent(
        &self,
        archive: ArchiveId,
        key: &ItipKey,
        recipients: &[String],
    ) -> Result<bool, ScheduleError> {
        let now = self.now();
        let window = RECENT.as_millis() as i64;
        let memory = self.memory.lock().expect("fake scheduler");
        let sent: Vec<&Sent> = memory
            .recent
            .get(&archive)
            .map(|all| {
                all.iter()
                    .filter(|s| s.key == *key && now - s.at < window)
                    .collect()
            })
            .unwrap_or_default();
        Ok(!recipients.is_empty()
            && recipients
                .iter()
                .all(|r| sent.iter().any(|s| s.recipients.contains(&norm(r)))))
    }
}

/// Builds [`FakeScheduler`]s, so the suite can run against the fake.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeHarness;

impl Harness for FakeHarness {
    fn scheduler(&self, submission: Arc<dyn Submission>, clock: Clock) -> Arc<dyn Scheduler> {
        Arc::new(FakeScheduler::new(submission, clock))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fake_keeps_the_contract() {
        conformance(&FakeHarness).await;
    }
}
