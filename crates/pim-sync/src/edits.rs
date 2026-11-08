use std::sync::Arc;

use bytes::Bytes;
use holtmail_blob::{BlobError, BlobStore};
use holtmail_core::{ArchiveId, ItemId, Timestamp};
use holtmail_ingest::{Ingest, IngestError};
use holtmail_pim::{Format, ItemKind};
use holtmail_pim_edit::{Patch, Rebased};
use holtmail_store::{
    ChangeOp, ChangeTarget, Collection, CollectionKind, Effective, ItemChange, ItemChangeQuery,
    ItemChangeState, ItemVersion, Resolution, Store, StoreError,
};

/// Why an edits operation failed.
#[derive(Debug, thiserror::Error)]
pub enum EditsError {
    /// The metadata store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The blob store failed.
    #[error(transparent)]
    Blob(#[from] BlobError),
    /// Rebuilding derived data failed.
    #[error(transparent)]
    Ingest(#[from] IngestError),
    /// Bytes a change or a version refers to are missing from the blob store.
    #[error("item bytes missing")]
    Missing,
}

/// The bytes served for an item or a held create: iCalendar or vCard text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// The bytes.
    pub raw: Bytes,
    /// `text/calendar` or `text/vcard`.
    pub content_type: String,
    /// What they are.
    pub kind: ItemKind,
}

/// What [`Edits::reconcile`] did with a collection's flagged changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Upstream now holds them: `applied`.
    pub applied: u64,
    /// Re-applied silently on the newer upstream version.
    pub rebased: u64,
    /// Now conflicts (`overlap`, `reply_obsolete`, `upstream_deleted`, `upstream_changed`,
    /// `upstream_unreadable`).
    pub conflicts: u64,
}

/// Held edits of contacts and events seen against upstream: what is served for them, the
/// post-batch step after a sync, and derived data that follows what is served.
#[derive(Clone)]
pub struct Edits {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    ingest: Ingest,
}

fn kind_of(collection: CollectionKind, item: Option<holtmail_store::ItemKind>) -> ItemKind {
    match (collection, item) {
        (_, Some(kind)) => ItemKind::parse(kind.as_str()).unwrap_or(ItemKind::Other),
        (CollectionKind::Contacts, None) => ItemKind::Contact,
        (_, None) => ItemKind::Event,
    }
}

fn patch_of(change: &ItemChange) -> Option<Patch> {
    serde_json::from_value(change.patch.clone()?).ok()
}

impl Edits {
    /// Edits over these stores; `ingest` rebuilds what is derived from items.
    pub fn new(store: Arc<dyn Store>, ingest: Ingest) -> Self {
        Self {
            store,
            blobs: ingest.blobs(),
            ingest,
        }
    }

    /// The addresses that count as the archive's own when matching the user's attendee entry:
    /// its accounts', identities' and own aliases'.
    pub async fn own_addresses(&self, archive: ArchiveId) -> Result<Vec<String>, StoreError> {
        let mut own: Vec<String> = self
            .store
            .list_accounts(archive)
            .await?
            .into_iter()
            .filter_map(|a| a.address)
            .collect();
        own.extend(
            self.store
                .list_identities(archive)
                .await?
                .into_iter()
                .map(|i| i.fields.address),
        );
        own.extend(self.store.own_alias_addresses(archive).await?);
        Ok(own)
    }

    async fn bytes(&self, blob: &holtmail_blob::BlobId) -> Result<Bytes, EditsError> {
        self.blobs.get(blob).await?.ok_or(EditsError::Missing)
    }

    /// A stored version as iCalendar or vCard text (`holtmail_pim::as_text`, so Graph and
    /// JMAP versions are converted); `None` when it cannot be read as `kind`.
    pub async fn version_text(
        &self,
        version: &ItemVersion,
        kind: ItemKind,
    ) -> Result<Option<Bytes>, EditsError> {
        let raw = self.bytes(&version.blob_id).await?;
        let format = Format::stored(&version.content_type, kind);
        Ok(holtmail_pim::as_text(format, kind, &raw).map(|t| Bytes::copy_from_slice(&t)))
    }

