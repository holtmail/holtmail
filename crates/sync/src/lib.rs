//! Archiving an account's folders incrementally.
//!
//! For each folder the sync remembers `UIDVALIDITY` and the highest archived UID, fetches only newer
//! messages in batches (every UID not archived yet on sources whose UIDs do not ascend with
//! arrival), records messages that fail instead of stalling on them, and notices
//! messages that disappeared from the server without deleting them from the archive. Flags and
//! labels changed on the server after archiving are taken in too: since the last mod-sequence on
//! servers with `CONDSTORE`, by reading every archived message's flags once a day otherwise.
//!
//! When a source keeps a state per folder (a Graph delta link), the state is stored with the
//! cursor after a sync that left nothing behind (nothing deferred, no failure left to retry), and
//! the next sync skips the folder when the source says nothing changed since it, unless the
//! folder's daily flag scan is due: a state may have been taken by a sync that did not read flags,
//! so only the scan can find a flag change made before it. Any other sync drops the stored state
//! first, so an interrupted sync never skips unfinished work.
//!
//! With a minimum age, a folder's sync stops before its first message that is too young, so the
//! UID cursor never passes a message that was not archived yet.
//!
//! Held and sending flags and labels changes (`mail_changes`) of a folder's messages are
//! confirmed after its flags are taken in: their flags are read once more and handed to the
//! store with the time taken before the read, so the server wins when it shows something else.
//!
//! On Gmail, labels shown as folders are skipped while the all-mail folder is synced: their
//! messages arrive once, from all mail, with their labels attached.

use std::collections::BTreeSet;
use std::time::Duration;

use holtmail_core::{FolderId, Timestamp};
use holtmail_ingest::{Ingest, IngestError, Placement};
use holtmail_source::{
    FolderStatus, RemoteFlags, RemoteFolder, RemoteMessage, Source, SourceError, SourceSession,
};
use holtmail_store::{
    Account, FolderCursor, FolderRole, MailChangeQuery, MailChangeState, Store, StoreError,
};
use serde::Serialize;

const CONFIRM_LIMIT: u32 = 500;

/// Which folders to archive and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOptions {
    /// Only these folder paths, when set. A label folder named here is synced even when the
    /// all-mail folder is.
    pub include: Option<Vec<String>>,
    /// Never these folder paths.
    pub exclude: Vec<String>,
    /// Messages fetched per round trip.
    pub batch: usize,
    /// A message that failed this many times is not retried.
    pub max_attempts: u32,
    /// On servers without mod-sequences, how often the flags of every archived message in a
    /// folder are read again.
    pub flag_scan_interval: Duration,
    /// UIDs covered by one flag read on such servers.
    pub flag_batch: usize,
    /// Archive only messages at least this old, by when the server received them, or by their
    /// `Date` header when the server does not say. A folder's sync stops at its first newer
    /// message: that message and every later one are left on the server for a sync once they
    /// are old enough, so none is skipped for good. Messages without either date are archived.
    pub min_age: Option<Duration>,
    /// Look only at these folder paths in this run, when set, on top of `include` and
    /// `exclude`: a run started because a watch saw these folders change. Which folders are
    /// skipped as labels is decided as for a full run, so naming a label folder while the
    /// all-mail folder is synced looks at nothing.
    pub only: Option<Vec<String>>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            include: None,
            exclude: Vec::new(),
            batch: 50,
            max_attempts: 5,
            flag_scan_interval: Duration::from_secs(24 * 60 * 60),
            flag_batch: 1000,
            min_age: None,
            only: None,
        }
    }
}

/// What happened in one folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FolderReport {
    /// The folder path.
    pub path: String,
    /// Messages added to the archive.
    pub new: u64,
    /// Messages the archive already had, now also located here.
    pub known: u64,
    /// Messages no longer on the server.
    pub removed: u64,
    /// Archived messages whose flags or labels changed on the server.
    pub flags_changed: u64,
    /// Messages that could not be archived this time.
    pub failed: u64,
    /// Messages left out because they were deleted from the archive on purpose.
    pub deleted: u64,
    /// New messages left for a later sync because the first of them is younger than
    /// [`SyncOptions::min_age`].
    pub deferred: u64,
    /// The server renumbered the folder (`UIDVALIDITY` changed).
    pub renumbered: bool,
    /// The source said nothing changed since the folder's stored state, so it was not listed.
    pub unchanged: bool,
    /// Why the folder was not finished, when it was not.
    pub error: Option<String>,
}

/// What one sync of an account did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// One report per folder that was looked at.
    pub folders: Vec<FolderReport>,
}

impl SyncReport {
    /// Messages added across folders.
    pub fn new_messages(&self) -> u64 {
        self.folders.iter().map(|f| f.new).sum()
    }
}

