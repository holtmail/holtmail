//! Sends held edits of contacts and events to their servers (CLIENTS.md, "Write-through").
//!
//! Edits wait in the store's `item_changes` ([`ItemChangeStore`]). [`Writeback`] takes the
//! changes of `write_through` collections first in first out per collection, one per item in
//! flight and only the head of a supersede chain (the store's claim guarantees all three), hands
//! each to the account's [`CollectionWriter`] and settles it: applied with the state upstream now
//! holds recorded as a `writeback` version, a conflict when upstream changed meanwhile, rejected
//! when upstream refused it, or held again after a temporary failure. A change is never dropped.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_blob::BlobStore;
use holtmail_collection::{
    CollectionError, CollectionSource, CollectionWriter, Field, ItemPatch, Partstat, Precondition,
    RemoteItem, WriteCaps, Written,
};
use holtmail_core::{AccountId, CollectionId, ItemId, Timestamp};
use holtmail_ingest::{IncomingItem, Ingest, pim_index};
use holtmail_pim::{Format, Normalized};
use holtmail_store::{
    ChangeClass, ChangeOp, ChangeTarget, Collection, CollectionKind, Item, ItemChange, ItemKind,
    NewItemVersion, Settle, Store, StoreError,
};
use tokio::sync::Notify;

/// Whether the account's sign-in lacks the consent to change contacts and calendars upstream:
/// an account signed in with OAuth whose last sign-in did not also grant the provider's write
/// scopes (`oauth.pim_write` in its settings): a Microsoft Graph account (`Contacts.ReadWrite`,
/// `Calendars.ReadWrite`) or a Google account reading contacts and calendars over DAV
/// (`auth/calendar`). Such an account gets no writer, and its changes wait, until someone
/// signs in again.
pub fn pim_write_consent_missing(account: &holtmail_store::Account) -> bool {
    let Some(oauth) = account.settings.get("oauth") else {
        return false;
    };
    let text = |key: &str| oauth.get(key).and_then(serde_json::Value::as_str);
    let reads = account.kind == holtmail_store::AccountKind::Graph
        || (text("provider") == Some("google") && text("access") == Some("imap_dav"));
    reads && oauth.get("pim_write").and_then(serde_json::Value::as_bool) != Some(true)
}

/// The writer of one account's collections with the source that reads them back.
#[derive(Clone)]
pub struct Upstream {
    /// Writes the changes.
    pub writer: Arc<dyn CollectionWriter>,
    /// Reads an item back after a write, and the upstream state after a failed precondition.
    pub source: Arc<dyn CollectionSource>,
}

/// Hands out the upstream of collections: the gate for writing.
#[async_trait]
pub trait Writers: Send + Sync {
    /// The upstream of `collection` when its changes may be sent now, or `None` when writing is
    /// not allowed at the moment (the account's write switch or scope is off): its changes then
    /// wait, as after a temporary failure, and the account pauses. An error counts like one from
    /// the writer.
    async fn upstream(&self, collection: &Collection) -> Result<Option<Upstream>, CollectionError>;

    /// Whether `reply` changes of `collection` may go upstream now (the account's
    /// `pim.respond`). While it is `false` they wait, held with the error `respond_off`, and the
    /// account's other changes go on. Defaults to `true`.
    async fn may_reply(&self, collection: &Collection) -> Result<bool, CollectionError> {
        let _ = collection;
        Ok(true)
    }

    /// Whether changes other than `reply` changes of `collection` may go upstream now (the
    /// account's `pim.edit`). While it is `false` they wait, held with the error `edit_off`, and
    /// the account's replies go on. Defaults to `true`.
    async fn may_edit(&self, collection: &Collection) -> Result<bool, CollectionError> {
        let _ = collection;
        Ok(true)
    }
}

/// How a `reply` change is answered upstream through [`CollectionWriter::respond`]: read from
/// the change by [`Rebaser::respond`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Respond {
    /// The occurrence answered, as iCalendar writes its `RECURRENCE-ID`; `None` for the series.
    pub recurrence_id: Option<String>,
    /// The answer.
    pub answer: Partstat,
    /// The server tells the organizer.
    pub notify: bool,
}