    /// What is served for `effective`: the open change re-applied on the latest upstream
    /// version (`holtmail_pim_edit::serve`), else the latest version as text. `None` when
    /// nothing is served (deleted, a held delete) or the latest version cannot be read.
    pub async fn served(&self, effective: &Effective) -> Result<Option<Served>, EditsError> {
        if !effective.served {
            return Ok(None);
        }
        let collection = self
            .store
            .get_collection(effective.collection_id)
            .await?
            .ok_or(StoreError::NotFound("collection"))?;
        let kind = kind_of(
            collection.kind,
            effective.item.as_ref().map(|i| i.item.kind),
        );
        let latest = match &effective.latest {
            Some(version) => self
                .version_text(version, kind)
                .await?
                .map(|t| (version.number, t)),
            None => None,
        };
        let raw = match effective
            .change
            .as_ref()
            .filter(|c| c.op != ChangeOp::Delete)
        {
            Some(change) => {
                let Some(content) = &change.content else {
                    return Ok(latest.map(|(_, raw)| served(raw, kind)));
                };
                let mine = self.bytes(&content.blob_id).await?;
                let own = self.own_addresses(change.archive_id).await?;
                let patch = patch_of(change);
                Bytes::from(holtmail_pim_edit::serve(
                    patch.as_ref(),
                    &mine,
                    change.base_version,
                    latest.as_ref().map(|(n, raw)| (*n, raw.as_ref())),
                    &own,
                ))
            }
            None => match latest {
                Some((_, raw)) => raw,
                None => return Ok(None),
            },
        };
        Ok(Some(served(raw, kind)))
    }

    /// Makes the item's derived data (normalized fields, calendar part, search document) follow
    /// what is served for it: an open change's bytes while one is open, its stored versions
    /// otherwise. Becomes searchable after `Ingest::commit`.
    pub async fn derive(&self, item: ItemId) -> Result<(), EditsError> {
        let Some(effective) = self.store.effective(&ChangeTarget::Item { item }).await? else {
            return Ok(());
        };
        let open = effective
            .change
            .as_ref()
            .is_some_and(|c| c.op != ChangeOp::Delete);
        if !open {
            self.ingest.derive_served(item, None).await?;
            return Ok(());
        }
        match self.served(&effective).await? {
            Some(served) => {
                let format = Format::of(served.kind);
                self.ingest
                    .derive_served(item, Some((&served.raw, format)))
                    .await?
            }
            None => self.ingest.derive_served(item, None).await?,
        }
        Ok(())
    }

    /// The post-batch step after a sync of `collection` (CLIENTS.md, "When upstream changes the
    /// same item"): every open change flagged by a newer upstream version or a removal upstream
    /// is decided, and the item's derived data follows what is served afterwards.
    ///
    /// - Upstream now holds what the change meant (the same edit made upstream, an empty
    ///   change, a delete of an item gone upstream): `applied`.
    /// - A held `noop`, `personal` or `reply` change, or an edit whose fields do not overlap
    ///   upstream's: rebased silently on the newest version.
    /// - Both changed a field where upstream wins: `conflict/overlap`; a reply whose event was
    ///   cancelled or whose attendee was removed: `conflict/reply_obsolete`; the item gone
    ///   upstream: `conflict/upstream_deleted`; a delete of an item changed upstream:
    ///   `conflict/upstream_changed`; upstream that cannot be read: `conflict/upstream_unreadable`.
    ///
    /// Only held changes are decided: changes being sent are left to the writer, and conflicts
    /// and refused changes wait for a person (their flags are cleared when they settle, so they
    /// never fill the batch of 500 flagged changes). Nothing is dropped. Idempotent.
    pub async fn reconcile(&self, collection: &Collection) -> Result<Reconciled, EditsError> {
        let mut report = Reconciled::default();
        let flagged = self
            .store
            .open_changes(&ItemChangeQuery {
                archive_id: collection.archive_id,
                collection_id: Some(collection.id),
                states: vec![ItemChangeState::Held],
                flagged_only: true,
                limit: 500,
            })
            .await?;
        if flagged.is_empty() {
            return Ok(report);
        }
        let own = self.own_addresses(collection.archive_id).await?;
        for change in flagged {
            if change.state == ItemChangeState::Sending {
                continue;
            }
            let Some(item) = change.item_id else {
                continue;
            };
            let resolution = self.decide(collection, &change, item, &own).await?;
            if let Some(resolution) = resolution {
                let decided = self
                    .store
                    .resolve_change(change.id, &resolution, Timestamp::now())
                    .await?;
                if decided.is_some() {
                    match resolution {
                        Resolution::Applied => report.applied += 1,
                        Resolution::Rebased { .. } => report.rebased += 1,
                        _ => report.conflicts += 1,
                    }
                }
            }
            if let Err(error) = self.derive(item).await {
                tracing::warn!(%item, %error, "derived data not rebuilt after an upstream change");
            }
        }
        if report != Reconciled::default() {
            tracing::info!(collection = %collection.id, applied = report.applied,
                rebased = report.rebased, conflicts = report.conflicts,
                "held changes reconciled with upstream");
        }
        Ok(report)
    }

