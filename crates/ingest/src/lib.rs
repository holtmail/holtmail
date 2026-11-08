//! From raw message bytes to a stored, indexed message, and from raw vCard and iCalendar objects
//! to stored item versions.
//!
//! Sync and import both hand every message to [`Ingest::ingest`] and every contact, event or task
//! to [`Ingest::ingest_item`], so there is one path into the archive and one set of rules: bytes
//! first, then metadata, then the index.

mod pim;

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_alias::{AliasDetector, Header, Mail};
use holtmail_blob::{BlobError, BlobId, BlobStore};
use holtmail_core::{ArchiveId, FolderId, MessageId, ThreadId, Timestamp};
use holtmail_extract::{ExtractError, Extractor, File};
use holtmail_index::{IndexDocument, IndexError, SearchIndex};
use holtmail_insight::KindTags;
use holtmail_insights::{Deriver, Runner};
use holtmail_mime::{MimeError, ParsedMessage};
use holtmail_store::{
    Attachment, CategoryPolicy, Derived, DerivedKind, InsightOutcome, InsightRow, MessageAlias,
    NewMessage, Store, StoreError,
};
use uuid::Uuid;

pub use pim::{IncomingItem, MAIL_COLLECTION, item_document, pim_index};

/// Plain text indexed per message; the stored bytes are never cut.
const MAX_INDEXED_TEXT: usize = 1 << 20;

/// Attachment text indexed per message, all attachments together.
pub const MAX_ATTACHMENT_TEXT: usize = 4 << 20;

/// Where a message was found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// The folder, when the message came from one.
    pub folder: Option<FolderId>,
    /// Its UID in the folder.
    pub uid: Option<u64>,
    /// Flags as the source reported them.
    pub flags: Vec<String>,
    /// Labels as the source reported them; they replace the message's labels. `None` leaves the
    /// labels as they are.
    pub labels: Option<Vec<String>>,
    /// When the server received it.
    pub received_at: Option<Timestamp>,
    /// It arrived while the folder was being followed (a watch, an incremental poll, a
    /// delivery), not in a backfill, an import or a rebuild.
    pub live: bool,
}

/// Told about every message [`Ingest::ingest`] adds to an archive, after it is stored and
/// located and before it is indexed.
#[async_trait]
pub trait Observer: Send + Sync {
    /// `message` was just added to `archive` with these bytes, as parsed, placed as
    /// `placement` says. Not called for bytes the archive already had. Whatever the observer
    /// does never fails ingest: it handles and logs its own errors.
    async fn added(
        &self,
        archive: ArchiveId,
        message: MessageId,
        raw: &[u8],
        parsed: &ParsedMessage,
        placement: &Placement,
    );
}

/// What [`Ingest::ingest`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ingested {
    /// The message.
    pub message_id: MessageId,
    /// Its thread.
    pub thread_id: ThreadId,
    /// `false` when the archive already had these bytes.
    pub created: bool,
}

