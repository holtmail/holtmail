//! Sends held changes to messages on their servers: flags, labels, moves, deletes and appends.
//!
//! Changes wait in the store's `mail_changes` ([`MailChangeStore`]). [`Writeback`] takes them
//! first in first out per account, at most one per message in flight, hands each to the
//! account's [`MailboxWriter`] and records the outcome: accepted, tried again later after a
//! temporary failure, or given up after a refusal. Flags and labels the server accepted are
//! confirmed by what sync reads afterwards, and the server wins when it shows something else.

mod policy;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use holtmail_blob::BlobStore;
use holtmail_core::{AccountId, FolderId, MailChangeId, Timestamp};
use holtmail_source::{Flag, MailboxWriter, SourceError};
use holtmail_store::{MailChange, MailChangeState, MailOp, Store, StoreError};
use tokio::sync::Notify;

pub use holtmail_access::{MAIL_WRITE_CAPABILITIES, write_consent_missing, writes_mail};
pub use policy::{
    MAX_RETENTION_DAYS, RETENTION_SETTING, RetentionPlan, allows, capability_for, is_folder_label,
    plan_retention, retention_days, trash_folder, with_retention_days,
};

/// Hands out the writers of accounts.
#[async_trait]
pub trait Writers: Send + Sync {
    /// The writer of `account` when `op` may be sent to its server now, or `None` when writing
    /// it is not allowed (the account's capability for this kind of change is off): the change
    /// then fails with `not_allowed`. An error is treated like one from the writer: a transient
    /// one, or [`SourceError::Auth`], tries again later; any other fails the change.
    async fn writer(
        &self,
        account: AccountId,
        op: &MailOp,
    ) -> Result<Option<Arc<dyn MailboxWriter>>, SourceError>;
}

/// How [`Writeback`] times its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WritebackConfig {
    /// The wait before the first retry; it doubles with every attempt.
    pub first_retry: Duration,
    /// The longest wait between two attempts.
    pub max_retry: Duration,
    /// How long after it was asked for a change is retried before it fails.
    pub retry_for: Duration,
    /// How long a flags or labels change the server accepted waits for a sync to confirm it
    /// before it counts as applied anyway.
    pub confirm_within: Duration,
    /// How often [`Writeback::run`] looks for due changes when nothing wakes it.
    pub poll: Duration,
    /// How often [`Writeback::run`] settles unconfirmed changes.
    pub settle_every: Duration,
    /// The most changes of one account one pass sends.
    pub batch: usize,
}

impl Default for WritebackConfig {
    fn default() -> Self {
        Self {
            first_retry: Duration::from_secs(30),
            max_retry: Duration::from_secs(3600),
            retry_for: Duration::from_secs(24 * 3600),
            confirm_within: Duration::from_secs(15 * 60),
            poll: Duration::from_secs(2),
            settle_every: Duration::from_secs(60),
            batch: 50,
        }
    }
}

