//! Archiving an account's address books and calendars: new and renamed collections, a version
//! for every change, removals marked and never deleted, and recovery by a full listing when a
//! sync token expires or the server keeps none.
//!
//! The engine talks to the server only through [`CollectionSource`] and stores through
//! [`Ingest`], so every version goes the same way as one imported from a file.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use holtmail_collection::{CollectionError, CollectionKind, CollectionSource, ItemTag};
use holtmail_core::{AccountId, ArchiveId, CollectionId, Timestamp};
use holtmail_ingest::{IncomingItem, Ingest, IngestError};
use holtmail_store::{Collection, NewCollection, Store, StoreError};

/// Items fetched from the server at a time.
pub const FETCH_BATCH: usize = 50;

/// Why a sync could not run at all.
#[derive(Debug, thiserror::Error)]
pub enum PimSyncError {
    /// The server could not list the account's collections.
    #[error(transparent)]
    Source(#[from] CollectionError),
    /// The metadata store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// How a collection's changes were read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeMode {
    /// The collection's tag had not changed, so nothing was asked.
    Unchanged,
    /// Changes since the stored token.
    SinceToken,
    /// Every item, for the first sync or a collection seen again.
    Full,
    /// Every item, because the server no longer accepted the stored token.
    TokenExpired,
    /// Every item's tag, because the server cannot list changes.
    Listing,
}

/// What a sync did to one collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionReport {
    /// The stored collection.
    pub collection: CollectionId,
    /// Address book or calendar.
    pub kind: CollectionKind,
    /// Whether it was stored for the first time.
    pub created: bool,
    /// How its changes were read.
    pub mode: ChangeMode,
    /// Items stored for the first time.
    pub new: u64,
    /// Items that got a new version, or came back after being marked removed.
    pub changed: u64,
    /// Items fetched whose bytes equal their latest version.
    pub same: u64,
    /// Items newly marked removed.
    pub removed: u64,
    /// Items that could not be stored.
    pub failed: u64,
    /// Why the collection's sync stopped; its token is then kept for the next run.
    pub error: Option<String>,
}

/// What a sync of one account did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PimReport {
    /// Each collection the server listed, in its order.
    pub collections: Vec<CollectionReport>,
    /// Stored collections of the account the server no longer lists, newly marked removed with
    /// their items.
    pub removed_collections: u64,
}

impl PimReport {
    /// Items stored for the first time, in all collections.
    pub fn new_items(&self) -> u64 {
        self.collections.iter().map(|c| c.new).sum()
    }

    /// Items with a new version.
    pub fn changed_items(&self) -> u64 {
        self.collections.iter().map(|c| c.changed).sum()
    }

    /// Items newly marked removed.
    pub fn removed_items(&self) -> u64 {
        self.collections.iter().map(|c| c.removed).sum()
    }

    /// The first collection error, if any.
    pub fn error(&self) -> Option<&str> {
        self.collections.iter().find_map(|c| c.error.as_deref())
    }
}

fn store_kind(kind: CollectionKind) -> holtmail_store::CollectionKind {
    match kind {
        CollectionKind::Contacts => holtmail_store::CollectionKind::Contacts,
        CollectionKind::Calendar => holtmail_store::CollectionKind::Calendar,
    }
}

fn format(kind: CollectionKind, content_type: &str) -> holtmail_pim::Format {
    holtmail_pim::Format::from_content_type(content_type)
        .filter(|f| f.is_contact() == (kind == CollectionKind::Contacts))
        .unwrap_or(match kind {
            CollectionKind::Contacts => holtmail_pim::Format::VCard,
            CollectionKind::Calendar => holtmail_pim::Format::ICalendar,
        })
}

/// Archives the address books and calendars of accounts.
#[derive(Clone)]
pub struct PimSyncer {
    store: Arc<dyn Store>,
    ingest: Ingest,
}