/// Why a message could not be ingested.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// The bytes are not an email message.
    #[error(transparent)]
    Mime(#[from] MimeError),
    /// Storing bytes failed.
    #[error(transparent)]
    Blob(#[from] BlobError),
    /// Storing metadata failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Indexing failed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// These bytes were deleted from the archive on purpose and are not archived again.
    #[error("deleted from this archive")]
    Deleted,
}

/// The ingest pipeline over the three storage contracts.
#[derive(Clone)]
pub struct Ingest {
    blobs: Arc<dyn BlobStore>,
    store: Arc<dyn Store>,
    index: Arc<dyn SearchIndex>,
    extractor: Option<Arc<dyn Extractor>>,
    detectors: Vec<Arc<dyn AliasDetector>>,
    insights: Option<Arc<Runner>>,
    categories: Option<(Arc<Deriver>, CategoryPolicy)>,
    observers: Vec<Arc<dyn Observer>>,
}

impl Ingest {
    /// A pipeline writing to these stores.
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        store: Arc<dyn Store>,
        index: Arc<dyn SearchIndex>,
    ) -> Self {
        Self {
            blobs,
            store,
            index,
            extractor: None,
            detectors: Vec::new(),
            insights: None,
            categories: None,
            observers: Vec::new(),
        }
    }

    /// The blob store it writes bytes to.
    pub fn blobs(&self) -> Arc<dyn BlobStore> {
        self.blobs.clone()
    }

    /// The same pipeline, also telling `observer` about every message it adds, in the order
    /// observers were added.
    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observers.push(observer);
        self
    }

    /// The same pipeline, also computing message insights with `runner` and recording them with
    /// `InsightStore::set_message_insights` under `runner.extract_fp()`.
    ///
    /// [`Ingest::ingest`] computes them for a new message, and for a known one that has no
    /// insights row yet, so a repeated sync repairs a lost write. Failing to store them never
    /// fails ingest. [`Ingest::reindex`] fills a missing or stale row. Without a runner no
    /// insights are read or written anywhere.
    pub fn with_insights(mut self, runner: Arc<Runner>) -> Self {
        self.insights = Some(runner);
        self
    }

    /// The same pipeline, also running stage B for one message with `deriver` and recording it
    /// with `CategoryStore::set_message_categories` under `policy`.
    ///
    /// It runs after every insights write ([`Ingest::ingest`], [`Ingest::insights_for`],
    /// [`Ingest::refresh_insights`], [`Ingest::reindex`]), and on every [`Ingest::ingest`] once
    /// the location is recorded, since a new location can change the trusted authserv-ids that
    /// apply. Each run takes a new [`DeriveContext`](holtmail_insights::DeriveContext), reads only
    /// the store, never a blob, and writes the fingerprint it computed with. A message without an
    /// insights row of outcome ok or partial is left alone. Failing never fails the call it runs
    /// in; the failure is logged with the message id only. Without it no categories are written
    /// here.
    pub fn with_categories(mut self, deriver: Arc<Deriver>, policy: CategoryPolicy) -> Self {
        self.categories = Some((deriver, policy));
        self
    }

    /// Computes the insights of `message` from its stored bytes `raw`, already parsed into
    /// `parsed`, and records them under the current fingerprint. A row equal to the stored one in
    /// everything but `computed_at` is not written. Returns `None` when it did not write, always
    /// without [`Ingest::with_insights`].
    ///
    /// Before a write the message is queued for reindexing as by `CategoryStore::queue_reindex`,
    /// so a new insights row is never stored without its queued reindex. After the write, stage B
    /// runs as [`Ingest::with_categories`] says, then the message's index document is rebuilt from
    /// `parsed`, so `has:` and `category:` follow the new rows (searchable after
    /// [`Ingest::commit`]); no blob is read. The returned sequence is the message's queue
    /// sequence read before that rebuild, for `CategoryStore::dequeue_reindex` once the index is
    /// committed; 0 when it was no longer queued. Fails with `StoreError::NotFound` for an
    /// unknown message.
    pub async fn insights_for(
        &self,
        message: MessageId,
        _raw: &[u8],
        parsed: &ParsedMessage,
    ) -> Result<Option<u64>, IngestError> {
        let Some(runner) = &self.insights else {
            return Ok(None);
        };
        let archive = self.archive_of(message).await?;
        let row = compute_insights(runner, message, archive, parsed);
        if !self.insights_changed(&row).await? {
            return Ok(None);
        }
        self.store.queue_reindex(&[message]).await?;
        self.store.set_message_insights(message, &row).await?;
        self.categorize(message, archive).await;
        let seq = self.queued_seq(archive, message).await?;
        self.reindex_parsed(message, parsed).await?;
        Ok(Some(seq))
    }

    /// Records that the stored bytes of `message` could not be read: outcome `unreadable`, no
    /// items, under the current fingerprint, so it is not listed as needing insights again until
    /// the fingerprint changes. Replaces any earlier row. Does nothing without
    /// [`Ingest::with_insights`].
    pub async fn mark_insights_unreadable(&self, message: MessageId) -> Result<(), IngestError> {
        self.mark_insights(message, InsightOutcome::Unreadable)
            .await
    }

    /// Records that the stored bytes of `message` no longer parse as a message: outcome
    /// `unparsable`, otherwise as [`Ingest::mark_insights_unreadable`].
    pub async fn mark_insights_unparsable(&self, message: MessageId) -> Result<(), IngestError> {
        self.mark_insights(message, InsightOutcome::Unparsable)
            .await
    }

    /// Computes the insights of a stored message again from its bytes and records them only when
    /// they changed: items, signals, kinds, auth facts, sender address, outcome, `truncated` or
    /// the fingerprint. Returns whether it wrote; always `false` without
    /// [`Ingest::with_insights`]. When it wrote, stage B runs and the message is reindexed as in
    /// [`Ingest::insights_for`], reusing the one blob read; searchable after
    /// [`Ingest::commit`].
    pub async fn refresh_insights(&self, message: MessageId) -> Result<bool, IngestError> {
        if self.insights.is_none() {
            return Ok(false);
        }
        let stored = self
            .store
            .get_message(message)
            .await?
            .ok_or(StoreError::NotFound("message"))?;
        let raw = self
            .blobs
            .get(&stored.blob_id)
            .await?
            .ok_or(StoreError::NotFound("message bytes"))?;
        let parsed = holtmail_mime::parse(&raw)?;
        Ok(self.insights_for(message, &raw, &parsed).await?.is_some())
    }

    /// Runs stage B for one stored message in place with a new
    /// [`DeriveContext`](holtmail_insights::DeriveContext) and stores the result through
    /// `CategoryStore::set_message_categories_queued`, replacing any earlier row, so the row then
    /// carries the current derive fingerprint and a changed kind or topic is queued for
    /// reindexing in the same transaction. Reads only the store, never a blob. Returns whether
    /// the effective kind or topics changed, a new row counting as changed; on `true` the caller
    /// wakes whatever drains the reindex queue. `false` without [`Ingest::with_categories`] or
    /// when the message has no insights row with outcome ok or partial, and then nothing is
    /// written. Fails with `StoreError::NotFound` for an unknown message.
    pub async fn derive_categories(&self, message: MessageId) -> Result<bool, IngestError> {
        let archive = self.archive_of(message).await?;
        let Some((deriver, policy)) = &self.categories else {
            return Ok(false);
        };
        let Some(row) = self.store.derive_input(message).await? else {
            return Ok(false);
        };
        let ctx = deriver.context(&*self.store, archive).await?;
        let write = deriver.derive(&ctx, &row);
        let changed = self
            .store
            .set_message_categories_queued(&[write], *policy)
            .await?;
        Ok(!changed.is_empty())
    }

    /// The same pipeline, also recording which alias each new message was forwarded to, by the
    /// first of `detectors` that recognises it. The sender a service hid is indexed as `From` too,
    /// so `from:` finds the message by either address.
    pub fn with_alias_detectors(mut self, detectors: Vec<Arc<dyn AliasDetector>>) -> Self {
        self.detectors = detectors;
        self
    }

    /// What the detectors find in `parsed`: the first hit, in the detectors' order.
    pub fn detect_alias(&self, parsed: &ParsedMessage) -> Option<MessageAlias> {
        if self.detectors.is_empty() {
            return None;
        }
        let mail = Mail {
            headers: parsed
                .headers
                .iter()
                .map(|h| Header::new(h.name.clone(), h.value.clone()))
                .collect(),
            from: parsed.from.clone(),
            to: parsed.to.clone(),
            cc: parsed.cc.clone(),
            reply_to: parsed.reply_to.clone(),
        };
        self.detectors
            .iter()
            .find_map(|d| d.detect(&mail))
            .map(|hit| MessageAlias {
                provider: hit.provider,
                alias: hit.alias,
                sender: hit.sender,
                reverse_alias: hit.reverse_alias,
                signed: hit.signed,
            })
    }

    /// Detects the alias of a stored message again and records it. When the answer changed, the
    /// message's stage-B row is removed with `CategoryStore::invalidate_derived`, so the next
    /// stage-B pass recomputes its trust, sender and categories, and the message is reindexed
    /// (searchable after [`Ingest::commit`]). Returns whether it changed; on `true` the caller
    /// must wake the stage-B pass (the server signals `WalkWake`). Reads the message's blob once.
    pub async fn refresh_alias(&self, message: MessageId) -> Result<bool, IngestError> {
        let stored = self
            .store
            .get_message(message)
            .await?
            .ok_or(StoreError::NotFound("message"))?;
        let raw = self
            .blobs
            .get(&stored.blob_id)
            .await?
            .ok_or(StoreError::NotFound("message bytes"))?;
        let parsed = holtmail_mime::parse(&raw)?;
        let found = self.detect_alias(&parsed);
        if self.store.message_alias(message).await? == found {
            return Ok(false);
        }
        self.store
            .set_message_alias(message, found.as_ref())
            .await?;
        self.store.invalidate_derived(&[message]).await?;
        self.index_parsed(message, &parsed, found.as_ref()).await?;
        Ok(true)
    }

    /// The same pipeline, also indexing text from attachments that are not inline.
    ///
    /// Text is extracted once per attachment blob and kept as a derived blob, so the same file in
    /// many messages, and later reindexing, reuse it. Files the extractor cannot read are
    /// recorded and not tried again; timeouts are tried again next time. Extraction never fails
    /// ingest.
    pub fn with_extractor(mut self, extractor: Arc<dyn Extractor>) -> Self {
        self.extractor = Some(extractor);
        self
    }

    /// Archives `raw` into `archive` at `placement`.
    ///
    /// A new message's calendar parts (iMIP invitations, updates, replies and cancellations, and
    /// attached `.ics` files) become versions of events and tasks in the archive's "From mail"
    /// calendar, keyed by `UID` and linked to the message; see [`Ingest::record_invitations`].
    /// Idempotent: the same bytes again only update where the message was seen. Bytes deleted
    /// from the archive on purpose are not stored again: [`IngestError::Deleted`], and their
    /// folder and UID are remembered so the source is not asked for them again. The raw bytes are
    /// durable before any metadata refers to them, so a crash never leaves a message without its
    /// bytes. With [`Ingest::with_insights`] and [`Ingest::with_categories`], insights and then
    /// stage B are recorded as those say; neither ever fails ingest. The index change becomes
    /// searchable after [`Ingest::commit`]; callers commit once per batch.
    pub async fn ingest(
        &self,
        archive: ArchiveId,
        raw: Bytes,
        placement: &Placement,
    ) -> Result<Ingested, IngestError> {
        let parsed = holtmail_mime::parse(&raw)?;
        let size = raw.len() as u64;
        if self.store.is_deleted(archive, &BlobId::of(&raw)).await? {
            if let (Some(folder), Some(uid)) = (placement.folder, placement.uid) {
                self.store.record_deleted_location(folder, uid).await?;
            }
            return Err(IngestError::Deleted);
        }
        let blob_id = self.blobs.put(raw.clone()).await?;

        let inserted = match self.store.find_by_blob(archive, &blob_id).await? {
            Some(message_id) => {
                let message = self
                    .store
                    .get_message(message_id)
                    .await?
                    .ok_or(StoreError::NotFound("message"))?;
                Ingested {
                    message_id,
                    thread_id: message.thread_id,
                    created: false,
                }
            }
            None => {
                let attachments = self.store_attachments(&parsed).await?;
                let inserted = self
                    .store
                    .insert_message(new_message(
                        archive,
                        blob_id,
                        size,
                        &parsed,
                        placement,
                        attachments,
                    ))
                    .await?;
                Ingested {
                    message_id: inserted.message_id,
                    thread_id: inserted.thread_id,
                    created: inserted.created,
                }
            }
        };

        let alias = self.detect_alias(&parsed);
        if inserted.created {
            self.store
                .set_message_alias(inserted.message_id, alias.as_ref())
                .await?;
            self.invitations(
                archive,
                inserted.message_id,
                parsed.sent_at.map(Timestamp::from_secs),
                &parsed.calendars,
            )
            .await?;
        }
        self.insights_on_ingest(archive, inserted, &parsed).await;
        if let Some(folder) = placement.folder {
            self.store
                .record_location(
                    inserted.message_id,
                    folder,
                    placement.uid,
                    &placement.flags,
                    Timestamp::now(),
                )
                .await?;
        }
        if let Some(labels) = &placement.labels {
            self.store.set_labels(inserted.message_id, labels).await?;
        }
        if inserted.created {
            for observer in &self.observers {
                observer
                    .added(archive, inserted.message_id, &raw, &parsed, placement)
                    .await;
            }
        }
        self.categorize(inserted.message_id, archive).await;
        let alias = if inserted.created {
            alias
        } else {
            self.store.message_alias(inserted.message_id).await?
        };
        self.index_parsed(inserted.message_id, &parsed, alias.as_ref())
            .await?;
        Ok(inserted)
    }

    /// Rebuilds the index document of a message from its stored bytes, e.g. after its folders,
    /// flags or labels changed. With [`Ingest::with_insights`], a missing insights row, or one
    /// with another fingerprint, is computed and stored first, after queueing the message for
    /// reindexing as [`Ingest::insights_for`] does, then stage B as [`Ingest::with_categories`]
    /// says. Searchable after [`Ingest::commit`].
    pub async fn reindex(&self, message: MessageId) -> Result<(), IngestError> {
        let stored = self
            .store
            .get_message(message)
            .await?
            .ok_or(StoreError::NotFound("message"))?;
        let raw = self
            .blobs
            .get(&stored.blob_id)
            .await?
            .ok_or(StoreError::NotFound("message bytes"))?;
        let parsed = holtmail_mime::parse(&raw)?;
        if let Some(runner) = &self.insights {
            let current = runner.extract_fp();
            let stale = self
                .store
                .insight_view(message)
                .await?
                .is_none_or(|view| view.insights.fingerprint != current);
            if stale {
                let row = compute_insights(runner, message, stored.archive_id, &parsed);
                self.store.queue_reindex(&[message]).await?;
                self.store.set_message_insights(message, &row).await?;
                self.categorize(message, stored.archive_id).await;
            }
        }
        self.reindex_parsed(message, &parsed).await
    }

    /// Rebuilds the index document of `message` from `parsed`, its stored bytes already parsed,
    /// with its current alias, folders, flags and labels. Reads no blob and writes no insights.
    /// Searchable after [`Ingest::commit`].
    pub async fn reindex_parsed(
        &self,
        message: MessageId,
        parsed: &ParsedMessage,
    ) -> Result<(), IngestError> {
        let alias = self.store.message_alias(message).await?;
        self.index_parsed(message, parsed, alias.as_ref()).await
    }

    /// Reads the text of the attachments of `message` that `which` picks again, ignoring what
    /// was kept from an earlier reading, deletes kept text nothing else uses, and rebuilds the
    /// message's index document. Returns how many attachments were picked. Searchable after
    /// [`Ingest::commit`].
    pub async fn refresh_attachment_text(
        &self,
        message: MessageId,
        which: &(dyn Fn(&Attachment) -> bool + Send + Sync),
    ) -> Result<u32, IngestError> {
        let mut picked = 0;
        for attachment in self.store.attachments(message).await? {
            if attachment.inline || !which(&attachment) {
                continue;
            }
            picked += 1;
            if let Some(orphan) = self
                .store
                .forget_derived(&attachment.blob_id, DerivedKind::Text)
                .await?
            {
                self.blobs.delete(&orphan).await?;
            }
        }
        if picked > 0 {
            self.reindex(message).await?;
        }
        Ok(picked)
    }

    /// Makes everything ingested so far searchable.
    pub async fn commit(&self) -> Result<(), IngestError> {
        Ok(self.index.commit().await?)
    }

    async fn store_attachments(
        &self,
        parsed: &ParsedMessage,
    ) -> Result<Vec<Attachment>, IngestError> {
        let mut out = Vec::with_capacity(parsed.attachments.len());
        for (ordinal, part) in parsed.attachments.iter().enumerate() {
            let blob_id = self.blobs.put(part.content.clone()).await?;
            out.push(Attachment {
                ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
                blob_id,
                filename: part.filename.clone(),
                content_type: part.content_type.clone(),
                size: part.content.len() as u64,
                content_id: part.content_id.clone(),
                inline: part.inline,
            });
        }
        Ok(out)
    }

    async fn index_parsed(
        &self,
        message: MessageId,
        parsed: &ParsedMessage,
        alias: Option<&MessageAlias>,
    ) -> Result<(), IngestError> {
        let stored = self
            .store
            .get_message(message)
            .await?
            .ok_or(StoreError::NotFound("message"))?;
        let mut folders = Vec::new();
        let mut flags = Vec::new();
        for location in self.store.locations(message).await? {
            if location.removed_at.is_some() {
                continue;
            }
            if let Some(folder) = self.store.get_folder(location.folder_id).await? {
                folders.push(folder.path);
                if let Some(role) = folder.role {
                    folders.push(role.as_str().to_owned());
                }
            }
            flags.extend(location.flags);
        }
        folders.sort();
        folders.dedup();
        flags.sort();
        flags.dedup();

        let (insights, categories) = self.indexed_insights(message).await?;
        let attachments: Vec<_> = parsed.attachments.iter().filter(|a| !a.inline).collect();
        let document = IndexDocument {
            message_id: message,
            archive_id: stored.archive_id,
            thread_id: stored.thread_id,
            subject: parsed.subject.clone().unwrap_or_default(),
            from: parsed
                .from
                .iter()
                .chain(alias.and_then(|a| a.sender.as_ref()))
                .cloned()
                .collect(),
            recipients: [&parsed.to, &parsed.cc, &parsed.bcc]
                .into_iter()
                .flatten()
                .cloned()
                .collect(),
            body: truncate(parsed.text.as_deref().unwrap_or_default()),
            attachment_names: attachments
                .iter()
                .filter_map(|a| a.filename.clone())
                .collect(),
            attachment_text: self.attachment_text(message, parsed).await?,
            folders,
            flags,
            labels: self.store.labels(message).await?,
            date: stored.date,
            has_attachments: !attachments.is_empty(),
            insights,
            categories,
        };
        self.index.put(vec![document]).await?;
        Ok(())
    }
}

