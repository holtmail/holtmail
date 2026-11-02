//! Archiving an account's folders incrementally.
//!
//! For each folder the sync remembers `UIDVALIDITY` and the highest archived UID, fetches only newer
//! messages in batches, records messages that fail instead of stalling on them, and notices
//! messages that disappeared from the server without deleting them from the archive.

use std::collections::BTreeSet;

use holtmail_core::{FolderId, Timestamp};
use holtmail_ingest::{Ingest, IngestError, Placement};
use holtmail_source::{RemoteFolder, Source, SourceError, SourceSession};
use holtmail_store::{Account, FolderCursor, Store, StoreError};
use serde::Serialize;

/// Which folders to archive and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOptions {
    /// Only these folder paths, when set.
    pub include: Option<Vec<String>>,
    /// Never these folder paths.
    pub exclude: Vec<String>,
    /// Messages fetched per round trip.
    pub batch: usize,
    /// A message that failed this many times is not retried.
    pub max_attempts: u32,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            include: None,
            exclude: Vec::new(),
            batch: 50,
            max_attempts: 5,
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
    /// Messages that could not be archived this time.
    pub failed: u64,
    /// The server renumbered the folder (`UIDVALIDITY` changed).
    pub renumbered: bool,
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
        for remote in folders.into_iter().filter(|f| wanted(f, options)) {
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

        let on_server: BTreeSet<u64> = session.uids().await?.into_iter().collect();
        let gone: Vec<u64> = self
            .store
            .folder_uids(folder)
            .await?
            .into_iter()
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

        let retry: BTreeSet<u64> = self
            .store
            .failures(folder)
            .await?
            .into_iter()
            .filter(|f| f.attempts < options.max_attempts && on_server.contains(&f.uid))
            .map(|f| f.uid)
            .collect();
        let last = cursor.last_uid.unwrap_or(0);
        let wanted: Vec<u64> = on_server
            .iter()
            .copied()
            .filter(|uid| *uid > last || retry.contains(uid))
            .collect();

        for batch in wanted.chunks(options.batch.max(1)) {
            let fetched = session.fetch(batch).await?;
            for message in fetched {
                let placement = Placement {
                    folder: Some(folder),
                    uid: Some(message.uid),
                    flags: message.flags,
                    received_at: message.received_at,
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
            if let Some(max) = batch.iter().copied().filter(|uid| *uid > last).max() {
                cursor.last_uid = Some(cursor.last_uid.unwrap_or(0).max(max));
            }
            cursor.highest_modseq = status.highest_modseq;
            self.store.set_cursor(folder, &cursor).await?;
            self.ingest.commit().await.map_err(ingest_error)?;
        }

        cursor.synced_at = Some(Timestamp::now());
        cursor.highest_modseq = status.highest_modseq;
        self.store.set_cursor(folder, &cursor).await?;
        self.ingest.commit().await.map_err(ingest_error)?;
        Ok(())
    }
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