struct Read {
    tags: Vec<ItemTag>,
    removed: Vec<String>,
    token: Option<String>,
    complete: bool,
    mode: ChangeMode,
}

impl PimSyncer {
    /// A syncer storing through `ingest`.
    pub fn new(store: Arc<dyn Store>, ingest: Ingest) -> Self {
        Self { store, ingest }
    }

    /// Syncs every collection `source` lists into `archive` for `account`.
    ///
    /// A collection is stored on first sight and renamed or recolored as the server says; one
    /// the server no longer lists is marked removed with its items. Only items whose version
    /// tag differs from the stored one are fetched, and a version is added only when the bytes
    /// changed. Without a token, after an expired one, or on a server that cannot list
    /// changes, every item is listed and stored items missing from the listing are marked
    /// removed. Nothing is ever deleted. A failure in one collection is reported in its
    /// [`CollectionReport::error`] and leaves its token for the next run; the others go on.
    /// Running it again without server changes changes nothing.
    pub async fn sync(
        &self,
        archive: ArchiveId,
        account: AccountId,
        source: &dyn CollectionSource,
    ) -> Result<PimReport, PimSyncError> {
        let remote = source.collections().await?;
        let stored: Vec<Collection> = self
            .store
            .list_collections(archive)
            .await?
            .into_iter()
            .map(|c| c.collection)
            .filter(|c| c.account_id == Some(account))
            .collect();
        let mut report = PimReport::default();
        let mut listed = HashSet::new();
        for collection in &remote {
            let kind = store_kind(collection.kind);
            listed.insert((kind, collection.id.clone()));
            let before = stored
                .iter()
                .find(|c| c.kind == kind && c.remote_id == collection.id)
                .cloned();
            let saved = self
                .store
                .upsert_collection(NewCollection {
                    archive_id: archive,
                    account_id: Some(account),
                    kind,
                    remote_id: collection.id.clone(),
                    name: collection.name.clone(),
                    color: collection.color.clone(),
                })
                .await?;
            let mut entry = CollectionReport {
                collection: saved.id,
                kind: collection.kind,
                created: before.is_none(),
                mode: ChangeMode::Full,
                new: 0,
                changed: 0,
                same: 0,
                removed: 0,
                failed: 0,
                error: None,
            };
            let (token, ctag) = match &before {
                Some(b) if b.removed_at.is_none() => (b.sync_token.clone(), b.ctag.clone()),
                _ => (None, None),
            };
            if collection.tag.is_some() && collection.tag == ctag {
                entry.mode = ChangeMode::Unchanged;
                self.store
                    .set_collection_synced(
                        saved.id,
                        token.as_deref(),
                        ctag.as_deref(),
                        Timestamp::now(),
                    )
                    .await?;
            } else if let Err(error) = self
                .collection(
                    source,
                    &collection.id,
                    collection.kind,
                    saved.id,
                    token,
                    collection.tag.as_deref(),
                    &mut entry,
                )
                .await
            {
                tracing::warn!(collection = %saved.id, %error, "collection sync failed");
                entry.error = Some(error);
            }
            report.collections.push(entry);
        }
        for gone in stored
            .iter()
            .filter(|c| c.removed_at.is_none() && !listed.contains(&(c.kind, c.remote_id.clone())))
        {
            self.store
                .mark_collection_removed(gone.id, Timestamp::now())
                .await?;
            report.removed_collections += 1;
        }
        if let Err(error) = self.ingest.commit().await {
            tracing::warn!(%error, "search index not committed");
        }
        tracing::info!(
            %account,
            collections = report.collections.len(),
            new = report.new_items(),
            changed = report.changed_items(),
            removed = report.removed_items(),
            removed_collections = report.removed_collections,
            "contacts and calendars synced"
        );
        Ok(report)
    }