impl Ingest {
    async fn indexed_insights(
        &self,
        message: MessageId,
    ) -> Result<(Vec<String>, Vec<String>), IngestError> {
        let Some(view) = self.store.insight_view(message).await? else {
            return Ok((Vec::new(), Vec::new()));
        };
        let insights = match view.insights.outcome {
            InsightOutcome::Ok | InsightOutcome::Partial => {
                KindTags::from_bits(view.insights.kinds)
                    .iter()
                    .map(|tag| tag.name().to_owned())
                    .collect()
            }
            InsightOutcome::Unparsable | InsightOutcome::Unreadable => Vec::new(),
        };
        let categories = view
            .categories
            .map(|c| {
                std::iter::once(format!("kind:{}", c.row.kind.name()))
                    .chain(c.row.topics.iter().map(|t| format!("topic:{}", t.name())))
                    .collect()
            })
            .unwrap_or_default();
        Ok((insights, categories))
    }

    async fn archive_of(&self, message: MessageId) -> Result<ArchiveId, IngestError> {
        Ok(self
            .store
            .get_message(message)
            .await?
            .ok_or(StoreError::NotFound("message"))?
            .archive_id)
    }

    async fn insights_on_ingest(
        &self,
        archive: ArchiveId,
        inserted: Ingested,
        parsed: &ParsedMessage,
    ) {
        let Some(runner) = &self.insights else {
            return;
        };
        let message = inserted.message_id;
        if !inserted.created {
            match self.store.insight_view(message).await {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(_) => {
                    tracing::warn!(%message, "reading message insights failed");
                    return;
                }
            }
        }
        let row = compute_insights(runner, message, archive, parsed);
        if self
            .store
            .set_message_insights(message, &row)
            .await
            .is_err()
        {
            tracing::warn!(%message, "storing message insights failed");
        }
    }

    async fn categorize(&self, message: MessageId, archive: ArchiveId) {
        let Some((deriver, policy)) = &self.categories else {
            return;
        };
        if self
            .derive_one(deriver, *policy, message, archive)
            .await
            .is_err()
        {
            tracing::warn!(%message, "deriving message categories failed");
        }
    }

    async fn derive_one(
        &self,
        deriver: &Deriver,
        policy: CategoryPolicy,
        message: MessageId,
        archive: ArchiveId,
    ) -> Result<(), StoreError> {
        let Some(row) = self.store.derive_input(message).await? else {
            return Ok(());
        };
        let ctx = deriver.context(&*self.store, archive).await?;
        let write = deriver.derive(&ctx, &row);
        self.store.set_message_categories(&[write], policy).await?;
        Ok(())
    }

    async fn insights_changed(&self, row: &InsightRow) -> Result<bool, IngestError> {
        Ok(self
            .store
            .insight_view(row.message)
            .await?
            .is_none_or(|view| {
                InsightRow {
                    computed_at: row.computed_at,
                    ..view.insights
                } != *row
            }))
    }

    async fn queued_seq(&self, archive: ArchiveId, message: MessageId) -> Result<u64, IngestError> {
        let before = message
            .as_uuid()
            .as_u128()
            .checked_sub(1)
            .map(|n| MessageId::from(Uuid::from_u128(n)));
        Ok(self
            .store
            .reindex_queue(archive, before, 1)
            .await?
            .into_iter()
            .find(|(id, _)| *id == message)
            .map_or(0, |(_, seq)| seq))
    }

    async fn mark_insights(
        &self,
        message: MessageId,
        outcome: InsightOutcome,
    ) -> Result<(), IngestError> {
        let Some(runner) = &self.insights else {
            return Ok(());
        };
        let archive = self.archive_of(message).await?;
        let mut empty = runner.extract_input(&Default::default(), false);
        empty.items.items.clear();
        let row = InsightRow {
            outcome,
            truncated: false,
            from_address: None,
            signals: 0,
            kinds: 0,
            ..empty.into_row(message, archive, runner.extract_fp(), Timestamp::now())
        };
        self.store.set_message_insights(message, &row).await?;
        Ok(())
    }