/// What `pim-edit` knows about a change, injected so this crate needs no reading of patches.
/// Pure: no I/O.
pub trait Rebaser: Send + Sync {
    /// The parts of the item the change touched, read from its stored patch; `None` (the
    /// default) means every part, so a field-patching writer replaces the whole item.
    fn fields(&self, change: &ItemChange) -> Option<Vec<Field>> {
        let _ = change;
        None
    }

    /// The whole item to send for `change`, whose own bytes are `mine`, after it was rebased
    /// (`ItemChange::rebased_onto`) on the upstream version `onto` (raw as stored, of media type
    /// `onto_type`): the change re-applied on that version, so upstream's own later edits are
    /// not overwritten. `None` (the default) sends `mine`.
    fn rebased(
        &self,
        change: &ItemChange,
        mine: &[u8],
        onto: &[u8],
        onto_type: &str,
    ) -> Option<Bytes> {
        let _ = (change, mine, onto, onto_type);
        None
    }

    /// The account's answer of the `reply` change `change`, whose whole item is `mine`, applied
    /// on top of `upstream`, the item as upstream holds it after it changed: the whole item to
    /// send instead. `None` when the answer no longer fits (the invitation was cancelled, the
    /// occurrence is gone, the account is no longer invited); the change is then a conflict
    /// `reply_obsolete`.
    fn rebase_reply(
        &self,
        change: &ItemChange,
        mine: &[u8],
        upstream: &RemoteItem,
    ) -> Option<Bytes>;

    /// For a `reply` change answered from an invitation (the RSVP API), how it goes through
    /// [`CollectionWriter::respond`]. `None` (the default) writes it with `put` like any change.
    fn respond(&self, change: &ItemChange) -> Option<Respond> {
        let _ = change;
        None
    }
}

/// A [`Rebaser`] that knows nothing: every part counts as changed and no reply is rebased.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRebase;

impl Rebaser for NoRebase {
    fn rebase_reply(&self, _: &ItemChange, _: &[u8], _: &RemoteItem) -> Option<Bytes> {
        None
    }
}

/// How [`Writeback`] times its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WritebackConfig {
    /// The wait before the first retry; it doubles with every attempt.
    pub first_retry: Duration,
    /// The longest wait between two attempts. A change is retried at this pace for as long as
    /// it takes; it never fails because of time.
    pub max_retry: Duration,
    /// How often [`Writeback::run`] looks for due changes when nothing wakes it.
    pub poll: Duration,
    /// The most changes of one collection one pass sends.
    pub batch: usize,
}

impl Default for WritebackConfig {
    fn default() -> Self {
        Self {
            first_retry: Duration::from_secs(30),
            max_retry: Duration::from_secs(24 * 3600),
            poll: Duration::from_secs(2),
            batch: 50,
        }
    }
}

