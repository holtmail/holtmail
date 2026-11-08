//! Answers to the user's own invitations that arrive by mail: an attendee's iMIP `REPLY` to an
//! event the user organizes in a `write_through` calendar whose server does not schedule becomes
//! a held change of the organizer's copy with that attendee's `PARTSTAT`, which `pim-writeback`
//! then writes. Elsewhere the reply only shows as derived data: it is a version of the event in
//! the "From mail" calendar, and the calendar lists each copy's answers.
//!
//! A reply speaks for one attendee, and only for the sender of the mail: a reply naming several
//! attendees or anyone but its `From` (or, for mail through an alias service that signed it, the
//! real sender behind the alias) stays derived data. A reply that cannot be applied now (the
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
use holtmail_pim::{ItemKind, Method};
use holtmail_pim_sync::Edits;
use holtmail_pim_writeback::Writeback;
use holtmail_store::{
    AuditKind, Capability, ChangeClass, ChangeContent, ChangeOp, ChangeOrigin, ChangeTarget,
    Collection, CollectionKind, DeferredReply, Effective, Expect, Item, MessageAlias,
    NewAuditEntry, NewItemChange, RecordedChange, Store, WriteMode,
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
    budget: Mutex<HashMap<ArchiveId, (Instant, u32)>>,
}

/// The ingest observer for replies; does nothing until [`Replies::start`] gave it the writers.
#[derive(Default)]
pub struct Replies(OnceLock<Arc<Inner>>);

impl Replies {
    /// Starts acting on replies with these services, and retrying those that had to wait.
    pub fn start(
        &self,
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        edits: Edits,
        writers: Arc<PimWriters>,
        writeback: Arc<Writeback>,
        switches: WriteConfig,
    ) {
        let inner = Arc::new(Inner {
            store,
            blobs,
            edits,
            writers,
            writeback,
            switches,
            budget: Mutex::default(),
        });
        if self.0.set(inner.clone()).is_ok() {
            tokio::spawn(inner.retry());
        }
    }
}

/// What the mail is known to come from.
fn senders(parsed: &holtmail_mime::ParsedMessage, alias: Option<&MessageAlias>) -> Vec<String> {
    let mut out: Vec<String> = parsed.from.iter().map(|a| a.email.clone()).collect();
    if let Some(sender) = alias.filter(|a| a.signed).and_then(|a| a.sender.as_ref()) {
        out.push(sender.email.clone());
    }
    out
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
        if !placement.live || !inner.switches.enabled {
            return;
        }
        if matches!(
            inner.apply_message(archive, message, parsed).await,
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
                self.apply_message(reply.archive, reply.message, &parsed)
                    .await
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

    async fn apply_message(
        &self,
        archive: ArchiveId,
        message: MessageId,
        parsed: &holtmail_mime::ParsedMessage,
    ) -> Applied {
        let mut outcome = Applied::Done;
        let alias = self.store.message_alias(message).await.ok().flatten();
        let senders = senders(parsed, alias.as_ref());
        for part in &parsed.calendars {
            let method = holtmail_pim::calendar_method(part);
            if Method::of(method.as_deref()) != Method::Reply {
                continue;
            }
            match self.apply(archive, part, &senders).await {
                Ok((count, later)) => {
                    if count > 0 {
                        tracing::info!(%message, count, "attendee answers recorded on organizer copies");
                        self.writeback.wake();
                    }
                    if later {
                        outcome = Applied::Later;
                    }
                }
                Err(error) => {
                    tracing::warn!(%message, %error, "attendee answer not recorded");
                    outcome = Applied::Later;
                }
            }
        }
        outcome
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

    /// How many copies took the reply and whether some copy must try again later.
    async fn apply(
        &self,
        archive: ArchiveId,
        part: &Bytes,
        senders: &[String],
    ) -> anyhow::Result<(u32, bool)> {
        let Some(uid) = holtmail_pim::split(part)
            .into_iter()
            .find(|r| r.kind == ItemKind::Event)
            .and_then(|r| r.uid)
        else {
            return Ok((0, false));
        };
        let group = holtmail_calendar::group_id(archive, Some(&uid), ItemId::new());
        let own: Vec<String> = self
            .edits
            .own_addresses(archive)
            .await?
            .iter()
            .filter_map(|a| holtmail_pim::normalize_email(a))
            .collect();
        let mut recorded = 0;
        let mut later = false;
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
            match self
                .apply_item(&item, &collection, part, senders, &own)
                .await?
            {
                Step::Recorded => recorded += 1,
                Step::Nothing => {}
                Step::Later => later = true,
            }
        }
        Ok((recorded, later))
    }

    async fn apply_item(
        &self,
        item: &Item,
        collection: &Collection,
        part: &Bytes,
        senders: &[String],
        own: &[String],
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
            let Some(next) = holtmail_pim_edit::apply_reply(&served.raw, part, senders) else {
                return Ok(Step::Nothing);
            };
            if !self.spend(item.archive_id) {
                return Ok(Step::Later);
            }
            if self.record(item, collection, &effective, next, own).await? {
                return Ok(Step::Recorded);
            }
        }
        Ok(Step::Later)
    }

    /// Records the copy with the answer; `false` when what is served changed meanwhile.
    async fn record(
        &self,
        item: &Item,
        collection: &Collection,
        effective: &Effective,
        next: Vec<u8>,
        own: &[String],
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
            origin: ChangeOrigin::Api,
            user_id: None,
            device: None,
            at: Timestamp::now(),
            not_before: None,
            messages: Vec::new(),
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
    Nothing,
    Later,
}
