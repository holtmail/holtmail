//! The incoming iTIP hook: every live arrival carrying `text/calendar` with `METHOD:REQUEST`,
//! `CANCEL` or `REPLY` goes through the scheduler's `incoming` decision, and each part it takes
//! becomes a schedule-inbox entry with the decision as its note. An attendee's `REPLY` to an
//! event the user organizes in a `write_through` calendar whose server does not schedule is
//! merged into the organizer's copy as a held change with origin `itip`, which `pim-writeback`
//! then writes; the copy's `Schedule-Tag` stays as it was. Elsewhere the reply only shows as
//! derived data: it is a version of the event in the "From mail" calendar, and the calendar
//! lists each copy's answers. Nothing here ever sends a message.
//!
//! A reply speaks for one attendee, and only for the sender of the mail, and only with DKIM or
//! DMARC passing for that sender in the topmost `Authentication-Results` whose authserv-id the
//! operator trusts for the account the mail came through (the account's trusted list, or
//! `[smtp] trusted_authserv_ids` for the SMTP listener's account); a field no trusted server
//! wrote proves nothing, since a sender can write one. A reply naming several
//! attendees, anyone but its `From` (or, for mail through an alias service that signed it, the
//! real sender behind the alias), or without that check passing stays derived data with a note;
//! "Apply anyway" merges an unauthenticated one. A reply that cannot be applied now (the
//! calendar's server cannot be asked, the copy keeps changing, the store fails, too many
//! automatic changes this hour) waits in the store and is tried again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_api::SchedulingProbe;
use holtmail_blob::BlobStore;
use holtmail_core::{ArchiveId, ItemId, MessageId, Timestamp};
use holtmail_pim::ItemKind;
use holtmail_pim_sync::Edits;
use holtmail_pim_writeback::Writeback;
use holtmail_schedule::{
    Arrival, AuthFacts, FromMail, Incoming, IncomingItip, ItipMethod, ReplyOutcome, Scheduler,
    Unapplied,
};
use holtmail_store::{
    AccountKind, AuditKind, Capability, ChangeClass, ChangeContent, ChangeOp, ChangeOrigin,
    ChangeTarget, Collection, CollectionKind, DeferredReply, Effective, Expect, FolderRole,
    InboxNote, Item, NewAuditEntry, NewItemChange, RecordedChange, ScheduleInboxEntry, Store,
    WriteMode,
};

use crate::config::WriteConfig;
use crate::pim_writeback::PimWriters;

const RETRY_TICK: Duration = Duration::from_secs(60);
const RETRY_BATCH: u32 = 100;
const FIRST_RETRY_MS: i64 = 60_000;
const MAX_RETRY_MS: i64 = 6 * 3_600_000;
const GIVE_UP_AFTER_MS: i64 = 7 * 86_400_000;
const RECORD_TRIES: u32 = 3;
const MAX_AUTOMATIC_PER_HOUR: u32 = 300;
const HOUR: Duration = Duration::from_secs(3_600);

struct Inner {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    edits: Edits,
    writers: Arc<PimWriters>,
    writeback: Arc<Writeback>,
    switches: WriteConfig,
    scheduler: Arc<dyn Scheduler>,
    smtp_authserv_ids: Vec<String>,
    budget: Mutex<HashMap<ArchiveId, (Instant, u32)>>,
}

/// The ingest observer for replies; does nothing until [`Replies::start`] gave it the writers.
#[derive(Default)]
pub struct Replies(OnceLock<Arc<Inner>>);