/// Why a writeback operation failed. Failures of a server, the blob store or the index never
/// surface here: they are recorded on the change or logged.
#[derive(Debug, thiserror::Error)]
pub enum WritebackError {
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

enum Outcome {
    Applied(Option<Box<NewItemVersion>>),
    Rejected(&'static str),
    Conflict(&'static str),
    Retry(&'static str),
}

impl Outcome {
    fn blocks_account(&self) -> bool {
        matches!(
            self,
            Outcome::Retry("auth" | "connection" | "config" | "not_allowed")
        )
    }
}

/// Drains `item_changes` of `write_through` collections into the accounts' writers.
pub struct Writeback {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    ingest: Ingest,
    writers: Arc<dyn Writers>,
    rebaser: Arc<dyn Rebaser>,
    config: WritebackConfig,
    clock: Arc<dyn Fn() -> Timestamp + Send + Sync>,
    wake: Notify,
    paused: Mutex<HashMap<AccountId, (u32, Timestamp)>>,
}

impl Writeback {
    /// A writeback over these parts and timings, on the system clock. `ingest` records the
    /// upstream state a failed precondition reveals and rebuilds what is derived from items.
    pub fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        ingest: Ingest,
        writers: Arc<dyn Writers>,
        rebaser: Arc<dyn Rebaser>,
        config: WritebackConfig,
    ) -> Self {
        Self {
            store,
            blobs,
            ingest,
            writers,
            rebaser,
            config,
            clock: Arc::new(Timestamp::now),
            wake: Notify::new(),
            paused: Mutex::new(HashMap::new()),
        }
    }

    /// The same writeback on another clock, for tests.
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> Timestamp + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Its timings.
    pub fn config(&self) -> WritebackConfig {
        self.config
    }

    /// Wakes [`Writeback::run`] now, e.g. after a change was recorded.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    fn now(&self) -> Timestamp {
        (self.clock)()
    }

    /// Puts every change a stopped process left sending back to held (or superseded, when a
    /// later change took its place) and returns the held ones' ids, oldest first. Call once at
    /// start, before [`Writeback::run`]. Sending them again is safe: creates are idempotent by
    /// key, and an update that already landed fails its precondition and becomes a conflict.
    pub async fn recover(&self) -> Result<Vec<holtmail_core::ItemChangeId>, WritebackError> {
        let released = self.store.release_changes(self.now()).await?;
        for change in &released {
            tracing::warn!(change = %change.id, collection = %change.collection_id,
                "item change interrupted, sending again");
        }
        Ok(released.into_iter().map(|c| c.id).collect())
    }

    /// Sends the collection's due changes one at a time in the order they were made, at most
    /// [`WritebackConfig::batch`] of them, and returns how many it attempted. Which change is
    /// due is the store's claim: held, a class that is sent, not flagged by an upstream change,
    /// past its retry time, the head of its supersede chain, and no change of the same item in
    /// flight. Nothing is sent while the collection's account is paused.
    ///
    /// Each attempt settles the change:
    /// - **Applied** when upstream took it; what upstream now holds (the writer's answer, else
    ///   read back from the source, else the bytes sent) is recorded as a `writeback` version.
    ///   A delete marks the item removed. A delete of an item that never reached upstream is
    ///   applied without asking.
    /// - **Conflict** when upstream changed meanwhile (a failed precondition): the item as
    ///   upstream now holds it is recorded as an upstream version, or the item marked removed
    ///   when it is gone, then the change becomes a conflict (`upstream_changed`,
    ///   `upstream_deleted`; `exists` for a create whose name is taken). A `reply` is first
    ///   rebased with [`Rebaser::rebase_reply`] and sent once more; a delete of an item already
    ///   gone upstream is applied.
    /// - **Rejected** when upstream refused it (`refused`, `unsupported` when the writer cannot
    ///   do it, `too_large`, `collection_gone`, `missing_bytes`, `invalid`). It stays served.
    /// - **Held again** after a temporary failure (`connection`, `protocol`, `auth`, `config`,
    ///   `not_allowed`, `store`, `blob`) until a backoff of [`WritebackConfig::first_retry`]
    ///   doubling per attempt, capped at [`WritebackConfig::max_retry`].
    ///
    /// A rejected sign-in, a connection failure, broken settings or a closed gate pause the
    /// whole account: the pass stops and the account's collections wait out the same backoff
    /// (doubling per failed pass, kept in memory), so a bad password costs one sign-in per step.
    /// Any other outcome ends the pause.
    pub async fn drain_collection(
        &self,
        collection: CollectionId,
    ) -> Result<usize, WritebackError> {
        let Some(found) = self.store.get_collection(collection).await? else {
            return Ok(0);
        };
        self.drain(&found).await
    }

    async fn drain(&self, collection: &Collection) -> Result<usize, WritebackError> {
        let account = collection.account_id;
        if account
            .and_then(|a| self.paused_until(a))
            .is_some_and(|until| until > self.now())
        {
            return Ok(0);
        }
        let mut attempted = 0;
        while attempted < self.config.batch {
            let Some(change) = self.store.claim_change(collection.id, self.now()).await? else {
                break;
            };
            attempted += 1;
            let outcome = self.attempt(collection, &change).await;
            let blocks = outcome.blocks_account();
            self.settle(&change, outcome).await?;
            if let Some(account) = account {
                if blocks {
                    self.pause(account);
                    break;
                }
                self.resume(account);
            }
        }
        if attempted > 0
            && let Err(error) = self.ingest.commit().await
        {
            tracing::warn!(%error, "committing the search index after writeback failed");
        }
        Ok(attempted)
    }

    /// [`Writeback::drain_collection`] for every collection with a due change whose account is
    /// not paused: accounts in parallel, the collections of one account one after another.
    /// Returns how many changes were attempted.
    pub async fn drain_due(&self) -> Result<usize, WritebackError> {
        let now = self.now();
        let mut groups: BTreeMap<Option<AccountId>, Vec<Collection>> = BTreeMap::new();
        for id in self.store.due_change_collections(now).await? {
            if let Some(collection) = self.store.get_collection(id).await? {
                if collection
                    .account_id
                    .and_then(|a| self.paused_until(a))
                    .is_some_and(|until| until > now)
                {
                    continue;
                }
                groups
                    .entry(collection.account_id)
                    .or_default()
                    .push(collection);
            }
        }
        let results =
            futures::future::join_all(groups.into_values().map(|collections| async move {
                let mut attempted = 0;
                for collection in &collections {
                    attempted += self.drain(collection).await?;
                }
                Ok::<_, WritebackError>(attempted)
            }))
            .await;
        let mut attempted = 0;
        for result in results {
            match result {
                Ok(n) => attempted += n,
                Err(error) => tracing::warn!(%error, "item writeback pass failed"),
            }
        }
        Ok(attempted)
    }

    /// Sends what is due, then waits for [`Writeback::wake`] or the next poll; runs until the
    /// process ends.
    pub async fn run(self: Arc<Self>) {
        loop {
            match self.drain_due().await {
                Ok(0) => {}
                Ok(_) => continue,
                Err(error) => tracing::warn!(%error, "item writeback pass failed"),
            }
            let _ = tokio::time::timeout(self.config.poll, self.wake.notified()).await;
        }
    }

    fn paused_until(&self, account: AccountId) -> Option<Timestamp> {
        let paused = self.paused.lock().unwrap_or_else(|e| e.into_inner());
        paused.get(&account).map(|(_, until)| *until)
    }

    fn pause(&self, account: AccountId) {
        let now = self.now();
        let mut paused = self.paused.lock().unwrap_or_else(|e| e.into_inner());
        let failures = paused
            .get(&account)
            .map_or(0, |(n, _)| *n)
            .saturating_add(1);
        let until = later(now, self.backoff(failures));
        paused.insert(account, (failures, until));
        tracing::warn!(%account, failures, "item writeback paused for the account");
    }

    fn resume(&self, account: AccountId) {
        let mut paused = self.paused.lock().unwrap_or_else(|e| e.into_inner());
        paused.remove(&account);
    }

    fn backoff(&self, attempts: u32) -> Duration {
        self.config
            .first_retry
            .saturating_mul(2u32.saturating_pow(attempts.saturating_sub(1).min(31)))
            .min(self.config.max_retry)
    }

    async fn settle(&self, change: &ItemChange, outcome: Outcome) -> Result<(), WritebackError> {
        let now = self.now();
        let (settle, code) = match outcome {
            Outcome::Applied(version) => (Settle::Applied { version }, "applied"),
            Outcome::Rejected(code) => (
                Settle::Rejected {
                    reason: code.into(),
                },
                code,
            ),
            Outcome::Conflict(code) => (
                Settle::Conflict {
                    reason: code.into(),
                },
                code,
            ),
            Outcome::Retry(code) => (
                Settle::Retry {
                    next_attempt_at: later(now, self.backoff(change.attempts)),
                    error: code.into(),
                },
                code,
            ),
        };
        let settled = match self.store.settle_change(change.id, &settle, now).await {
            Ok(settled) => settled,
            Err(error) => {
                tracing::warn!(change = %change.id, %error, "settling an item change failed");
                let fallback = match settle {
                    Settle::Applied { .. } => Settle::Applied { version: None },
                    _ => Settle::Retry {
                        next_attempt_at: later(now, self.backoff(change.attempts)),
                        error: "store".into(),
                    },
                };
                self.store.settle_change(change.id, &fallback, now).await?
            }
        };
        tracing::info!(change = %change.id, collection = %change.collection_id, code,
            attempts = change.attempts, "item change settled");
        if let (Settle::Applied { version: Some(_) }, Some(item)) =
            (&settle, settled.and_then(|c| c.item_id))
            && let Err(error) = self.derive(item).await
        {
            tracing::warn!(%item, %error, "rebuilding an item after writeback failed");
        }
        Ok(())
    }

    async fn derive(&self, item: ItemId) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let effective = self.store.effective(&ChangeTarget::Item { item }).await?;
        let head = effective.as_ref().and_then(|e| {
            let change = e.change.as_ref().filter(|c| c.op != ChangeOp::Delete)?;
            Some((change, change.content.as_ref()?, e.latest.as_ref()?))
        });
        let Some((change, content, latest)) = head else {
            self.ingest.derive_item(item).await?;
            return Ok(());
        };
        let Some(format) = Format::from_content_type(&content.content_type) else {
            self.ingest.derive_item(item).await?;
            return Ok(());
        };
        let (Some(mine), Some(onto)) = (
            self.blobs.get(&content.blob_id).await?,
            self.blobs.get(&latest.blob_id).await?,
        ) else {
            self.ingest.derive_item(item).await?;
            return Ok(());
        };
        let served = self
            .rebaser
            .rebased(change, &mine, &onto, &latest.content_type)
            .unwrap_or(mine);
        self.ingest
            .derive_served(item, Some((&served, format)))
            .await?;
        Ok(())
    }