    async fn attachment_text(
        &self,
        message: MessageId,
        parsed: &ParsedMessage,
    ) -> Result<String, IngestError> {
        let Some(extractor) = &self.extractor else {
            return Ok(String::new());
        };
        let mut out = String::new();
        for attachment in self.store.attachments(message).await? {
            if attachment.inline || out.len() >= MAX_ATTACHMENT_TEXT {
                continue;
            }
            let cached = match self
                .store
                .derived(&attachment.blob_id, DerivedKind::Text)
                .await?
            {
                Some(Derived::Nothing) => continue,
                Some(Derived::Blob(blob)) => self.blobs.get(&blob).await?,
                None => None,
            };
            let text = match cached {
                Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                None => match self.extract(&**extractor, &attachment, parsed).await? {
                    Some(text) => text,
                    None => continue,
                },
            };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text);
        }
        Ok(holtmail_extract::cut(out, MAX_ATTACHMENT_TEXT))
    }

    async fn extract(
        &self,
        extractor: &dyn Extractor,
        attachment: &Attachment,
        parsed: &ParsedMessage,
    ) -> Result<Option<String>, IngestError> {
        if attachment.size > holtmail_extract::MAX_INPUT as u64 {
            self.store
                .set_derived(&attachment.blob_id, DerivedKind::Text, &Derived::Nothing)
                .await?;
            return Ok(None);
        }
        let content = match parsed.attachments.get(attachment.ordinal as usize) {
            Some(part) => part.content.clone(),
            None => match self.blobs.get(&attachment.blob_id).await? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
        };
        let file = File {
            content_type: attachment.content_type.clone(),
            filename: attachment.filename.clone(),
            content,
        };
        let derived = match extractor.extract(&file).await {
            Ok(None) | Err(ExtractError::TimedOut) => return Ok(None),
            Ok(Some(text)) if !text.trim().is_empty() => {
                let blob = self.blobs.put(Bytes::from(text.clone())).await?;
                self.store
                    .set_derived(&attachment.blob_id, DerivedKind::Text, &Derived::Blob(blob))
                    .await?;
                return Ok(Some(text));
            }
            Ok(Some(_)) | Err(ExtractError::TooLarge | ExtractError::Unreadable(_)) => {
                Derived::Nothing
            }
        };
        self.store
            .set_derived(&attachment.blob_id, DerivedKind::Text, &derived)
            .await?;
        Ok(None)
    }
}

fn new_message(
    archive: ArchiveId,
    blob_id: holtmail_blob::BlobId,
    size: u64,
    parsed: &ParsedMessage,
    placement: &Placement,
    attachments: Vec<Attachment>,
) -> NewMessage {
    let mut parent_ids = parsed.references.clone();
    parent_ids.extend(parsed.in_reply_to.iter().cloned());
    NewMessage {
        archive_id: archive,
        blob_id,
        size,
        rfc_message_id: parsed.message_id.clone(),
        parent_ids,
        subject: parsed.subject.clone(),
        from: parsed.from.clone(),
        to: parsed.to.clone(),
        cc: parsed.cc.clone(),
        bcc: parsed.bcc.clone(),
        sent_at: parsed.sent_at.map(Timestamp::from_secs),
        received_at: placement.received_at,
        preview: parsed.preview.clone(),
        attachments,
    }
}

fn compute_insights(
    runner: &Runner,
    message: MessageId,
    archive: ArchiveId,
    parsed: &ParsedMessage,
) -> InsightRow {
    holtmail_insights::with_message(message, || runner.extract(parsed)).into_row(
        message,
        archive,
        runner.extract_fp(),
        Timestamp::now(),
    )
}