    async fn read(
        source: &dyn CollectionSource,
        remote: &str,
        token: Option<String>,
    ) -> Result<Read, CollectionError> {
        let full = |mode| async move {
            match source.changes(remote, None).await {
                Ok(c) => Ok(Read {
                    tags: c.changed,
                    removed: Vec::new(),
                    token: Some(c.token),
                    complete: true,
                    mode,
                }),
                Err(CollectionError::Unsupported) => Ok(Read {
                    tags: source.list(remote).await?,
                    removed: Vec::new(),
                    token: None,
                    complete: true,
                    mode: ChangeMode::Listing,
                }),
                Err(error) => Err(error),
            }
        };
        let Some(token) = token else {
            return full(ChangeMode::Full).await;
        };
        match source.changes(remote, Some(&token)).await {
            Ok(c) => Ok(Read {
                tags: c.changed,
                removed: c.removed,
                token: Some(c.token),
                complete: false,
                mode: ChangeMode::SinceToken,
            }),
            Err(CollectionError::TokenExpired) => full(ChangeMode::TokenExpired).await,
            Err(CollectionError::Unsupported) => Ok(Read {
                tags: source.list(remote).await?,
                removed: Vec::new(),
                token: None,
                complete: true,
                mode: ChangeMode::Listing,
            }),
            Err(error) => Err(error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn collection(
        &self,
        source: &dyn CollectionSource,
        remote: &str,
        kind: CollectionKind,
        id: CollectionId,
        token: Option<String>,
        tag: Option<&str>,
        entry: &mut CollectionReport,
    ) -> Result<(), String> {
        let read = Self::read(source, remote, token)
            .await
            .map_err(|e| e.to_string())?;
        entry.mode = read.mode;
        let known: BTreeMap<String, Option<String>> = self
            .store
            .item_etags(id)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect();
        let wanted: Vec<String> = read
            .tags
            .iter()
            .filter(|t| t.etag.is_none() || known.get(&t.id) != Some(&t.etag))
            .map(|t| t.id.clone())
            .collect();
        let tags: BTreeMap<&str, Option<&String>> = read
            .tags
            .iter()
            .map(|t| (t.id.as_str(), t.etag.as_ref()))
            .collect();
        let mut answered = BTreeSet::new();
        for batch in wanted.chunks(FETCH_BATCH) {
            let items = source
                .fetch(remote, batch)
                .await
                .map_err(|e| e.to_string())?;
            for item in items {
                if !answered.insert(item.id.clone()) {
                    continue;
                }
                let etag = item
                    .etag
                    .clone()
                    .or_else(|| tags.get(item.id.as_str()).copied().flatten().cloned());
                let format = format(kind, &item.content_type);
                let incoming = IncomingItem {
                    collection: id,
                    kind: format.kind(&item.raw),
                    format,
                    remote_id: item.id,
                    raw: item.raw,
                    etag,
                    seen_at: Timestamp::now(),
                };
                match self.ingest.ingest_item(incoming).await {
                    Ok(recorded) if recorded.created_item => entry.new += 1,
                    Ok(recorded) if recorded.new_version || recorded.restored => entry.changed += 1,
                    Ok(_) => entry.same += 1,
                    Err(IngestError::Store(error)) => return Err(error.to_string()),
                    Err(error) => {
                        tracing::warn!(collection = %id, %error, "item not stored");
                        entry.failed += 1;
                    }
                }
            }
        }
        let gone: Vec<String> = if read.complete {
            known
                .keys()
                .filter(|k| !tags.contains_key(k.as_str()))
                .cloned()
                .collect()
        } else {
            read.removed
        };
        if !gone.is_empty() {
            entry.removed = self
                .store
                .mark_items_removed(id, &gone, Timestamp::now())
                .await
                .map_err(|e| e.to_string())?;
        }
        let (token, tag) = if entry.failed > 0 {
            (None, None)
        } else {
            (read.token, tag)
        };
        self.store
            .set_collection_synced(id, token.as_deref(), tag, Timestamp::now())
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