    async fn attempt(&self, collection: &Collection, change: &ItemChange) -> Outcome {
        let upstream = match self.writers.upstream(collection).await {
            Ok(Some(upstream)) => upstream,
            Ok(None) => return Outcome::Retry("not_allowed"),
            Err(error) => return classify(&error),
        };
        if change.class == ChangeClass::Reply {
            match self.writers.may_reply(collection).await {
                Ok(true) => {}
                Ok(false) => return Outcome::Retry("respond_off"),
                Err(error) => return classify(&error),
            }
        } else {
            match self.writers.may_edit(collection).await {
                Ok(true) => {}
                Ok(false) => return Outcome::Retry("edit_off"),
                Err(error) => return classify(&error),
            }
        }
        let item = match change.item_id {
            Some(id) => match self.store.get_item(id).await {
                Ok(Some(summary)) => Some(summary.item),
                Ok(None) => return Outcome::Rejected("invalid"),
                Err(_) => return Outcome::Retry("store"),
            },
            None => None,
        };
        let caps = upstream.writer.capabilities(&collection.remote_id);
        let send = Job {
            collection,
            change,
            upstream: &upstream,
            caps,
        };
        match (item, change.op) {
            (None, ChangeOp::Delete) => Outcome::Applied(None),
            (Some(item), ChangeOp::Delete) => self.delete(&send, &item).await,
            (None, _) => self.create(&send).await,
            (Some(item), _) => self.update(&send, &item).await,
        }
    }