impl Replies {
    /// Starts acting on replies with these services, and retrying those that had to wait.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        edits: Edits,
        writers: Arc<PimWriters>,
        writeback: Arc<Writeback>,
        switches: WriteConfig,
        scheduler: Arc<dyn Scheduler>,
        smtp_authserv_ids: Vec<String>,
    ) {
        let inner = Arc::new(Inner {
            store,
            blobs,
            edits,
            writers,
            writeback,
            switches,
            scheduler,
            smtp_authserv_ids: smtp_authserv_ids
                .iter()
                .map(|id| id.trim().to_ascii_lowercase())
                .collect(),
            budget: Mutex::default(),
        });
        if self.0.set(inner.clone()).is_ok() {
            tokio::spawn(inner.retry());
        }
    }

    /// "Apply anyway": the message's parts decided again with the authentication check
    /// overridden; returns their notes as they are now. Nothing when the hook has not started
    /// or the instance switch is off: no calendar change is recorded while writing is closed.
    pub async fn apply_anyway(
        &self,
        archive: ArchiveId,
        message: MessageId,
    ) -> anyhow::Result<Vec<InboxNote>> {
        let Some(inner) = self.0.get() else {
            return Ok(Vec::new());
        };
        if !inner.switches.enabled {
            return Ok(Vec::new());
        }
        let Some(parsed) = inner.parsed(message).await? else {
            return Ok(Vec::new());
        };
        let (_, notes) = inner
            .apply_message(archive, message, &parsed, Arrival::live(), true)
            .await;
        if notes.contains(&InboxNote::Applied) {
            inner.writeback.wake();
        }
        Ok(notes)
    }
}

trait Live {
    fn live() -> Self;
}

impl Live for Arrival {
    fn live() -> Self {
        Arrival {
            live: true,
            sent_folder: false,
            outbox_copy: false,
        }
    }
}

/// The DKIM and DMARC results of the topmost `Authentication-Results` whose authserv-id is in
/// `trusted` (lowercase); none without one.
fn auth_facts(parsed: &holtmail_mime::ParsedMessage, trusted: &[String]) -> AuthFacts {
    let Some(results) = parsed
        .headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("Authentication-Results"))
        .filter_map(|h| holtmail_auth_results::parse_auth_results(&h.value))
        .find(|r| trusted.contains(&r.authserv_id.trim().to_ascii_lowercase()))
    else {
        return AuthFacts::default();
    };
    let mut facts = AuthFacts::default();
    for result in &results.results {
        if result.result != "pass" {
            continue;
        }
        match result.method.as_str() {
            "dkim" => {
                if let Some(domain) = result.dkim().and_then(|d| d.signing_domain())
                    && !facts.dkim_pass.contains(&domain)
                {
                    facts.dkim_pass.push(domain);
                }
            }
            "dmarc" => {
                if let Some(domain) = result
                    .property("header", "from")
                    .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
                    .filter(|d| !d.is_empty())
                    && !facts.dmarc_pass.contains(&domain)
                {
                    facts.dmarc_pass.push(domain);
                }
            }
            _ => {}
        }
    }
    facts
}