/// Why a writeback operation failed. Failures of a server or the blob store never surface
/// here: they are recorded on the change.
#[derive(Debug, thiserror::Error)]
pub enum WritebackError {
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What sync read for one message at one location: the input to [`Writeback::confirm`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagObservation {
    /// The folder the message is in.
    pub folder: FolderId,
    /// Its UID there.
    pub uid: u64,
    /// Its flags, as IMAP spells them.
    pub flags: Vec<String>,
    /// Its labels, when the server has labels and sync read them.
    pub labels: Option<Vec<String>>,
    /// When the server reported them, taken before the read started.
    pub read_at: Timestamp,
}

/// What [`Writeback::confirm`] closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmed {
    /// Changes the observed state satisfies, now applied.
    pub applied: Vec<MailChangeId>,
    /// Sent changes the observed state contradicts, now superseded by the server.
    pub superseded: Vec<MailChangeId>,
}

enum Attempt {
    Done(Option<u64>),
    Retry(&'static str),
    Fail(&'static str),
}

impl Attempt {
    fn blocks_account(&self) -> bool {
        matches!(self, Attempt::Retry("auth" | "connection"))
    }
}

enum Place {
    Here,
    Arrived(u64),
    Lost,
    At(FolderId, u64),
}

const MAX_FOLLOWED_MOVES: usize = 16;

/// Drains `mail_changes` into the accounts' writers.
pub struct Writeback {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    writers: Arc<dyn Writers>,
    config: WritebackConfig,
    clock: Arc<dyn Fn() -> Timestamp + Send + Sync>,
    wake: Notify,
    paused: Mutex<HashMap<AccountId, (u32, Timestamp)>>,
}

impl Writeback {
    /// A writeback over these parts and timings, on the system clock.
    pub fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        writers: Arc<dyn Writers>,
        config: WritebackConfig,
    ) -> Self {
        Self {
            store,
            blobs,
            writers,
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

    /// Wakes [`Writeback::run`] now, e.g. after a change was enqueued.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    fn now(&self) -> Timestamp {
        (self.clock)()
    }

    /// Puts every change left sending and not yet accepted by a stopped process back to held,
    /// and returns their ids, oldest first. Call once at start, before [`Writeback::run`]. An
    /// append among them may be saved twice on its server.
    pub async fn recover(&self) -> Result<Vec<MailChangeId>, WritebackError> {
        let released = self.store.release_mail_changes(self.now()).await?;
        for change in &released {
            tracing::warn!(change = %change.id, kind = change.new.op.kind(),
                "mail change interrupted, sending again");
        }
        Ok(released.into_iter().map(|c| c.id).collect())
    }

    /// Sends the account's due changes one at a time, oldest first, at most
    /// [`WritebackConfig::batch`] of them, and returns how many it attempted. A message's
    /// changes go out in the order they were made and never two at once; a change waiting for
    /// a retry, or a sent flags change waiting for confirmation, holds back the later changes
    /// of its message but not those of other messages.
    ///
    /// Each attempt ends in one of three ways. Accepted: the change is applied, or for flags
    /// and labels waits for [`Writeback::confirm`] or [`Writeback::settle`]. A temporary
    /// failure (a connection or protocol error, a rejected sign-in, the blob store): held
    /// again until a backoff of [`WritebackConfig::first_retry`] doubling per attempt, capped
    /// at [`WritebackConfig::max_retry`], or failed once that would pass
    /// [`WritebackConfig::retry_for`] after the change was asked for. A refusal: failed at
    /// once. The change's `last_error` is a short code: `connection`, `protocol`, `auth`,
    /// `store`, `blob`, `config`, `unsupported`, `missing_folder` (not on the server),
    /// `unknown_folder` (no longer in the store), `missing_bytes`, `gone` (the message is no
    /// longer at its UID), `moved` (an earlier move took the message to a UID the server did
    /// not report), `not_allowed` or `invalid`.
    ///
    /// A change made at a place an earlier applied move of its message left is sent to where
    /// the message went (the move's target and reported UID); a move to the folder the message
    /// is already in is applied without asking the server.
    ///
    /// A rejected sign-in or a connection failure pauses the whole account, not only the
    /// change: the pass stops there and the account's other changes wait out the same backoff
    /// (doubling per failed pass, kept in memory), so a bad password costs one sign-in per
    /// backoff step, never one per held change. An accepted or refused change ends the pause.
    pub async fn drain_account(&self, account: AccountId) -> Result<usize, WritebackError> {
        let mut attempted = 0;
        if self
            .paused_until(account)
            .is_some_and(|until| until > self.now())
        {
            return Ok(0);
        }
        while attempted < self.config.batch {
            let Some(change) = self.store.claim_mail_change(account, self.now()).await? else {
                break;
            };
            attempted += 1;
            let attempt = self.attempt(&change).await;
            let blocks = attempt.blocks_account();
            self.record(&change, attempt).await?;
            if blocks {
                self.pause(account);
                break;
            }
            self.resume(account);
        }
        Ok(attempted)
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
        let until = Timestamp::from_millis(
            now.as_millis()
                .saturating_add(millis(self.backoff(failures))),
        );
        paused.insert(account, (failures, until));
        tracing::warn!(%account, failures, "mail writeback paused for the account");
    }

    fn resume(&self, account: AccountId) {
        let mut paused = self.paused.lock().unwrap_or_else(|e| e.into_inner());
        paused.remove(&account);
    }

    /// [`Writeback::drain_account`] for every account with a due change that is not paused,
    /// accounts in parallel. Returns how many changes were attempted.
    pub async fn drain_due(&self) -> Result<usize, WritebackError> {
        let now = self.now();
        let mut due = self.store.due_mail_accounts(now).await?;
        due.retain(|a| self.paused_until(*a).is_none_or(|until| until <= now));
        let results =
            futures::future::join_all(due.into_iter().map(|a| self.drain_account(a))).await;
        let mut attempted = 0;
        for result in results {
            match result {
                Ok(n) => attempted += n,
                Err(error) => tracing::warn!(%error, "mail writeback pass failed"),
            }
        }
        Ok(attempted)
    }

    /// Tells the open flags and labels changes what sync read. A change the observed state
    /// already satisfies is applied, even one still held; a change the server accepted before
    /// `read_at` that the state contradicts is superseded by the server, which wins. A change
    /// being sent right now, and one accepted after `read_at`, are left alone. Idempotent.
    pub async fn confirm(
        &self,
        observations: &[FlagObservation],
    ) -> Result<Confirmed, WritebackError> {
        let mut confirmed = Confirmed::default();
        for seen in observations {
            let closed = self
                .store
                .confirm_mail_flags(
                    seen.folder,
                    seen.uid,
                    &seen.flags,
                    seen.labels.as_deref(),
                    seen.read_at,
                )
                .await?;
            for change in closed {
                match change.state {
                    MailChangeState::SupersededByServer => confirmed.superseded.push(change.id),
                    _ => confirmed.applied.push(change.id),
                }
            }
        }
        if !confirmed.applied.is_empty() || !confirmed.superseded.is_empty() {
            tracing::info!(
                applied = confirmed.applied.len(),
                superseded = confirmed.superseded.len(),
                "mail changes confirmed by sync"
            );
        }
        Ok(confirmed)
    }

    /// Applies the account's flags and labels changes the server accepted more than
    /// [`WritebackConfig::confirm_within`] ago that no sync confirmed, trusting the server's
    /// answer. Returns how many.
    pub async fn settle(&self, account: AccountId) -> Result<u64, WritebackError> {
        let now = self.now();
        let before = Timestamp::from_millis(now.as_millis() - millis(self.config.confirm_within));
        let settled = self.store.settle_mail_changes(account, before, now).await?;
        if settled > 0 {
            tracing::info!(%account, settled, "unconfirmed mail changes settled");
        }
        Ok(settled)
    }

    /// [`Writeback::settle`] for every account of every archive. Returns how many.
    pub async fn settle_all(&self) -> Result<u64, WritebackError> {
        let mut settled = 0;
        for archive in self.store.list_archives().await? {
            for account in self.store.list_accounts(archive.id).await? {
                settled += self.settle(account.id).await?;
            }
        }
        Ok(settled)
    }

    /// Sends what is due and settles what no sync confirmed in time, then waits for
    /// [`Writeback::wake`] or the next poll; runs until the process ends.
    pub async fn run(self: Arc<Self>) {
        let mut last_settle: Option<Timestamp> = None;
        loop {
            let now = self.now();
            if last_settle.is_none_or(|at| {
                now.as_millis() - at.as_millis() >= millis(self.config.settle_every)
            }) {
                last_settle = Some(now);
                if let Err(error) = self.settle_all().await {
                    tracing::warn!(%error, "settling mail changes failed");
                }
            }
            match self.drain_due().await {
                Ok(0) => {}
                Ok(_) => continue,
                Err(error) => tracing::warn!(%error, "mail writeback pass failed"),
            }
            let _ = tokio::time::timeout(self.config.poll, self.wake.notified()).await;
        }
    }

    async fn follow(&self, change: &MailChange) -> Result<Place, StoreError> {
        let (Some(message), Some(mut uid)) = (change.new.message_id, change.new.uid) else {
            return Ok(Place::Here);
        };
        if matches!(change.new.op, MailOp::Append { .. }) {
            return Ok(Place::Here);
        }
        let mut folder = change.new.folder_id;
        let mut moved = false;
        for _ in 0..MAX_FOLLOWED_MOVES {
            let Some(earlier) = self
                .store
                .applied_move_from(message, folder, uid, change.id)
                .await?
            else {
                break;
            };
            let MailOp::Move { target } = earlier.new.op else {
                break;
            };
            let Some(next) = earlier.result_uid else {
                return Ok(Place::Lost);
            };
            folder = target;
            uid = next;
            moved = true;
        }
        if !moved {
            return Ok(Place::Here);
        }
        match change.new.op {
            MailOp::Move { target } if target == folder => Ok(Place::Arrived(uid)),
            _ => Ok(Place::At(folder, uid)),
        }
    }

    async fn attempt(&self, change: &MailChange) -> Attempt {
        let followed;
        let change = match self.follow(change).await {
            Ok(Place::Here) => change,
            Ok(Place::Arrived(uid)) => return Attempt::Done(Some(uid)),
            Ok(Place::Lost) => return Attempt::Fail("moved"),
            Ok(Place::At(folder, uid)) => {
                match self
                    .store
                    .retarget_mail_change(change.id, folder, uid, self.now())
                    .await
                {
                    Ok(Some(moved)) => {
                        followed = moved;
                        &followed
                    }
                    Ok(None) => return Attempt::Retry("store"),
                    Err(StoreError::Invalid(_)) => return Attempt::Fail("invalid"),
                    Err(_) => return Attempt::Retry("store"),
                }
            }
            Err(_) => return Attempt::Retry("store"),
        };
        let writer = match self
            .writers
            .writer(change.new.account_id, &change.new.op)
            .await
        {
            Ok(Some(writer)) => writer,
            Ok(None) => return Attempt::Fail("not_allowed"),
            Err(error) => return classify(&error),
        };
        let folder = match self.folder_path(change.new.folder_id).await {
            Ok(Some(path)) => path,
            Ok(None) => return Attempt::Fail("unknown_folder"),
            Err(_) => return Attempt::Retry("store"),
        };
        let uid = change.new.uid;
        let result = match &change.new.op {
            MailOp::Flags { add, remove } => {
                let Some(uid) = uid else {
                    return Attempt::Fail("invalid");
                };
                writer
                    .set_flags(&folder, &[uid], &parse(add), &parse(remove), None)
                    .await
                    .map(|done| {
                        if done.updated.contains(&uid) {
                            Attempt::Done(None)
                        } else {
                            Attempt::Fail("gone")
                        }
                    })
            }
            MailOp::Labels { add, remove } => {
                let Some(uid) = uid else {
                    return Attempt::Fail("invalid");
                };
                writer
                    .set_labels(&[uid], add, remove)
                    .await
                    .map(|()| Attempt::Done(None))
            }
            MailOp::Move { target } => {
                let Some(uid) = uid else {
                    return Attempt::Fail("invalid");
                };
                let target = match self.folder_path(*target).await {
                    Ok(Some(path)) => path,
                    Ok(None) => return Attempt::Fail("unknown_folder"),
                    Err(_) => return Attempt::Retry("store"),
                };
                writer.move_to(&folder, &[uid], &target).await.map(|moved| {
                    Attempt::Done(
                        moved
                            .uids
                            .iter()
                            .find(|(from, _)| *from == uid)
                            .and_then(|(_, to)| *to),
                    )
                })
            }
            MailOp::Delete { expunge } => {
                let Some(uid) = uid else {
                    return Attempt::Fail("invalid");
                };
                writer
                    .delete(&folder, &[uid], *expunge)
                    .await
                    .map(|()| Attempt::Done(None))
            }
            MailOp::Append {
                blob_id,
                flags,
                date,
            } => {
                let raw = match self.blobs.get(blob_id).await {
                    Ok(Some(raw)) => raw,
                    Ok(None) => return Attempt::Fail("missing_bytes"),
                    Err(_) => return Attempt::Retry("blob"),
                };
                writer
                    .append(&folder, &raw, &parse(flags), *date)
                    .await
                    .map(Attempt::Done)
            }
        };
        result.unwrap_or_else(|error| classify(&error))
    }

    async fn folder_path(&self, folder: FolderId) -> Result<Option<String>, StoreError> {
        Ok(self.store.get_folder(folder).await?.map(|f| f.path))
    }

    async fn record(&self, change: &MailChange, attempt: Attempt) -> Result<(), WritebackError> {
        let now = self.now();
        let kind = change.new.op.kind();
        match attempt {
            Attempt::Done(result_uid) => {
                self.store
                    .mark_mail_change_sent(change.id, result_uid, now)
                    .await?;
                tracing::info!(change = %change.id, kind, "mail change accepted by the server");
            }
            Attempt::Retry(code) => {
                let next = Timestamp::from_millis(
                    now.as_millis()
                        .saturating_add(millis(self.backoff(change.attempts))),
                );
                let deadline = change
                    .new
                    .at
                    .as_millis()
                    .saturating_add(millis(self.config.retry_for));
                if next.as_millis() <= deadline {
                    self.store
                        .retry_mail_change(change.id, next, code, now)
                        .await?;
                    tracing::warn!(change = %change.id, kind, code, attempts = change.attempts,
                        "mail change not sent, trying again later");
                } else {
                    self.store.fail_mail_change(change.id, code, now).await?;
                    tracing::warn!(change = %change.id, kind, code, attempts = change.attempts,
                        "mail change given up after retries");
                }
            }
            Attempt::Fail(code) => {
                self.store.fail_mail_change(change.id, code, now).await?;
                tracing::warn!(change = %change.id, kind, code, "mail change refused");
            }
        }
        Ok(())
    }

    fn backoff(&self, attempts: u32) -> Duration {
        self.config
            .first_retry
            .saturating_mul(2u32.saturating_pow(attempts.saturating_sub(1)))
            .min(self.config.max_retry)
    }
}

fn parse(names: &[String]) -> Vec<Flag> {
    names.iter().map(|name| Flag::parse(name)).collect()
}

fn classify(error: &SourceError) -> Attempt {
    match error {
        SourceError::Connection(_) => Attempt::Retry("connection"),
        SourceError::Protocol(_) => Attempt::Retry("protocol"),
        SourceError::Auth(_) => Attempt::Retry("auth"),
        SourceError::Config(_) => Attempt::Fail("config"),
        SourceError::Unsupported(_) => Attempt::Fail("unsupported"),
        SourceError::Missing(_) => Attempt::Fail("missing_folder"),
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