    async fn delete(&self, send: &Job<'_>, item: &Item) -> Outcome {
        if !send.caps.delete {
            return Outcome::Rejected("unsupported");
        }
        let base = match self.base(send.change, item).await {
            Ok(base) => base,
            Err(outcome) => return outcome,
        };
        let etag = base.as_ref().and_then(|b| b.etag.as_deref());
        match send
            .upstream
            .writer
            .delete(&send.collection.remote_id, &item.remote_id, etag)
            .await
        {
            Ok(()) => Outcome::Applied(None),
            Err(CollectionError::PreconditionFailed { .. }) => {
                match self.reveal(send, &item.remote_id).await {
                    Ok(Some(_)) => Outcome::Conflict("upstream_changed"),
                    Ok(None) => Outcome::Applied(None),
                    Err(outcome) => outcome,
                }
            }
            Err(error) => classify(&error),
        }
    }

    async fn create(&self, send: &Job<'_>) -> Outcome {
        if !send.caps.create {
            return Outcome::Rejected("unsupported");
        }
        let patch = match self.patch(send, None).await {
            Ok(patch) => patch,
            Err(outcome) => return outcome,
        };
        let key = send
            .change
            .href
            .clone()
            .unwrap_or_else(|| send.change.id.to_string());
        match send
            .upstream
            .writer
            .put(
                &send.collection.remote_id,
                None,
                &patch,
                Precondition::Absent,
                &key,
            )
            .await
        {
            Ok(written) => self.applied(send, written, &patch).await,
            Err(CollectionError::PreconditionFailed { .. }) => Outcome::Conflict("exists"),
            Err(error) => classify(&error),
        }
    }