/// Why a sync stopped.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The server could not be used at all.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// Holtmail's own storage failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Storing or indexing a message failed for a reason other than the message itself.
    #[error(transparent)]
    Ingest(IngestError),
}

/// Syncs accounts into their archives.
#[derive(Clone)]
pub struct Syncer {
    store: std::sync::Arc<dyn Store>,
    ingest: Ingest,
}

impl Syncer {
    /// A syncer writing through `ingest` and keeping cursors in `store`.
    pub fn new(store: std::sync::Arc<dyn Store>, ingest: Ingest) -> Self {
        Self { store, ingest }
    }

    /// Archives everything new in the account's folders.
    ///
    /// Safe to run again at any time and to interrupt: cursors are saved after every batch. A
    /// connection that breaks inside a folder ends that folder with an `error` in its report and
    /// moves on to the next one.
    pub async fn sync_account(
        &self,
        account: &Account,
        source: &dyn Source,
        options: &SyncOptions,
    ) -> Result<SyncReport, SyncError> {
        let mut session = source.connect().await?;
        let mut report = SyncReport::default();
        let mut folders = session.folders().await?;
        folders.sort_by(|a, b| a.path.cmp(&b.path));
        let all_mail = folders
            .iter()
            .any(|f| f.role == Some(FolderRole::All) && wanted(f, options));
        for remote in folders.into_iter().filter(|f| {
            wanted(f, options)
                && !(f.label && all_mail && !named(f, options.include.as_deref()))
                && named_or_all(f, options.only.as_deref())
        }) {
            let folder = self
                .store
                .upsert_folder(account.id, &remote.path, remote.role)
                .await?;
            let mut folder_report = FolderReport {
                path: remote.path.clone(),
                ..FolderReport::default()
            };
            match self
                .sync_folder(
                    account,
                    &mut *session,
                    &remote,
                    folder.id,
                    options,
                    &mut folder_report,
                )
                .await
            {
                Ok(()) => {}
                Err(SyncError::Source(error)) if error.is_transient() => {
                    tracing::warn!(folder = %folder.id, %error, "folder sync interrupted");
                    folder_report.error = Some(error.to_string());
                }
                Err(error) => return Err(error),
            }
            report.folders.push(folder_report);
        }
        let _ = session.logout().await;
        Ok(report)
    }