fn truncate(text: &str) -> String {
    if text.len() <= MAX_INDEXED_TEXT {
        return text.to_owned();
    }
    let mut end = MAX_INDEXED_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use holtmail_blob_fs::FsBlobStore;
    use holtmail_category::{MessageKind, TrustState};
    use holtmail_core::CollectionId;
    use holtmail_index::{Clause, SearchQuery, SortOrder};
    use holtmail_index_tantivy::TantivyIndex;
    use holtmail_store::{AccountKind, NewAccount};
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    struct Fixture {
        ingest: Ingest,
        store: Arc<dyn Store>,
        index: Arc<dyn SearchIndex>,
        blobs: Arc<dyn BlobStore>,
        archive: ArchiveId,
        inbox: FolderId,
        _dir: tempfile::TempDir,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> =
            Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let index: Arc<dyn SearchIndex> = Arc::new(TantivyIndex::in_memory().unwrap());
        let archive = store.create_archive("Test").await.unwrap().id;
        let account = store
            .create_account(NewAccount {
                archive_id: archive,
                kind: AccountKind::Import,
                name: "Import".into(),
                address: None,
                settings: Default::default(),
                sealed_secret: None,
                enabled: false,
            })
            .await
            .unwrap();
        let inbox = store
            .upsert_folder(account.id, "INBOX", Some(holtmail_store::FolderRole::Inbox))
            .await
            .unwrap()
            .id;
        Fixture {
            ingest: Ingest::new(blobs.clone(), store.clone(), index.clone()),
            store,
            index,
            blobs,
            archive,
            inbox,
            _dir: dir,
        }
    }

    const REPLY: &[u8] = b"From: Bob <bob@example.org>\r\nTo: ada@example.com\r\nSubject: Re: Lunch\r\nMessage-ID: <r1@example.org>\r\nIn-Reply-To: <l1@example.com>\r\nDate: Fri, 14 Mar 2025 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nNoon works.\r\n--b\r\nContent-Type: text/plain; name=menu.txt\r\nContent-Disposition: attachment; filename=menu.txt\r\n\r\nSoup\r\n--b--\r\n";
    const ROOT: &[u8] = b"From: Ada <ada@example.com>\r\nTo: bob@example.org\r\nSubject: Lunch\r\nMessage-ID: <l1@example.com>\r\nDate: Fri, 14 Mar 2025 09:00:00 +0000\r\n\r\nShall we?\r\n";

    fn in_inbox(f: &Fixture, uid: u64, flags: &[&str]) -> Placement {
        Placement {
            folder: Some(f.inbox),
            uid: Some(uid),
            flags: flags.iter().map(|s| s.to_string()).collect(),
            labels: None,
            received_at: None,
            live: false,
        }
    }

    type Heard = (MessageId, bool, Option<String>, usize);

    #[derive(Default)]
    struct Seen(std::sync::Mutex<Vec<Heard>>);

    #[async_trait]
    impl Observer for Seen {
        async fn added(
            &self,
            _: ArchiveId,
            message: MessageId,
            raw: &[u8],
            parsed: &ParsedMessage,
            placement: &Placement,
        ) {
            self.0.lock().unwrap().push((
                message,
                placement.live,
                parsed.message_id.clone(),
                raw.len(),
            ));
        }
    }

    #[tokio::test]
    async fn observers_hear_of_added_messages_only() {
        let f = fixture().await;
        let seen = Arc::new(Seen::default());
        let ingest = f.ingest.clone().with_observer(seen.clone());
        let live = Placement {
            live: true,
            ..in_inbox(&f, 1, &[])
        };
        let first = ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &live)
            .await
            .unwrap();
        ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 2, &[]))
            .await
            .unwrap();
        let reply = ingest
            .ingest(f.archive, Bytes::from_static(REPLY), &in_inbox(&f, 3, &[]))
            .await
            .unwrap();
        assert_eq!(
            *seen.0.lock().unwrap(),
            [
                (
                    first.message_id,
                    true,
                    Some("l1@example.com".into()),
                    ROOT.len()
                ),
                (
                    reply.message_id,
                    false,
                    Some("r1@example.org".into()),
                    REPLY.len()
                ),
            ]
        );
    }

    async fn find(f: &Fixture, clauses: Vec<Clause>) -> Vec<MessageId> {
        f.index
            .search(
                f.archive,
                &SearchQuery { clauses },
                SortOrder::Newest,
                0,
                50,
            )
            .await
            .unwrap()
            .hits
    }

    const FORWARDED: &[u8] = b"X-SimpleLogin-Type: Forward\r\nX-SimpleLogin-Envelope-To: shop@alias.example.org\r\nFrom: \"Grace Hopper - grace at example.com\" <ra_q1@sl.example.org>\r\nTo: shop@alias.example.org\r\nSubject: Parcel\r\nMessage-ID: <p1@example.com>\r\nDate: Fri, 14 Mar 2025 11:00:00 +0000\r\n\r\nOn its way.\r\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn records_aliases_and_finds_the_hidden_sender() {
        let mut f = fixture().await;
        let plain = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        let forwarded = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(FORWARDED),
                &in_inbox(&f, 2, &[]),
            )
            .await
            .unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            f.store.message_alias(forwarded.message_id).await.unwrap(),
            None
        );

        f.ingest = f.ingest.clone().with_alias_detectors(vec![Arc::new(
            holtmail_alias_simplelogin::SimpleLoginDetector::default(),
        )]);
        assert!(!f.ingest.refresh_alias(plain.message_id).await.unwrap());
        assert!(f.ingest.refresh_alias(forwarded.message_id).await.unwrap());
        assert!(!f.ingest.refresh_alias(forwarded.message_id).await.unwrap());
        f.ingest.commit().await.unwrap();
        let alias = f
            .store
            .message_alias(forwarded.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alias.alias, "shop@alias.example.org");
        assert_eq!(alias.sender.unwrap().email, "grace@example.com");
        assert_eq!(
            find(&f, vec![Clause::From("grace@example.com".into())]).await,
            vec![forwarded.message_id]
        );
        assert_eq!(
            find(&f, vec![Clause::From("ra_q1@sl.example.org".into())]).await,
            vec![forwarded.message_id]
        );

        f.ingest.reindex(forwarded.message_id).await.unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            find(&f, vec![Clause::From("grace".into())]).await,
            vec![forwarded.message_id],
            "reindexing keeps the hidden sender"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stores_threads_and_indexes_a_message() {
        let f = fixture().await;
        let reply = f
            .ingest
            .ingest(f.archive, Bytes::from_static(REPLY), &in_inbox(&f, 2, &[]))
            .await
            .unwrap();
        let root = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(ROOT),
                &in_inbox(&f, 1, &["\\Seen"]),
            )
            .await
            .unwrap();
        f.ingest.commit().await.unwrap();

        assert!(reply.created && root.created);
        assert_eq!(
            reply.thread_id,
            f.store
                .get_message(root.message_id)
                .await
                .unwrap()
                .unwrap()
                .thread_id
        );
        let stored = f
            .store
            .get_message(reply.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.attachment_count, 1);
        assert_eq!(stored.date, Timestamp::from_secs(1_741_946_400));
        assert_eq!(
            f.blobs.get(&stored.blob_id).await.unwrap().unwrap(),
            Bytes::from_static(REPLY)
        );
        let attachment = &f.store.attachments(reply.message_id).await.unwrap()[0];
        assert_eq!(
            f.blobs.get(&attachment.blob_id).await.unwrap().unwrap(),
            Bytes::from_static(b"Soup")
        );

        assert_eq!(
            find(&f, vec![Clause::Text("noon".into())]).await,
            [reply.message_id]
        );
        assert_eq!(
            find(&f, vec![Clause::Filename("menu".into())]).await,
            [reply.message_id]
        );
        assert_eq!(find(&f, vec![Clause::In("inbox".into())]).await.len(), 2);
        assert_eq!(
            find(&f, vec![Clause::Flag("\\Seen".into())]).await,
            [root.message_id]
        );
    }

    struct Counting(
        std::sync::atomic::AtomicUsize,
        holtmail_extract_basic::BasicExtractor,
    );

    #[async_trait::async_trait]
    impl Extractor for Counting {
        async fn extract(&self, file: &File) -> Result<Option<String>, ExtractError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.1.extract(file).await
        }
    }

    const FORWARD: &[u8] = b"From: Carol <carol@example.org>\r\nTo: ada@example.com\r\nSubject: Fwd: menu\r\nMessage-ID: <f1@example.org>\r\nDate: Sat, 15 Mar 2025 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--b\r\nContent-Type: application/octet-stream; name=menu.txt\r\nContent-Disposition: attachment; filename=menu.txt\r\n\r\nSoup\r\n--b\r\nContent-Type: application/pdf; name=broken.pdf\r\nContent-Disposition: attachment; filename=broken.pdf\r\n\r\nnot a pdf\r\n--b--\r\n";

    struct Fixed(&'static str);

    #[async_trait::async_trait]
    impl Extractor for Fixed {
        async fn extract(&self, file: &File) -> Result<Option<String>, ExtractError> {
            Ok(Some(format!(
                "{} {}",
                self.0,
                file.filename.as_deref().unwrap_or_default()
            )))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_picked_attachments_again() {
        let f = fixture().await;
        let old = f.ingest.clone().with_extractor(Arc::new(Fixed("oldword")));
        let forward = old
            .ingest(
                f.archive,
                Bytes::from_static(FORWARD),
                &in_inbox(&f, 1, &[]),
            )
            .await
            .unwrap();
        old.commit().await.unwrap();
        let menu = f.store.attachments(forward.message_id).await.unwrap()[0].clone();
        let Some(Derived::Blob(kept)) = f
            .store
            .derived(&menu.blob_id, DerivedKind::Text)
            .await
            .unwrap()
        else {
            panic!("no text kept");
        };

        let new = f.ingest.clone().with_extractor(Arc::new(Fixed("newword")));
        let picked = new
            .refresh_attachment_text(forward.message_id, &|a| {
                a.filename.as_deref() == Some("menu.txt")
            })
            .await
            .unwrap();
        new.commit().await.unwrap();
        assert_eq!(picked, 1);
        assert!(
            !f.blobs.contains(&kept).await.unwrap(),
            "old text left behind"
        );
        assert_eq!(
            find(&f, vec![Clause::Text("newword".into())]).await,
            [forward.message_id]
        );
        let pdf = &f.store.attachments(forward.message_id).await.unwrap()[1];
        assert!(
            matches!(
                f.store
                    .derived(&pdf.blob_id, DerivedKind::Text)
                    .await
                    .unwrap(),
                Some(Derived::Blob(_))
            ),
            "an attachment not picked was forgotten"
        );
        assert_eq!(
            find(&f, vec![Clause::Text("oldword".into())]).await,
            [forward.message_id],
            "the attachment not picked lost its text"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn indexes_attachment_text_once_per_file() {
        let f = fixture().await;
        let counter = Arc::new(Counting(Default::default(), Default::default()));
        let ingest = f.ingest.clone().with_extractor(counter.clone());
        let reply = ingest
            .ingest(f.archive, Bytes::from_static(REPLY), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        let forward = ingest
            .ingest(
                f.archive,
                Bytes::from_static(FORWARD),
                &in_inbox(&f, 2, &[]),
            )
            .await
            .unwrap();
        ingest.commit().await.unwrap();
        let calls = || counter.0.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(calls(), 2, "the shared file was read twice");

        let results = f
            .index
            .search(
                f.archive,
                &SearchQuery {
                    clauses: vec![Clause::Text("soup".into())],
                },
                SortOrder::Newest,
                0,
                50,
            )
            .await
            .unwrap();
        assert_eq!(results.hits, [forward.message_id, reply.message_id]);
        assert_eq!(results.attachment_hits, results.hits);

        let broken = &f.store.attachments(forward.message_id).await.unwrap()[1];
        assert_eq!(
            f.store
                .derived(&broken.blob_id, DerivedKind::Text)
                .await
                .unwrap(),
            Some(Derived::Nothing)
        );

        ingest.reindex(reply.message_id).await.unwrap();
        ingest.reindex(forward.message_id).await.unwrap();
        ingest.commit().await.unwrap();
        assert_eq!(calls(), 2, "reindexing read the files again");
        assert_eq!(
            find(&f, vec![Clause::Text("soup".into())]).await.len(),
            2,
            "reindexing lost the attachment text"
        );

        f.ingest.reindex(reply.message_id).await.unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            find(&f, vec![Clause::Text("soup".into())]).await,
            [forward.message_id],
            "without an extractor no attachment text is indexed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_bytes_only_add_a_location() {
        let f = fixture().await;
        let first = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        let account = f
            .store
            .get_folder(f.inbox)
            .await
            .unwrap()
            .unwrap()
            .account_id;
        let archive_folder = f
            .store
            .upsert_folder(account, "Archive", None)
            .await
            .unwrap()
            .id;
        let again = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(ROOT),
                &Placement {
                    folder: Some(archive_folder),
                    uid: Some(7),
                    ..Placement::default()
                },
            )
            .await
            .unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(again.message_id, first.message_id);
        assert!(!again.created);
        assert_eq!(f.store.locations(first.message_id).await.unwrap().len(), 2);
        assert_eq!(
            find(&f, vec![Clause::In("archive".into())]).await,
            [first.message_id]
        );
        assert_eq!(f.store.archive_stats(f.archive).await.unwrap().messages, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_bytes_that_are_not_mail() {
        let f = fixture().await;
        let result = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(b"just text"),
                &Placement::default(),
            )
            .await;
        assert!(matches!(result, Err(IngestError::Mime(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reindex_picks_up_new_flags() {
        let f = fixture().await;
        let ingested = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        f.store
            .record_location(
                ingested.message_id,
                f.inbox,
                Some(1),
                &["\\Flagged".into()],
                Timestamp::now(),
            )
            .await
            .unwrap();
        f.ingest.reindex(ingested.message_id).await.unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            find(&f, vec![Clause::Flag("\\Flagged".into())]).await,
            [ingested.message_id]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stores_and_indexes_labels() {
        let f = fixture().await;
        let mut placement = in_inbox(&f, 1, &[]);
        placement.labels = Some(vec!["Inbox".into(), "Projects/2025".into()]);
        let ingested = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &placement)
            .await
            .unwrap();
        f.ingest.commit().await.unwrap();
        assert_eq!(
            find(&f, vec![Clause::Label("projects/2025".into())]).await,
            [ingested.message_id]
        );

        let mut again = in_inbox(&f, 1, &[]);
        again.labels = None;
        f.ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &again)
            .await
            .unwrap();
        assert_eq!(
            f.store.labels(ingested.message_id).await.unwrap(),
            ["Inbox", "Projects/2025"],
            "a placement without labels dropped them"
        );

        f.store
            .set_labels(ingested.message_id, &["Done".into()])
            .await
            .unwrap();
        f.ingest.reindex(ingested.message_id).await.unwrap();
        f.ingest.commit().await.unwrap();
        assert!(
            find(&f, vec![Clause::Label("inbox".into())])
                .await
                .is_empty()
        );
        assert_eq!(
            find(&f, vec![Clause::Label("done".into())]).await,
            [ingested.message_id]
        );
    }

    struct Quiet(&'static str);

    impl holtmail_insight::InsightExtractor for Quiet {
        fn id(&self) -> &str {
            "quiet"
        }

        fn version(&self) -> String {
            self.0.into()
        }

        fn extract(
            &self,
            _input: &holtmail_insight::InsightInput,
        ) -> Vec<holtmail_insight::Insight> {
            Vec::new()
        }
    }

    fn runner(version: &'static str) -> Arc<Runner> {
        Arc::new(Runner::new(vec![Arc::new(Quiet(version))]))
    }

    async fn insights(f: &Fixture, message: MessageId) -> Option<InsightRow> {
        f.store
            .insight_view(message)
            .await
            .unwrap()
            .map(|view| view.insights)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_messages_get_insights_and_known_ones_are_repaired() {
        let f = fixture().await;
        let runner = runner("1");
        let with = f.ingest.clone().with_insights(runner.clone());
        let created = with
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        let row = insights(&f, created.message_id).await.expect("no row");
        assert_eq!(row.fingerprint, runner.extract_fp());
        assert_eq!(row.outcome, InsightOutcome::Ok);
        assert_eq!(row.archive, f.archive);

        let plain = f
            .ingest
            .ingest(f.archive, Bytes::from_static(REPLY), &in_inbox(&f, 2, &[]))
            .await
            .unwrap();
        assert_eq!(insights(&f, plain.message_id).await, None);
        let again = with
            .ingest(f.archive, Bytes::from_static(REPLY), &in_inbox(&f, 2, &[]))
            .await
            .unwrap();
        assert!(!again.created);
        assert!(
            insights(&f, plain.message_id).await.is_some(),
            "known bytes without a row were not repaired"
        );

        let mut marked = row.clone();
        marked.computed_at = Timestamp::from_secs(1);
        f.store
            .set_message_insights(created.message_id, &marked)
            .await
            .unwrap();
        with.ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        assert_eq!(
            insights(&f, created.message_id).await,
            Some(marked),
            "known bytes with a row were written again"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_insights_writes_only_on_change() {
        let f = fixture().await;
        assert!(!f.ingest.refresh_insights(MessageId::new()).await.unwrap());
        let old = f.ingest.clone().with_insights(runner("1"));
        let ingested = old
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        assert!(!old.refresh_insights(ingested.message_id).await.unwrap());

        let next = runner("2");
        let new = f.ingest.clone().with_insights(next.clone());
        assert!(new.refresh_insights(ingested.message_id).await.unwrap());
        assert_eq!(
            insights(&f, ingested.message_id).await.unwrap().fingerprint,
            next.extract_fp()
        );
        assert!(!new.refresh_insights(ingested.message_id).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reindex_fills_missing_and_stale_insights() {
        let f = fixture().await;
        let ingested = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        f.ingest.reindex(ingested.message_id).await.unwrap();
        assert_eq!(insights(&f, ingested.message_id).await, None);

        let runner = runner("1");
        let with = f.ingest.clone().with_insights(runner.clone());
        with.reindex(ingested.message_id).await.unwrap();
        let mut row = insights(&f, ingested.message_id).await.expect("no row");
        assert_eq!(row.fingerprint, runner.extract_fp());

        row.computed_at = Timestamp::from_secs(1);
        f.store
            .set_message_insights(ingested.message_id, &row)
            .await
            .unwrap();
        with.reindex(ingested.message_id).await.unwrap();
        assert_eq!(
            insights(&f, ingested.message_id).await.as_ref(),
            Some(&row),
            "a current row was rewritten"
        );

        row.fingerprint = "old".into();
        f.store
            .set_message_insights(ingested.message_id, &row)
            .await
            .unwrap();
        with.reindex(ingested.message_id).await.unwrap();
        with.commit().await.unwrap();
        assert_eq!(
            insights(&f, ingested.message_id).await.unwrap().fingerprint,
            runner.extract_fp()
        );
        assert_eq!(
            find(&f, vec![Clause::Text("shall".into())]).await,
            [ingested.message_id]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn marks_unreadable_and_unparsable_messages() {
        let f = fixture().await;
        let ingested = f
            .ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        f.ingest
            .mark_insights_unreadable(ingested.message_id)
            .await
            .unwrap();
        assert_eq!(insights(&f, ingested.message_id).await, None);

        let runner = runner("1");
        let with = f.ingest.clone().with_insights(runner.clone());
        let fp = runner.extract_fp();
        for (outcome, mark) in [
            (InsightOutcome::Unreadable, true),
            (InsightOutcome::Unparsable, false),
        ] {
            if mark {
                with.mark_insights_unreadable(ingested.message_id)
                    .await
                    .unwrap();
            } else {
                with.mark_insights_unparsable(ingested.message_id)
                    .await
                    .unwrap();
            }
            let row = insights(&f, ingested.message_id).await.unwrap();
            assert_eq!(row.outcome, outcome);
            assert_eq!(row.fingerprint, fp);
            assert_eq!(row.kinds, 0);
            assert!(
                holtmail_insight::Items::from_json(&row.items)
                    .unwrap()
                    .items
                    .items
                    .is_empty()
            );
            assert!(
                f.store
                    .messages_needing_insights(f.archive, &fp, None, 10)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(matches!(
            with.mark_insights_unparsable(MessageId::new()).await,
            Err(IngestError::Store(StoreError::NotFound(_)))
        ));
    }

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn card(name: &str) -> Bytes {
        Bytes::from(format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{name}\r\nFN:{name}\r\nEMAIL:{name}@example.com\r\nPHOTO;ENCODING=b;TYPE=PNG:{PNG}\r\nEND:VCARD\r\n"
        ))
    }

    fn item(collection: CollectionId, remote: &str, raw: Bytes) -> IncomingItem {
        IncomingItem {
            collection,
            remote_id: remote.into(),
            kind: holtmail_pim::ItemKind::Contact,
            format: holtmail_pim::Format::of(holtmail_pim::ItemKind::Contact),
            raw,
            etag: None,
            seen_at: Timestamp::from_secs(1_700_000_000),
        }
    }

    #[tokio::test]
    async fn items_keep_raw_bytes_and_share_pictures() {
        let f = fixture().await;
        let book = f
            .store
            .upsert_collection(holtmail_store::NewCollection {
                archive_id: f.archive,
                account_id: None,
                kind: holtmail_store::CollectionKind::Contacts,
                remote_id: "people.vcf".into(),
                name: "People".into(),
                color: None,
            })
            .await
            .unwrap()
            .id;
        let ada = f
            .ingest
            .ingest_item(item(book, "ada", card("Ada")))
            .await
            .unwrap();
        let bo = f
            .ingest
            .ingest_item(item(book, "bo", card("Bo")))
            .await
            .unwrap();
        let ada_versions = f.store.item_versions(ada.item_id).await.unwrap();
        let bo_versions = f.store.item_versions(bo.item_id).await.unwrap();
        let photo = ada_versions[0].photo_blob_id.expect("picture split off");
        assert_eq!(
            bo_versions[0].photo_blob_id,
            Some(photo),
            "one blob per picture"
        );
        assert!(
            f.blobs
                .get(&photo)
                .await
                .unwrap()
                .unwrap()
                .starts_with(b"\x89PNG")
        );
        assert_eq!(
            f.blobs
                .get(&ada_versions[0].blob_id)
                .await
                .unwrap()
                .unwrap(),
            card("Ada"),
            "raw stays whole"
        );
        assert_eq!(ada_versions[0].content_type, "text/vcard");

        let again = f
            .ingest
            .ingest_item(item(book, "ada", card("Ada")))
            .await
            .unwrap();
        assert!(!again.new_version);

        let summary = f.store.get_item(ada.item_id).await.unwrap().unwrap();
        let index = summary.index.unwrap();
        assert_eq!(index.title.as_deref(), Some("Ada"));
        assert_eq!(index.emails, ["ada@example.com"]);
        assert_eq!(summary.item.key.as_deref(), Some("Ada"));

        let broken = f
            .ingest
            .ingest_item(item(book, "broken", Bytes::from_static(b"garbage")))
            .await
            .unwrap();
        let broken = f.store.get_item(broken.item_id).await.unwrap().unwrap();
        assert!(!broken.index.unwrap().readable, "unreadable, but archived");

        f.store
            .set_pim_index(
                ada.item_id,
                &pim_index(&holtmail_pim::Normalized::Unreadable, 1),
            )
            .await
            .unwrap();
        f.ingest.rebuild_item(ada.item_id).await.unwrap();
        let rebuilt = f.store.get_item(ada.item_id).await.unwrap().unwrap();
        assert_eq!(rebuilt.index.unwrap().title.as_deref(), Some("Ada"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn events_join_the_calendar_and_items_the_index() {
        let f = fixture().await;
        let calendar = |name: &str| holtmail_store::NewCollection {
            archive_id: f.archive,
            account_id: None,
            kind: holtmail_store::CollectionKind::Calendar,
            remote_id: name.into(),
            name: name.into(),
            color: None,
        };
        let work = f
            .store
            .upsert_collection(calendar("work.ics"))
            .await
            .unwrap()
            .id;
        let home = f
            .store
            .upsert_collection(calendar("home.ics"))
            .await
            .unwrap()
            .id;
        let event = |seq: u32| {
            Bytes::from(format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:review@example.com\r\nSEQUENCE:{seq}\r\nSUMMARY:Lamp review\r\nLOCATION:Harbour office\r\nDTSTART;TZID=Europe/Berlin:20250310T090000\r\nDTEND;TZID=Europe/Berlin:20250310T100000\r\nORGANIZER;CN=Ada Quill:mailto:ada@example.com\r\nATTENDEE;CN=Bo Lind;PARTSTAT=ACCEPTED:mailto:bo@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            ))
        };
        let mut incoming = item(work, "review", event(1));
        incoming.kind = holtmail_pim::ItemKind::Event;
        incoming.format = holtmail_pim::Format::ICalendar;
        let first = f.ingest.ingest_item(incoming.clone()).await.unwrap();
        incoming.collection = home;
        incoming.raw = event(2);
        let second = f.ingest.ingest_item(incoming).await.unwrap();
        let mut card_item = item(work, "ada", card("Ada"));
        card_item.collection = f
            .store
            .upsert_collection(holtmail_store::NewCollection {
                kind: holtmail_store::CollectionKind::Contacts,
                ..calendar("people.vcf")
            })
            .await
            .unwrap()
            .id;
        let ada = f.ingest.ingest_item(card_item).await.unwrap();
        f.ingest.commit().await.unwrap();

        let series = f.store.series(first.item_id).await.unwrap().unwrap();
        assert_eq!(series.window, None);
        let members = f.store.group_members(series.group_id).await.unwrap();
        assert_eq!(
            members.len(),
            2,
            "the same UID in two calendars is one group"
        );
        assert_eq!(f.store.series(ada.item_id).await.unwrap(), None);
        let found = f
            .store
            .occurrences(&holtmail_store::OccurrenceQuery {
                archive_id: f.archive,
                from: Timestamp::from_millis(1_741_593_600_000),
                to: Timestamp::from_millis(1_741_593_600_001),
                collections: None,
                removed: false,
            })
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            holtmail_calendar::fold(found)[0].leading().item_id,
            second.item_id,
            "the higher SEQUENCE leads"
        );

        let items = |clauses: Vec<Clause>| {
            let index = f.index.clone();
            let archive = f.archive;
            async move {
                let mut ids: Vec<holtmail_core::ItemId> = index
                    .search_items(archive, &SearchQuery { clauses }, SortOrder::Newest, 0, 10)
                    .await
                    .unwrap()
                    .hits
                    .into_iter()
                    .map(|h| h.item_id)
                    .collect();
                ids.sort();
                ids
            }
        };
        let mut both = vec![first.item_id, second.item_id];
        both.sort();
        assert_eq!(items(vec![Clause::Text("harbour".into())]).await, both);
        assert_eq!(items(vec![Clause::With("bo lind".into())]).await, both);
        assert_eq!(
            items(vec![Clause::Kind(holtmail_index::DocumentKind::Contact)]).await,
            [ada.item_id]
        );
        assert!(
            find(&f, vec![Clause::Text("harbour".into())])
                .await
                .is_empty()
        );
    }

    #[test]
    fn events_index_their_start_and_people() {
        let raw = b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:e1@example.com\nSUMMARY:Review\nDTSTART;TZID=Europe/Berlin:20250310T090000\nDTEND;TZID=Europe/Berlin:20250310T100000\nRRULE:FREQ=DAILY\nSEQUENCE:3\nORGANIZER:mailto:Ada@Example.com\nATTENDEE;PARTSTAT=ACCEPTED:mailto:bo@example.org\nEND:VEVENT\nEND:VCALENDAR\n";
        let normalized = holtmail_pim::normalize(holtmail_pim::ItemKind::Event, raw);
        let index = pim_index(&normalized, 2);
        assert_eq!(index.version, 2);
        assert_eq!(index.title.as_deref(), Some("Review"));
        assert_eq!(
            index.starts_at,
            Some(Timestamp::from_millis(1_741_593_600_000))
        );
        assert_eq!(
            index.ends_at,
            Some(Timestamp::from_millis(1_741_597_200_000))
        );
        assert_eq!(index.time_zone.as_deref(), Some("Europe/Berlin"));
        assert!(index.recurring && !index.all_day);
        assert_eq!(index.sequence, Some(3));
        assert_eq!(index.uid.as_deref(), Some("e1@example.com"));
        assert_eq!(index.organizer.as_deref(), Some("ada@example.com"));
        assert_eq!(index.emails, ["ada@example.com", "bo@example.org"]);
        assert_eq!(index.data["type"], "calendar");
    }

    struct CountingBlobs {
        inner: Arc<dyn BlobStore>,
        gets: std::sync::atomic::AtomicUsize,
    }

    impl CountingBlobs {
        fn gets(&self) -> usize {
            self.gets.swap(0, std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl BlobStore for CountingBlobs {
        async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError> {
            self.inner.put(bytes).await
        }

        async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError> {
            self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get(id).await
        }

        async fn contains(&self, id: &BlobId) -> Result<bool, BlobError> {
            self.inner.contains(id).await
        }

        async fn delete(&self, id: &BlobId) -> Result<(), BlobError> {
            self.inner.delete(id).await
        }
    }

    fn deriver() -> Arc<Deriver> {
        Arc::new(Deriver::new(
            vec![Arc::new(
                holtmail_category_signals::SignalsCategorizer::new(),
            )],
            holtmail_insights::DeriveConfig::default(),
        ))
    }

    fn categorized(f: &Fixture, blobs: Arc<dyn BlobStore>, deriver: &Arc<Deriver>) -> Ingest {
        Ingest::new(blobs, f.store.clone(), f.index.clone())
            .with_insights(runner("1"))
            .with_categories(deriver.clone(), deriver.policy())
    }

    async fn categories(f: &Fixture, message: MessageId) -> Option<holtmail_store::CategoryRow> {
        f.store
            .insight_view(message)
            .await
            .unwrap()
            .and_then(|view| view.categories)
            .map(|view| view.row)
    }

    const SIGNED_FORWARD: &[u8] = b"Authentication-Results: mx.example.org; dkim=pass header.d=sl.example.org\r\nX-SimpleLogin-Type: Forward\r\nX-SimpleLogin-Envelope-To: shop@alias.example.org\r\nFrom: \"Grace Hopper - grace at example.com\" <ra_q1@sl.example.org>\r\nTo: shop@alias.example.org\r\nSubject: Parcel\r\nMessage-ID: <p2@example.com>\r\nDate: Fri, 14 Mar 2025 11:00:00 +0000\r\n\r\nOn its way.\r\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn stage_b_follows_ingest_and_alias_changes_without_blob_reads() {
        let f = fixture().await;
        let counting = Arc::new(CountingBlobs {
            inner: f.blobs.clone(),
            gets: Default::default(),
        });
        let deriver = deriver();
        let ingest = categorized(&f, counting.clone(), &deriver);
        let created = ingest
            .ingest(
                f.archive,
                Bytes::from_static(SIGNED_FORWARD),
                &in_inbox(&f, 1, &[]),
            )
            .await
            .unwrap();
        assert_eq!(counting.gets(), 0, "ingest read a blob");
        let row = categories(&f, created.message_id)
            .await
            .expect("no categories row");
        let ctx = deriver.context(&*f.store, f.archive).await.unwrap();
        assert_eq!(row.write.derive_fp, ctx.derive_fp());
        assert_eq!(row.write.trust, TrustState::Unknown);
        assert_eq!(
            row.write.sender_address.as_deref(),
            Some("ra_q1@sl.example.org")
        );
        assert_eq!(row.kind, MessageKind::Personal);

        let detecting = ingest.with_alias_detectors(vec![Arc::new(
            holtmail_alias_simplelogin::SimpleLoginDetector::new(&["sl.example.org".into()]),
        )]);
        assert!(detecting.refresh_alias(created.message_id).await.unwrap());
        assert_eq!(counting.gets(), 1, "detection reads the blob once");
        assert_eq!(categories(&f, created.message_id).await, None);
        assert_eq!(
            f.store
                .count_needing_derive(f.archive, ctx.derive_fp())
                .await
                .unwrap(),
            1
        );
        assert!(!detecting.refresh_alias(created.message_id).await.unwrap());
        counting.gets();

        let ctx = deriver.context(&*f.store, f.archive).await.unwrap();
        let input = f
            .store
            .derive_input(created.message_id)
            .await
            .unwrap()
            .unwrap();
        let write = deriver.derive(&ctx, &input);
        f.store
            .set_message_categories(&[write], deriver.policy())
            .await
            .unwrap();
        assert_eq!(counting.gets(), 0, "stage B read a blob");
        let row = categories(&f, created.message_id).await.unwrap();
        assert_eq!(
            row.write.trust,
            TrustState::Relayed {
                provider: "simplelogin".into()
            }
        );
        assert_eq!(
            row.write.sender_address.as_deref(),
            Some("grace@example.com")
        );
        assert_eq!(row.write.sender_domain.as_deref(), Some("example.com"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_insights_write_runs_stage_b() {
        let f = fixture().await;
        let plain = f
            .ingest
            .clone()
            .with_insights(runner("1"))
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap()
            .message_id;
        assert_eq!(categories(&f, plain).await, None, "no deriver, no row");

        let deriver = deriver();
        let next = f
            .ingest
            .clone()
            .with_insights(runner("2"))
            .with_categories(deriver.clone(), deriver.policy());
        assert!(next.refresh_insights(plain).await.unwrap());
        assert!(categories(&f, plain).await.is_some());

        f.store.invalidate_derived(&[plain]).await.unwrap();
        assert!(!next.refresh_insights(plain).await.unwrap());
        assert_eq!(categories(&f, plain).await, None, "nothing was written");

        let mut row = insights(&f, plain).await.unwrap();
        row.fingerprint = "old".into();
        f.store.set_message_insights(plain, &row).await.unwrap();
        next.reindex(plain).await.unwrap();
        assert!(categories(&f, plain).await.is_some());

        f.store.invalidate_derived(&[plain]).await.unwrap();
        next.ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        assert!(
            categories(&f, plain).await.is_some(),
            "ingest of known bytes runs stage B"
        );

        let unreadable = categorized(&f, f.blobs.clone(), &deriver);
        unreadable.mark_insights_unreadable(plain).await.unwrap();
        f.store.invalidate_derived(&[plain]).await.unwrap();
        unreadable
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap();
        assert_eq!(
            categories(&f, plain).await,
            None,
            "an unreadable row gets no stage B"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn derive_categories_rewrites_one_row_without_blob_reads() {
        let f = fixture().await;
        let counting = Arc::new(CountingBlobs {
            inner: f.blobs.clone(),
            gets: Default::default(),
        });
        let deriver = deriver();
        let ingest = categorized(&f, counting.clone(), &deriver);
        let message = ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 1, &[]))
            .await
            .unwrap()
            .message_id;
        let plain = f.ingest.clone().with_insights(runner("1"));
        assert!(!plain.derive_categories(message).await.unwrap());

        f.store.invalidate_derived(&[message]).await.unwrap();
        let queued = f.store.reindex_queue(f.archive, None, 10).await.unwrap();
        f.store.dequeue_reindex(&queued).await.unwrap();
        assert!(ingest.derive_categories(message).await.unwrap());
        let ctx = deriver.context(&*f.store, f.archive).await.unwrap();
        let row = categories(&f, message).await.expect("no categories row");
        assert_eq!(row.write.derive_fp, ctx.derive_fp());
        let queued = f.store.reindex_queue(f.archive, None, 10).await.unwrap();
        assert!(queued.iter().any(|(id, _)| *id == message), "{queued:?}");

        let mut stale = row.write.clone();
        stale.derive_fp = "old".into();
        f.store
            .set_message_categories(&[stale], deriver.policy())
            .await
            .unwrap();
        assert!(!ingest.derive_categories(message).await.unwrap());
        let row = categories(&f, message).await.unwrap();
        assert_eq!(row.write.derive_fp, ctx.derive_fp());
        assert_eq!(counting.gets(), 0, "stage B read a blob");

        let unknown = MessageId::new();
        assert!(ingest.derive_categories(unknown).await.is_err());
    }

    const SIGN_IN_CODE: &[u8] = include_bytes!("../tests/fixtures/sign-in-code.eml");
    const NEWSLETTER: &[u8] = include_bytes!("../tests/fixtures/newsletter.eml");

    fn extracting() -> Arc<Runner> {
        Arc::new(Runner::new(vec![
            Arc::new(holtmail_insight_unsubscribe::UnsubscribeExtractor::new()),
            Arc::new(holtmail_insight_code::CodeExtractor::new()),
        ]))
    }

    fn insight(name: &str) -> Clause {
        Clause::Insight(name.into())
    }

    fn category(value: &str) -> Clause {
        Clause::Category(value.into())
    }

    async fn document_values(f: &Fixture, message: MessageId) -> (Vec<String>, Vec<String>) {
        f.ingest.indexed_insights(message).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_messages_are_found_by_insights_and_categories() {
        let f = fixture().await;
        let deriver = deriver();
        let ingest = f
            .ingest
            .clone()
            .with_insights(extracting())
            .with_categories(deriver.clone(), deriver.policy());
        let code = ingest
            .ingest(
                f.archive,
                Bytes::from_static(SIGN_IN_CODE),
                &in_inbox(&f, 1, &[]),
            )
            .await
            .unwrap()
            .message_id;
        let news = ingest
            .ingest(
                f.archive,
                Bytes::from_static(NEWSLETTER),
                &in_inbox(&f, 2, &[]),
            )
            .await
            .unwrap()
            .message_id;
        let plain = ingest
            .ingest(f.archive, Bytes::from_static(ROOT), &in_inbox(&f, 3, &[]))
            .await
            .unwrap()
            .message_id;
        ingest.commit().await.unwrap();

        assert_eq!(find(&f, vec![insight("code")]).await, [code]);
        assert_eq!(find(&f, vec![insight("unsubscribe")]).await, [news]);
        assert_eq!(find(&f, vec![category("kind:bulk")]).await, [news]);
        assert_eq!(
            find(&f, vec![Clause::Not(Box::new(insight("code")))]).await,
            [news, plain]
        );
        let (insights, categories) = document_values(&f, news).await;
        assert_eq!(insights, ["unsubscribe"]);
        assert_eq!(categories, ["kind:bulk"]);
        let (insights, categories) = document_values(&f, plain).await;
        assert!(insights.is_empty());
        assert_eq!(categories, ["kind:personal"]);
        assert!(
            !find(&f, vec![Clause::Text("482913".into())])
                .await
                .is_empty(),
            "the body is still indexed as text"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn insights_for_an_old_message_makes_it_findable() {
        let f = fixture().await;
        let old = f
            .ingest
            .ingest(
                f.archive,
                Bytes::from_static(SIGN_IN_CODE),
                &in_inbox(&f, 1, &[]),
            )
            .await
            .unwrap()
            .message_id;
        f.ingest.commit().await.unwrap();
        assert_eq!(document_values(&f, old).await, (Vec::new(), Vec::new()));
        assert!(find(&f, vec![insight("code")]).await.is_empty());

        let counting = Arc::new(CountingBlobs {
            inner: f.blobs.clone(),
            gets: Default::default(),
        });
        let deriver = deriver();
        let with = Ingest::new(counting.clone(), f.store.clone(), f.index.clone())
            .with_insights(extracting())
            .with_categories(deriver.clone(), deriver.policy());
        let parsed = holtmail_mime::parse(SIGN_IN_CODE).unwrap();
        let seq = with
            .insights_for(old, SIGN_IN_CODE, &parsed)
            .await
            .unwrap()
            .expect("insights written");
        assert_eq!(
            f.store.reindex_queue(f.archive, None, 10).await.unwrap(),
            [(old, seq)],
            "queued before the write, so a crash before the commit leaves it queued"
        );
        assert_eq!(
            with.insights_for(old, SIGN_IN_CODE, &parsed).await.unwrap(),
            None
        );
        with.commit().await.unwrap();
        assert_eq!(counting.gets(), 0, "insights_for read a blob");
        assert_eq!(find(&f, vec![insight("code")]).await, [old]);
        let (insights, categories) = document_values(&f, old).await;
        assert_eq!(insights, ["code"]);
        assert_eq!(
            find(&f, vec![category(&categories[0])]).await,
            [old],
            "the document carries its kind"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_insights_reindexes_what_it_wrote() {
        let f = fixture().await;
        let message = f
            .ingest
            .clone()
            .with_insights(runner("1"))
            .ingest(
                f.archive,
                Bytes::from_static(NEWSLETTER),
                &in_inbox(&f, 1, &[]),
            )
            .await
            .unwrap()
            .message_id;
        f.ingest.commit().await.unwrap();
        assert!(find(&f, vec![insight("unsubscribe")]).await.is_empty());

        let next = f.ingest.clone().with_insights(extracting());
        assert!(next.refresh_insights(message).await.unwrap());
        next.commit().await.unwrap();
        assert_eq!(find(&f, vec![insight("unsubscribe")]).await, [message]);
        assert!(
            find(&f, vec![category("kind:bulk")]).await.is_empty(),
            "no stage B without a deriver"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unreadable_rows_index_no_insights() {
        let f = fixture().await;
        let deriver = deriver();
        let ingest = f
            .ingest
            .clone()
            .with_insights(extracting())
            .with_categories(deriver.clone(), deriver.policy());
        let message = ingest
            .ingest(
                f.archive,
                Bytes::from_static(SIGN_IN_CODE),
                &in_inbox(&f, 1, &[]),
            )
            .await
            .unwrap()
            .message_id;
        let mut row = insights(&f, message).await.unwrap();
        row.outcome = InsightOutcome::Unreadable;
        f.store.set_message_insights(message, &row).await.unwrap();
        assert!(document_values(&f, message).await.0.is_empty());
    }
}