    async fn update(&self, send: &Job<'_>, item: &Item) -> Outcome {
        if send.change.class == ChangeClass::Reply
            && send.caps.respond
            && let Some(respond) = self.rebaser.respond(send.change)
        {
            return self.respond(send, item, &respond).await;
        }
        if !send.caps.update {
            return Outcome::Rejected("unsupported");
        }
        let patch = match self.patch(send, Some(item)).await {
            Ok(patch) => patch,
            Err(outcome) => return outcome,
        };
        let base = match self.base(send.change, item).await {
            Ok(base) => base,
            Err(outcome) => return outcome,
        };
        let precondition = match base {
            Some(version) => match version.etag {
                Some(etag) => Precondition::IfMatch(etag),
                None => match self.blobs.get(&version.blob_id).await {
                    Ok(Some(raw)) => Precondition::IfUnchanged(raw),
                    Ok(None) => Precondition::None,
                    Err(_) => return Outcome::Retry("blob"),
                },
            },
            None => Precondition::None,
        };
        let key = send.change.id.to_string();
        let remote = &send.collection.remote_id;
        let writer = &send.upstream.writer;
        match writer
            .put(remote, Some(&item.remote_id), &patch, precondition, &key)
            .await
        {
            Ok(written) => return self.applied(send, written, &patch).await,
            Err(CollectionError::PreconditionFailed { .. }) => {}
            Err(error) => return classify(&error),
        }
        let current = match self.reveal(send, &item.remote_id).await {
            Ok(Some(current)) => current,
            Ok(None) => return Outcome::Conflict("upstream_deleted"),
            Err(outcome) => return outcome,
        };
        if send.change.class != ChangeClass::Reply {
            return Outcome::Conflict("upstream_changed");
        }
        let Some(raw) = self.rebaser.rebase_reply(send.change, &patch.raw, &current) else {
            return Outcome::Conflict("reply_obsolete");
        };
        let rebased = ItemPatch { raw, ..patch };
        let precondition = match current.etag.clone() {
            Some(etag) => Precondition::IfMatch(etag),
            None => Precondition::IfUnchanged(current.raw.clone()),
        };
        match writer
            .put(remote, Some(&item.remote_id), &rebased, precondition, &key)
            .await
        {
            Ok(written) => self.applied(send, written, &rebased).await,
            Err(CollectionError::PreconditionFailed { .. }) => {
                match self.reveal(send, &item.remote_id).await {
                    Ok(Some(_)) => Outcome::Conflict("upstream_changed"),
                    Ok(None) => Outcome::Conflict("upstream_deleted"),
                    Err(outcome) => outcome,
                }
            }
            Err(error) => classify(&error),
        }
    }