    async fn sync_folder(
        &self,
        account: &Account,
        session: &mut dyn SourceSession,
        remote: &RemoteFolder,
        folder: FolderId,
        options: &SyncOptions,
        report: &mut FolderReport,
    ) -> Result<(), SyncError> {
        let status = session.select(&remote.path).await?;
        let mut cursor = self.store.cursor(folder).await?;
        if cursor
            .uid_validity
            .is_some_and(|v| v != status.uid_validity)
        {
            self.store.forget_uids(folder).await?;
            cursor = FolderCursor::default();
            report.renumbered = true;
        }
        cursor.uid_validity = Some(status.uid_validity);
        let archived_up_to = cursor.last_uid;

        let interval = i64::try_from(options.flag_scan_interval.as_millis()).unwrap_or(i64::MAX);
        let flag_scan_due = status.highest_modseq.is_none()
            && cursor.last_uid.is_some()
            && cursor.flags_checked_at.is_none_or(|checked| {
                Timestamp::now()
                    .as_millis()
                    .saturating_sub(checked.as_millis())
                    >= interval
            });
        if let Some(state) = cursor.state.take() {
            if !flag_scan_due && let Some(kept) = session.unchanged_since(&state).await? {
                cursor.state = Some(kept);
                cursor.synced_at = Some(Timestamp::now());
                self.store.set_cursor(folder, &cursor).await?;
                report.unchanged = true;
                return Ok(());
            }
            self.store.set_cursor(folder, &cursor).await?;
        }

        let on_server: BTreeSet<u64> = session.uids().await?.into_iter().collect();
        let archived: BTreeSet<u64> = self.store.folder_uids(folder).await?.into_iter().collect();
        let gone: Vec<u64> = archived
            .iter()
            .copied()
            .filter(|uid| !on_server.contains(uid))
            .collect();
        if !gone.is_empty() {
            let mut affected = Vec::new();
            for uid in &gone {
                if let Some(message) = self.store.message_at_uid(folder, *uid).await? {
                    affected.push(message);
                }
            }
            self.store
                .mark_removed(folder, &gone, Timestamp::now())
                .await?;
            for message in affected {
                self.ingest.reindex(message).await.map_err(ingest_error)?;
            }
            report.removed = gone.len() as u64;
        }

        self.refresh_flags(
            session,
            folder,
            &status,
            &mut cursor,
            archived_up_to,
            &on_server,
            options,
            report,
        )
        .await?;
        self.store.set_cursor(folder, &cursor).await?;
        self.confirm_changes(account, session, folder, &on_server, report)
            .await?;

        let failures = self.store.failures(folder).await?;
        let retry: BTreeSet<u64> = failures
            .iter()
            .filter(|f| f.attempts < options.max_attempts && on_server.contains(&f.uid))
            .map(|f| f.uid)
            .collect();
        let given_up: BTreeSet<u64> = failures
            .iter()
            .filter(|f| f.attempts >= options.max_attempts)
            .map(|f| f.uid)
            .collect();
        let last = cursor.last_uid.unwrap_or(0);
        let live = cursor.synced_at.is_some();
        let fresh = |uid: u64| {
            if status.uids_ascend {
                uid > last
            } else {
                !retry.contains(&uid)
            }
        };
        let wanted: Vec<u64> = on_server
            .iter()
            .copied()
            .filter(|uid| {
                if status.uids_ascend {
                    *uid > last || retry.contains(uid)
                } else {
                    !archived.contains(uid) && !given_up.contains(uid)
                }
            })
            .collect();

        let cutoff = options.min_age.map(|age| {
            Timestamp::from_millis(
                Timestamp::now()
                    .as_millis()
                    .saturating_sub(i64::try_from(age.as_millis()).unwrap_or(i64::MAX)),
            )
        });
        for batch in wanted.chunks(options.batch.max(1)) {
            let mut fetched = session.fetch(batch).await?;
            fetched.sort_by_key(|m| m.uid);
            let mut young = None;
            for message in fetched {
                if let Some(cutoff) = cutoff
                    && fresh(message.uid)
                    && newer_than(&message, cutoff)
                {
                    young = Some(message.uid);
                    break;
                }
                let placement = Placement {
                    folder: Some(folder),
                    uid: Some(message.uid),
                    flags: message.flags,
                    labels: message.labels,
                    received_at: message.received_at,
                    live,
                };
                match self
                    .ingest
                    .ingest(account.archive_id, message.raw, &placement)
                    .await
                {
                    Ok(done) => {
                        if done.created {
                            report.new += 1;
                        } else {
                            report.known += 1;
                        }
                        if retry.contains(&message.uid) {
                            self.store.clear_failure(folder, message.uid).await?;
                        }
                    }
                    Err(IngestError::Deleted) => {
                        report.deleted += 1;
                        if retry.contains(&message.uid) {
                            self.store.clear_failure(folder, message.uid).await?;
                        }
                    }
                    Err(IngestError::Mime(error)) => {
                        report.failed += 1;
                        self.store
                            .record_failure(
                                folder,
                                message.uid,
                                &error.to_string(),
                                Timestamp::now(),
                            )
                            .await?;
                    }
                    Err(error) => return Err(ingest_error(error)),
                }
            }
            let before = young.unwrap_or(u64::MAX);
            if let Some(max) = batch
                .iter()
                .copied()
                .filter(|uid| *uid > last && *uid < before)
                .max()
            {
                cursor.last_uid = Some(cursor.last_uid.unwrap_or(0).max(max));
            }
            self.store.set_cursor(folder, &cursor).await?;
            self.ingest.commit().await.map_err(ingest_error)?;
            if let Some(young) = young {
                report.deferred = wanted.iter().filter(|uid| **uid >= young).count() as u64;
                break;
            }
        }

        let pending = report.deferred > 0
            || self
                .store
                .failures(folder)
                .await?
                .iter()
                .any(|f| f.attempts < options.max_attempts && on_server.contains(&f.uid));
        cursor.state = if pending { None } else { session.state() };
        cursor.synced_at = Some(Timestamp::now());
        self.store.set_cursor(folder, &cursor).await?;
        self.ingest.commit().await.map_err(ingest_error)?;
        Ok(())
    }