fn recipients(parsed: &holtmail_mime::ParsedMessage) -> Vec<String> {
    let mut out: Vec<String> = parsed
        .to
        .iter()
        .chain(&parsed.cc)
        .map(|a| a.email.to_ascii_lowercase())
        .collect();
    for header in &parsed.headers {
        if ["Delivered-To", "X-Original-To", "Envelope-To"]
            .iter()
            .any(|n| header.name.eq_ignore_ascii_case(n))
        {
            let value = header
                .value
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            out.push(value.to_ascii_lowercase());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn note_of(unapplied: Unapplied) -> InboxNote {
    match unapplied {
        Unapplied::NotFromAttendee => InboxNote::NotFromAttendee,
        Unapplied::Unauthenticated => InboxNote::Unauthenticated,
        Unapplied::NoCopy => InboxNote::NoCopy,
        Unapplied::Unchanged => InboxNote::Unchanged,
    }
}

enum Applied {
    /// Nothing to do, or nothing more to do.
    Done,
    /// It must be tried again later.
    Later,
}

#[async_trait]
impl holtmail_ingest::Observer for Replies {
    async fn added(
        &self,
        archive: ArchiveId,
        message: MessageId,
        _raw: &[u8],
        parsed: &holtmail_mime::ParsedMessage,
        placement: &holtmail_ingest::Placement,
    ) {
        let Some(inner) = self.0.get() else {
            return;
        };
        if !placement.live || !inner.switches.enabled || parsed.calendars.is_empty() {
            return;
        }
        let arrival = inner.arrival(archive, parsed, placement).await;
        if matches!(
            inner
                .apply_message(archive, message, parsed, arrival, false)
                .await
                .0,
            Applied::Later
        ) {
            inner.defer(archive, message, 1).await;
        }
    }
}

impl Inner {
    async fn retry(self: Arc<Self>) {
        loop {
            tokio::time::sleep(RETRY_TICK).await;
            if !self.switches.enabled {
                continue;
            }
            let now = Timestamp::now();
            let due = match self.store.due_replies(now, RETRY_BATCH).await {
                Ok(due) => due,
                Err(error) => {
                    tracing::warn!(%error, "deferred replies not read");
                    continue;
                }
            };
            for reply in due {
                self.retry_one(reply, now).await;
            }
        }
    }

    async fn retry_one(&self, reply: DeferredReply, now: Timestamp) {
        let outcome = match self.parsed(reply.message).await {
            Ok(Some(parsed)) => {
                self.apply_message(
                    reply.archive,
                    reply.message,
                    &parsed,
                    Arrival::live(),
                    false,
                )
                .await
                .0
            }
            Ok(None) => Applied::Done,
            Err(error) => {
                tracing::warn!(message = %reply.message, %error, "deferred reply not read");
                Applied::Later
            }
        };
        let expired = now.as_millis() - reply.since.as_millis() > GIVE_UP_AFTER_MS;
        match outcome {
            Applied::Later if !expired => {
                self.defer(reply.archive, reply.message, reply.attempts + 1)
                    .await;
            }
            later => {
                if matches!(later, Applied::Later) {
                    tracing::warn!(message = %reply.message, "attendee answer given up on");
                }
                if let Err(error) = self.store.forget_reply(reply.message).await {
                    tracing::warn!(message = %reply.message, %error, "deferred reply not cleared");
                }
            }
        }
    }

    async fn parsed(
        &self,
        message: MessageId,
    ) -> anyhow::Result<Option<holtmail_mime::ParsedMessage>> {
        let Some(found) = self.store.get_message(message).await? else {
            return Ok(None);
        };
        let Some(raw) = self.blobs.get(&found.blob_id).await? else {
            return Ok(None);
        };
        Ok(holtmail_mime::parse(&raw).ok())
    }

    async fn defer(&self, archive: ArchiveId, message: MessageId, attempts: u32) {
        let now = Timestamp::now();
        let wait = FIRST_RETRY_MS
            .saturating_mul(1_i64 << attempts.saturating_sub(1).min(16))
            .min(MAX_RETRY_MS);
        let next = Timestamp::from_millis(now.as_millis() + wait);
        if let Err(error) = self.store.defer_reply(archive, message, now, next).await {
            tracing::warn!(%message, %error, "attendee answer not queued for another try");
        }
    }

    /// Where the message was found: a Sent folder or label, or the archived copy of an outbox
    /// message.
    async fn arrival(
        &self,
        archive: ArchiveId,
        parsed: &holtmail_mime::ParsedMessage,
        placement: &holtmail_ingest::Placement,
    ) -> Arrival {
        let mut sent_folder = placement
            .labels
            .as_ref()
            .is_some_and(|labels| labels.iter().any(|l| l.eq_ignore_ascii_case("\\Sent")));
        if let Some(folder) = placement.folder
            && let Ok(Some(folder)) = self.store.get_folder(folder).await
        {
            sent_folder |= folder.role == Some(FolderRole::Sent);
        }
        let outbox_copy = match &parsed.message_id {
            Some(id) => self
                .store
                .outbox_by_rfc_message_id(archive, id)
                .await
                .is_ok_and(|rows| !rows.is_empty()),
            None => false,
        };
        Arrival {
            live: placement.live,
            sent_folder,
            outbox_copy,
        }
    }

    /// Decides every iTIP part of the message, records its inbox entry and merges replies;
    /// whether something must be tried again, and the notes in part order.
    async fn apply_message(
        &self,
        archive: ArchiveId,
        message: MessageId,
        parsed: &holtmail_mime::ParsedMessage,
        arrival: Arrival,
        force: bool,
    ) -> (Applied, Vec<InboxNote>) {
        let mut outcome = Applied::Done;
        let mut notes = Vec::new();
        let alias = self.store.message_alias(message).await.ok().flatten();
        let own = match self.edits.own_addresses(archive).await {
            Ok(own) => own,
            Err(error) => {
                tracing::warn!(%message, %error, "own addresses not read for an iTIP message");
                return (Applied::Later, notes);
            }
        };
        let base = IncomingItip {
            calendar: Bytes::new(),
            from: parsed.from.iter().map(|a| a.email.clone()).collect(),
            signed_sender: alias
                .as_ref()
                .filter(|a| a.signed)
                .and_then(|a| a.sender.as_ref())
                .map(|s| s.email.clone()),
            recipients: recipients(parsed),
            own,
            arrival,
            auth: auth_facts(parsed, &self.trusted_authserv_ids(archive, message).await),
            upstream_holds_uid: false,
            organizer_copy: None,
            force,
        };
        for (index, part) in parsed.calendars.iter().enumerate() {
            let method = holtmail_pim::calendar_method(part);
            let Some(method) = method.as_deref().and_then(ItipMethod::parse) else {
                continue;
            };
            let Some(uid) = holtmail_pim::split(part)
                .into_iter()
                .find(|r| r.kind == ItemKind::Event)
                .and_then(|r| r.uid)
            else {
                continue;
            };
            let mut mail = base.clone();
            mail.calendar = part.clone();
            let decided = match method {
                ItipMethod::Reply => self.apply(archive, &uid, &mail).await,
                _ => {
                    mail.upstream_holds_uid = self.upstream_holds(archive, &uid).await;
                    self.scheduler
                        .incoming(&mail)
                        .await
                        .map(|incoming| (invitation_note(&incoming), false))
                        .map_err(anyhow::Error::from)
                }
            };
            let note = match decided {
                Ok((note, later)) => {
                    if later {
                        outcome = Applied::Later;
                    }
                    note
                }
                Err(error) => {
                    tracing::warn!(%message, %error, "iTIP message not decided");
                    outcome = Applied::Later;
                    continue;
                }
            };
            let Some(note) = note else {
                continue;
            };
            notes.push(note);
            let entry = ScheduleInboxEntry {
                archive_id: archive,
                message_id: message,
                part: u32::try_from(index).unwrap_or(u32::MAX),
                method: method.as_str().to_owned(),
                uid,
                note,
                received_at: Timestamp::now(),
            };
            if let Err(error) = self.store.put_inbox_entry(&entry).await {
                tracing::warn!(%message, %error, "schedule inbox entry not recorded");
            }
        }
        (outcome, notes)
    }

    /// The authserv-ids trusted for the accounts whose folders hold the message, lowercase.
    async fn trusted_authserv_ids(&self, archive: ArchiveId, message: MessageId) -> Vec<String> {
        let mut accounts = Vec::new();
        for location in self.store.locations(message).await.unwrap_or_default() {
            if let Ok(Some(folder)) = self.store.get_folder(location.folder_id).await
                && !accounts.contains(&folder.account_id)
            {
                accounts.push(folder.account_id);
            }
        }
        let stored = self
            .store
            .trusted_authserv_ids(archive)
            .await
            .unwrap_or_default();
        let mut out: Vec<String> = Vec::new();
        for account in accounts {
            let smtp = matches!(
                self.store.get_account(account).await,
                Ok(Some(a)) if a.kind == AccountKind::Import && a.name == crate::smtp::ACCOUNT_NAME
            );
            let ids = match smtp {
                true => self.smtp_authserv_ids.clone(),
                false => stored.get(&account).cloned().unwrap_or_default(),
            };
            for id in ids {
                let id = id.trim().to_ascii_lowercase();
                if !id.is_empty() && !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// Whether a served upstream calendar of the archive holds the `UID`.
    async fn upstream_holds(&self, archive: ArchiveId, uid: &str) -> bool {
        let group = holtmail_calendar::group_id(archive, Some(uid), ItemId::new());
        let Ok(members) = self.store.group_members(group).await else {
            return false;
        };
        for member in members {
            let item = member.summary.item;
            if item.removed_at.is_some() || item.archive_id != archive {
                continue;
            }
            if let Ok(Some(collection)) = self.store.get_collection(item.collection_id).await
                && collection.account_id.is_some()
                && collection.removed_at.is_none()
                && collection.kind == CollectionKind::Calendar
            {
                return true;
            }
        }
        false
    }

    /// Takes one automatic change from the archive's budget for this hour.
    fn spend(&self, archive: ArchiveId) -> bool {
        let mut budget = self.budget.lock().expect("reply budget");
        let now = Instant::now();
        let (start, used) = budget.entry(archive).or_insert((now, 0));
        if now.duration_since(*start) >= HOUR {
            *start = now;
            *used = 0;
        }
        if *used >= MAX_AUTOMATIC_PER_HOUR {
            return false;
        }
        *used += 1;
        true
    }

    /// The note of a reply and whether some copy must try again later.
    async fn apply(
        &self,
        archive: ArchiveId,
        uid: &str,
        mail: &IncomingItip,
    ) -> anyhow::Result<(Option<InboxNote>, bool)> {
        let group = holtmail_calendar::group_id(archive, Some(uid), ItemId::new());
        let own: Vec<String> = mail
            .own
            .iter()
            .filter_map(|a| holtmail_pim::normalize_email(a))
            .collect();
        let mut note: Option<InboxNote> = None;
        let mut later = false;
        let mut recorded = 0;
        for member in self.store.group_members(group).await? {
            let item = member.summary.item;
            if item.removed_at.is_some() || item.archive_id != archive {
                continue;
            }
            let Some(collection) = self.store.get_collection(item.collection_id).await? else {
                continue;
            };
            let Some(account) = collection.account_id else {
                continue;
            };
            if collection.kind != CollectionKind::Calendar
                || collection.write_mode != WriteMode::WriteThrough
                || collection.removed_at.is_some()
                || !self
                    .store
                    .account_capabilities(account)
                    .await?
                    .has(Capability::PimEdit)
            {
                continue;
            }
            match self.apply_item(&item, &collection, &own, mail).await? {
                Step::Recorded => recorded += 1,
                Step::Noted(n) => {
                    note.get_or_insert(n);
                }
                Step::Nothing => {}
                Step::Later => later = true,
            }
        }
        if recorded > 0 {
            tracing::info!(
                count = recorded,
                "attendee answers recorded on organizer copies"
            );
            self.writeback.wake();
            return Ok((Some(InboxNote::Applied), later));
        }
        if note.is_none() && !later {
            note = match self.scheduler.incoming(mail).await? {
                Incoming::Ignored(_) => None,
                Incoming::Reply {
                    outcome: ReplyOutcome::Unapplied(reason),
                    ..
                } => Some(note_of(reason)),
                Incoming::Reply { .. } | Incoming::Invitation { .. } => Some(InboxNote::NoCopy),
            };
        }
        Ok((note, later))
    }

    async fn apply_item(
        &self,
        item: &Item,
        collection: &Collection,
        own: &[String],
        mail: &IncomingItip,
    ) -> anyhow::Result<Step> {
        for _ in 0..RECORD_TRIES {
            let Some(effective) = self
                .store
                .effective(&ChangeTarget::Item { item: item.id })
                .await?
            else {
                return Ok(Step::Nothing);
            };
            let Some(served) = self.edits.served(&effective).await? else {
                return Ok(Step::Nothing);
            };
            if !holtmail_pim_edit::organizer(&served.raw).is_some_and(|o| own.contains(&o)) {
                return Ok(Step::Nothing);
            }
            match self.writers.scheduling(collection).await {
                SchedulingProbe::Known(known) if !known.host.schedules() => {}
                SchedulingProbe::Unknown => return Ok(Step::Later),
                _ => return Ok(Step::Nothing),
            }
            let mut decide = mail.clone();
            decide.organizer_copy = Some(served.raw.clone());
            let next = match self.scheduler.incoming(&decide).await? {
                Incoming::Reply {
                    outcome: ReplyOutcome::Merged { body },
                    ..
                } => body,
                Incoming::Reply {
                    outcome: ReplyOutcome::Unapplied(reason),
                    ..
                } => return Ok(Step::Noted(note_of(reason))),
                _ => return Ok(Step::Nothing),
            };
            if !self.spend(item.archive_id) {
                return Ok(Step::Later);
            }
            let pinned = item
                .schedule_tag
                .clone()
                .unwrap_or_else(|| holtmail_blob::BlobId::of(&served.raw).to_string());
            if self
                .record(item, collection, &effective, next.to_vec(), own, pinned)
                .await?
            {
                return Ok(Step::Recorded);
            }
        }
        Ok(Step::Later)
    }

    /// Records the copy with the answer, keeping the `Schedule-Tag` `pinned` in the same step;
    /// `false` when what is served changed meanwhile.
    async fn record(
        &self,
        item: &Item,
        collection: &Collection,
        effective: &Effective,
        next: Vec<u8>,
        own: &[String],
        pinned: String,
    ) -> anyhow::Result<bool> {
        let base = match &effective.latest {
            Some(latest) => self
                .edits
                .version_text(latest, ItemKind::Event)
                .await?
                .map(|t| (latest.number, t)),
            None => None,
        };
        let base_bytes = base.as_ref().map(|(_, b)| b.as_ref());
        if holtmail_pim_edit::validate(ItemKind::Event, &next, base_bytes).is_err() {
            tracing::debug!(item = %item.id, "attendee answer would not be a valid event");
            return Ok(true);
        }
        let classified = holtmail_pim_edit::classify(ItemKind::Event, base_bytes, &next, own)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let blob_id = self.blobs.put(Bytes::from(next)).await?;
        let change = NewItemChange {
            target: ChangeTarget::Item { item: item.id },
            op: ChangeOp::Update,
            class: ChangeClass::parse(classified.class.as_str()).unwrap_or(ChangeClass::Edit),
            content: Some(ChangeContent {
                blob_id,
                content_type: ItemKind::Event.content_type().into(),
            }),
            patch: serde_json::to_value(&classified.patch).ok(),
            base_version: base.map(|(n, _)| n),
            origin: ChangeOrigin::Itip,
            user_id: None,
            device: None,
            at: Timestamp::now(),
            not_before: None,
            messages: Vec::new(),
            schedule_tag: Some(pinned),
        };
        let RecordedChange::Recorded(recorded) = self
            .store
            .record_change(&change, Expect::State(effective.state))
            .await?
        else {
            return Ok(false);
        };
        let audited = self
            .store
            .append_audit(&NewAuditEntry {
                at: Timestamp::now(),
                user: None,
                kind: AuditKind::PimEdit,
                archive: Some(item.archive_id),
                account: collection.account_id,
                targets: vec![recorded.id.to_string()],
                count: 1,
            })
            .await;
        if let Err(error) = audited {
            tracing::warn!(change = %recorded.id, %error, "attendee answer not audited");
        }
        if let Err(error) = self.edits.derive(item.id).await {
            tracing::warn!(item = %item.id, %error, "derived data not rebuilt after an answer");
        }
        Ok(true)
    }
}

enum Step {
    Recorded,
    Noted(InboxNote),
    Nothing,
    Later,
}

/// The note of an incoming `REQUEST` or `CANCEL`; `None` when the scheduler ignored it.
fn invitation_note(incoming: &Incoming) -> Option<InboxNote> {
    match incoming {
        Incoming::Ignored(_) => None,
        Incoming::Invitation { from_mail, .. } => Some(match from_mail {
            FromMail::Update => InboxNote::Update,
            FromMail::UpstreamHolds => InboxNote::UpstreamHolds,
            FromMail::OwnOrganizer => InboxNote::OwnOrganizer,
        }),
        Incoming::Reply { .. } => Some(InboxNote::NoCopy),
    }
}