    async fn respond(&self, send: &Job<'_>, item: &Item, respond: &Respond) -> Outcome {
        let patch = match self.patch(send, Some(item)).await {
            Ok(patch) => patch,
            Err(outcome) => return outcome,
        };
        match send
            .upstream
            .writer
            .respond(
                &send.collection.remote_id,
                &item.remote_id,
                respond.recurrence_id.as_deref(),
                respond.answer,
                respond.notify,
            )
            .await
        {
            Ok(written) => {
                let unvouched = Job {
                    caps: WriteCaps {
                        field_patch: true,
                        ..send.caps
                    },
                    ..*send
                };
                self.applied(&unvouched, written, &patch).await
            }
            Err(CollectionError::PreconditionFailed { .. }) => {
                match self.reveal(send, &item.remote_id).await {
                    Ok(Some(_)) => Outcome::Conflict("upstream_changed"),
                    Ok(None) => Outcome::Conflict("upstream_deleted"),
                    Err(outcome) => outcome,
                }
            }
            Err(error) => classify(&error),
        }
    }

    async fn base(
        &self,
        change: &ItemChange,
        item: &Item,
    ) -> Result<Option<holtmail_store::ItemVersion>, Outcome> {
        let versions = self
            .store
            .item_versions(item.id)
            .await
            .map_err(|_| Outcome::Retry("store"))?;
        let wanted = change.rebased_onto.or(change.base_version);
        let found = wanted
            .and_then(|n| versions.iter().find(|v| v.number == n))
            .or(versions.last());
        Ok(found.cloned())
    }

    async fn patch(&self, send: &Job<'_>, item: Option<&Item>) -> Result<ItemPatch, Outcome> {
        let Some(content) = &send.change.content else {
            return Err(Outcome::Rejected("invalid"));
        };
        let raw = match self.blobs.get(&content.blob_id).await {
            Ok(Some(raw)) => raw,
            Ok(None) => return Err(Outcome::Rejected("missing_bytes")),
            Err(_) => return Err(Outcome::Retry("blob")),
        };
        let raw = match (item, send.change.rebased_onto) {
            (Some(item), Some(onto)) if Some(onto) != send.change.base_version => {
                self.rebased_bytes(send.change, item, onto, raw).await?
            }
            _ => raw,
        };
        if send.caps.max_size.is_some_and(|max| raw.len() as u64 > max) {
            return Err(Outcome::Rejected("too_large"));
        }
        let uid = match item.and_then(|i| i.key.clone()) {
            Some(uid) => Some(uid),
            None => {
                let format = format_of(send.collection.kind, &content.content_type);
                key_of(&holtmail_pim::read_as(format, format.kind(&raw), &raw).normalized)
            }
        };
        Ok(ItemPatch {
            content_type: content.content_type.clone(),
            raw,
            uid,
            fields: self.rebaser.fields(send.change),
        })
    }

    async fn rebased_bytes(
        &self,
        change: &ItemChange,
        item: &Item,
        onto: u32,
        mine: Bytes,
    ) -> Result<Bytes, Outcome> {
        let versions = self
            .store
            .item_versions(item.id)
            .await
            .map_err(|_| Outcome::Retry("store"))?;
        let Some(version) = versions.iter().find(|v| v.number == onto) else {
            return Ok(mine);
        };
        let onto_raw = match self.blobs.get(&version.blob_id).await {
            Ok(Some(raw)) => raw,
            Ok(None) => return Ok(mine),
            Err(_) => return Err(Outcome::Retry("blob")),
        };
        Ok(self
            .rebaser
            .rebased(change, &mine, &onto_raw, &version.content_type)
            .unwrap_or(mine))
    }

    async fn applied(&self, send: &Job<'_>, written: Written, sent: &ItemPatch) -> Outcome {
        let remote = &send.collection.remote_id;
        let item = match written.item {
            Some(item) => Some(item),
            None => send
                .upstream
                .source
                .fetch(remote, std::slice::from_ref(&written.id))
                .await
                .ok()
                .and_then(|items| items.into_iter().find(|i| i.id == written.id)),
        };
        let item = item.unwrap_or_else(|| RemoteItem {
            id: written.id.clone(),
            etag: if send.caps.field_patch {
                None
            } else {
                written.etag.clone()
            },
            content_type: sent.content_type.clone(),
            raw: sent.raw.clone(),
        });
        match self.version(send.collection, item).await {
            Some(version) => Outcome::Applied(Some(Box::new(version))),
            None => Outcome::Applied(None),
        }
    }