    async fn decide(
        &self,
        collection: &Collection,
        change: &ItemChange,
        item: ItemId,
        own: &[String],
    ) -> Result<Option<Resolution>, EditsError> {
        let held = change.state == ItemChangeState::Held;
        let may_conflict = matches!(
            change.state,
            ItemChangeState::Held | ItemChangeState::Rejected
        );
        let conflict = |reason: &str| {
            may_conflict.then(|| Resolution::Conflict {
                reason: reason.to_owned(),
            })
        };
        let Some(summary) = self.store.get_item(item).await? else {
            return Ok(None);
        };
        let gone = change.upstream_deleted || summary.item.removed_at.is_some();
        if change.op == ChangeOp::Delete {
            return Ok(if gone {
                Some(Resolution::Applied)
            } else {
                conflict("upstream_changed")
            });
        }
        if gone {
            return Ok(conflict("upstream_deleted"));
        }
        let kind = kind_of(collection.kind, Some(summary.item.kind));
        let Some(latest) = self.store.item_versions(item).await?.pop() else {
            return Ok(None);
        };
        let Some(theirs) = self.version_text(&latest, kind).await? else {
            return Ok(conflict("upstream_unreadable"));
        };
        let patch = match patch_of(change) {
            Some(patch) => patch,
            None => match self.patch_against_base(change, item, kind, own).await? {
                Some(patch) => patch,
                None => return Ok(conflict("upstream_unreadable")),
            },
        };
        Ok(match holtmail_pim_edit::rebase(&patch, &theirs, own) {
            Rebased::Applied => Some(Resolution::Applied),
            Rebased::Clean(_) => held.then_some(Resolution::Rebased {
                onto: latest.number,
            }),
            rebased => conflict(rebased.reason().unwrap_or("overlap")),
        })
    }

    async fn patch_against_base(
        &self,
        change: &ItemChange,
        item: ItemId,
        kind: ItemKind,
        own: &[String],
    ) -> Result<Option<Patch>, EditsError> {
        let Some(content) = &change.content else {
            return Ok(None);
        };
        let mine = self.bytes(&content.blob_id).await?;
        let versions = self.store.item_versions(item).await?;
        let base = match change
            .base_version
            .and_then(|n| versions.iter().find(|v| v.number == n))
        {
            Some(version) => self.version_text(version, kind).await?,
            None => None,
        };
        Ok(
            holtmail_pim_edit::classify(kind, base.as_deref(), &mine, own)
                .ok()
                .map(|c| c.patch),
        )
    }
}

fn served(raw: Bytes, kind: ItemKind) -> Served {
    Served {
        raw,
        content_type: kind.content_type().to_owned(),
        kind,
    }
}