    /// Takes in flag and label changes of messages archived up to `archived_up_to` and moves the
    /// cursor's mod-sequence and flag check time on.
    #[allow(clippy::too_many_arguments)]
    async fn refresh_flags(
        &self,
        session: &mut dyn SourceSession,
        folder: FolderId,
        status: &FolderStatus,
        cursor: &mut FolderCursor,
        archived_up_to: Option<u64>,
        on_server: &BTreeSet<u64>,
        options: &SyncOptions,
        report: &mut FolderReport,
    ) -> Result<(), SyncError> {
        let now = Timestamp::now();
        let Some(up_to) = archived_up_to else {
            cursor.highest_modseq = status.highest_modseq;
            cursor.flags_checked_at = Some(now);
            return Ok(());
        };
        let checked = cursor.flags_checked_at.is_some();
        let changes = match (status.highest_modseq, cursor.highest_modseq) {
            (Some(current), Some(previous)) if checked && current == previous => Vec::new(),
            (Some(current), Some(previous)) if checked && current > previous => {
                session.flags(1..=up_to, Some(previous)).await?
            }
            (current, previous) => {
                let interval =
                    i64::try_from(options.flag_scan_interval.as_millis()).unwrap_or(i64::MAX);
                let due = current.is_some()
                    || previous.is_some()
                    || cursor.flags_checked_at.is_none_or(|checked| {
                        now.as_millis().saturating_sub(checked.as_millis()) >= interval
                    });
                if !due {
                    cursor.highest_modseq = current;
                    return Ok(());
                }
                let archived: Vec<u64> = on_server.range(..=up_to).copied().collect();
                let mut changes = Vec::new();
                for chunk in archived.chunks(options.flag_batch.max(1)) {
                    let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
                        continue;
                    };
                    changes.extend(session.flags(*first..=*last, None).await?);
                }
                cursor.flags_checked_at = Some(now);
                changes
            }
        };
        for change in changes.into_iter().filter(|c| c.uid <= up_to) {
            if self.apply(folder, change).await? {
                report.flags_changed += 1;
            }
        }
        if report.flags_changed > 0 {
            self.ingest.commit().await.map_err(ingest_error)?;
        }
        cursor.highest_modseq = status.highest_modseq;
        Ok(())
    }

    /// Reads the flags (and labels) of the folder's messages with held or sending flags or
    /// labels changes and tells the store what they are now, so a change the server shows is
    /// applied and one it contradicts after it was sent is superseded: the server wins. The
    /// time passed along is taken before the read.
    async fn confirm_changes(
        &self,
        account: &Account,
        session: &mut dyn SourceSession,
        folder: FolderId,
        on_server: &BTreeSet<u64>,
        report: &mut FolderReport,
    ) -> Result<(), SyncError> {
        let open = self
            .store
            .list_mail_changes(&MailChangeQuery {
                account: account.id,
                states: vec![MailChangeState::Held, MailChangeState::Sending],
                limit: CONFIRM_LIMIT,
            })
            .await?;
        let uids: BTreeSet<u64> = open
            .iter()
            .filter(|c| c.new.folder_id == folder && c.new.op.needs_confirmation())
            .filter_map(|c| c.new.uid)
            .filter(|uid| on_server.contains(uid))
            .collect();
        if uids.is_empty() {
            return Ok(());
        }
        let read_at = Timestamp::now();
        let mut changed = false;
        for uid in uids {
            for seen in session.flags(uid..=uid, None).await? {
                if seen.uid != uid {
                    continue;
                }
                self.store
                    .confirm_mail_flags(folder, uid, &seen.flags, seen.labels.as_deref(), read_at)
                    .await?;
                if self.apply(folder, seen).await? {
                    report.flags_changed += 1;
                    changed = true;
                }
            }
        }
        if changed {
            self.ingest.commit().await.map_err(ingest_error)?;
        }
        Ok(())
    }

    async fn apply(&self, folder: FolderId, change: RemoteFlags) -> Result<bool, SyncError> {
        let Some(message) = self.store.message_at_uid(folder, change.uid).await? else {
            return Ok(false);
        };
        let mut changed = self
            .store
            .set_flags(folder, change.uid, &change.flags)
            .await?;
        if let Some(labels) = &change.labels {
            changed |= self.store.set_labels(message, labels).await?;
        }
        if changed {
            self.ingest.reindex(message).await.map_err(ingest_error)?;
        }
        Ok(changed)
    }
}

fn newer_than(message: &RemoteMessage, cutoff: Timestamp) -> bool {
    let date = message.received_at.or_else(|| {
        holtmail_mime::parse(&message.raw)
            .ok()
            .and_then(|parsed| parsed.sent_at)
            .map(Timestamp::from_secs)
    });
    date.is_some_and(|date| date > cutoff)
}

fn named(folder: &RemoteFolder, include: Option<&[String]>) -> bool {
    include.is_some_and(|include| include.iter().any(|p| p == &folder.path))
}

fn named_or_all(folder: &RemoteFolder, only: Option<&[String]>) -> bool {
    only.is_none_or(|only| only.iter().any(|p| p == &folder.path))
}

fn wanted(folder: &RemoteFolder, options: &SyncOptions) -> bool {
    folder.selectable
        && options
            .include
            .as_ref()
            .is_none_or(|include| include.iter().any(|p| p == &folder.path))
        && !options.exclude.iter().any(|p| p == &folder.path)
}

fn ingest_error(error: IngestError) -> SyncError {
    match error {
        IngestError::Store(error) => SyncError::Store(error),
        other => SyncError::Ingest(other),
    }
}

#[cfg(test)]
mod tests;