    async fn version(&self, collection: &Collection, item: RemoteItem) -> Option<NewItemVersion> {
        let format = format_of(collection.kind, &item.content_type);
        let kind = format.kind(&item.raw);
        let read = holtmail_pim::read_as(format, kind, &item.raw);
        let photo_blob_id = match read.photo {
            Some(photo) => match self.blobs.put(Bytes::from(photo.bytes)).await {
                Ok(id) => Some(id),
                Err(error) => {
                    tracing::warn!(collection = %collection.id, %error,
                        "storing the photo of a writeback version failed");
                    return None;
                }
            },
            None => None,
        };
        let blob_id = match self.blobs.put(item.raw).await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(collection = %collection.id, %error,
                    "storing a writeback version failed");
                return None;
            }
        };
        Some(NewItemVersion {
            collection_id: collection.id,
            remote_id: item.id,
            kind: ItemKind::parse(kind.as_str()).unwrap_or(ItemKind::Other),
            key: key_of(&read.normalized),
            blob_id,
            content_type: format.content_type().to_owned(),
            etag: item.etag,
            seen_at: self.now(),
            photo_blob_id,
            index: pim_index(&read.normalized, 0),
        })
    }

    async fn reveal(&self, send: &Job<'_>, id: &str) -> Result<Option<RemoteItem>, Outcome> {
        let collection = send.collection;
        let found = send
            .upstream
            .source
            .fetch(&collection.remote_id, &[id.to_owned()])
            .await
            .map_err(|e| classify(&e))?
            .into_iter()
            .find(|i| i.id == id);
        let now = self.now();
        match &found {
            Some(current) => {
                let format = format_of(collection.kind, &current.content_type);
                let incoming = IncomingItem {
                    collection: collection.id,
                    remote_id: current.id.clone(),
                    kind: format.kind(&current.raw),
                    format,
                    raw: current.raw.clone(),
                    etag: current.etag.clone(),
                    seen_at: now,
                };
                if let Err(error) = self.ingest.ingest_item(incoming).await {
                    tracing::warn!(collection = %collection.id, %error,
                        "recording the upstream version after a failed precondition failed");
                }
            }
            None => {
                if let Err(error) = self
                    .store
                    .mark_items_removed(collection.id, &[id.to_owned()], now)
                    .await
                {
                    tracing::warn!(collection = %collection.id, %error,
                        "marking an item removed after a failed precondition failed");
                }
            }
        }
        Ok(found)
    }
}

#[derive(Clone, Copy)]
struct Job<'a> {
    collection: &'a Collection,
    change: &'a ItemChange,
    upstream: &'a Upstream,
    caps: WriteCaps,
}

fn format_of(kind: CollectionKind, content_type: &str) -> Format {
    Format::from_content_type(content_type).unwrap_or(match kind {
        CollectionKind::Contacts => Format::VCard,
        CollectionKind::Calendar => Format::ICalendar,
    })
}

fn key_of(normalized: &Normalized) -> Option<String> {
    match normalized {
        Normalized::Contact(contact) => contact.uid.clone(),
        Normalized::Calendar(calendar) => calendar.main().and_then(|e| e.uid.clone()),
        Normalized::Unreadable => None,
    }
}

fn classify(error: &CollectionError) -> Outcome {
    match error {
        CollectionError::Auth(_) => Outcome::Retry("auth"),
        CollectionError::Connection(_) => Outcome::Retry("connection"),
        CollectionError::Protocol(_) | CollectionError::TokenExpired => Outcome::Retry("protocol"),
        CollectionError::Config(_) => Outcome::Retry("config"),
        CollectionError::Unsupported => Outcome::Rejected("unsupported"),
        CollectionError::Gone => Outcome::Rejected("collection_gone"),
        CollectionError::PreconditionFailed { .. } => Outcome::Conflict("upstream_changed"),
        CollectionError::Refused(_) => Outcome::Rejected("refused"),
    }
}

fn later(now: Timestamp, wait: Duration) -> Timestamp {
    let wait = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
    Timestamp::from_millis(now.as_millis().saturating_add(wait))
}

#[cfg(test)]
mod tests;
