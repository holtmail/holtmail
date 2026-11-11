use std::collections::{BTreeMap, HashMap, HashSet};

use bytes::Bytes;
use holtmail_access::{AccessError, Scopes};
use holtmail_blob::{BlobError, BlobId};
use holtmail_core::{ArchiveId, CollectionId, DeviceId, ItemId, Timestamp};
use holtmail_pim::{Format, ItemKind};
use holtmail_pim_sync::EditsError;
use holtmail_store::{
    Account, Capability, ChangeKind, ChangeOp, ChangeTarget, Collection, CollectionKind, Effective,
    ItemChangeQuery, ItemOrder, ItemQuery, PersonQuery, PersonSource, RemovedFilter, StoreError,
    User, WriteMode,
};

use crate::Shared;
use crate::path::{CollectionKey, Home};
use crate::people;

/// The store or the blob store could not answer; the client is told to come back (503).
#[derive(Debug)]
pub(crate) struct Unavailable;

impl From<StoreError> for Unavailable {
    fn from(error: StoreError) -> Self {
        tracing::warn!(%error, "dav: store unavailable");
        Self
    }
}

impl From<BlobError> for Unavailable {
    fn from(error: BlobError) -> Self {
        tracing::warn!(%error, "dav: blob store unavailable");
        Self
    }
}

impl From<EditsError> for Unavailable {
    fn from(error: EditsError) -> Self {
        tracing::warn!(%error, "dav: served bytes unavailable");
        Self
    }
}

pub(crate) type R<T> = Result<T, Unavailable>;

/// Most resources one `sync-collection` answer reports before it is cut with a 507.
pub(crate) const MAX_SYNC: usize = 1_000;

/// How long a collection moved to `read_only` still takes writes as held changes.
pub(crate) const GRACE_MS: i64 = 24 * 60 * 60 * 1000;

/// The signed-in device and its user.
pub(crate) struct Caller {
    pub user: User,
    pub device: DeviceId,
    pub archives: Vec<ArchiveId>,
    pub scopes: Scopes,
    pub device_scopes: std::collections::BTreeSet<holtmail_core::DeviceScope>,
    /// Set when calendar-auto-schedule is advertised to this caller.
    pub sched: Option<crate::sched::Sched>,
}

/// A served calendar or address book.
#[derive(Debug, Clone)]
pub(crate) struct DavCollection {
    pub key: CollectionKey,
    pub home: Home,
    pub archive: ArchiveId,
    pub name: String,
    pub color: Option<String>,
    pub stored: Option<Collection>,
}

impl DavCollection {
    /// "From mail": a calendar no account owns, which shows only invitations no other calendar
    /// of the archive holds.
    pub fn is_from_mail(&self) -> bool {
        self.stored
            .as_ref()
            .is_some_and(|c| c.account_id.is_none() && c.kind == CollectionKind::Calendar)
    }
}

/// One served resource.
#[derive(Debug, Clone)]
pub(crate) struct Res {
    pub href: String,
    pub kind: ItemKind,
    pub etag: String,
    pub bytes: Option<Bytes>,
    /// For a calendar resource: its `Schedule-Tag`, unquoted; the ETag unless a merged reply
    /// pinned another one.
    pub schedule_tag: Option<String>,
}

impl Res {
    pub fn quoted_schedule_tag(&self) -> Option<String> {
        self.schedule_tag.as_ref().map(|t| format!("\"{t}\""))
    }

    pub fn quoted_etag(&self) -> String {
        format!("\"{}\"", self.etag)
    }
}

/// Whether a device may change a collection now.
#[derive(Debug, Clone)]
pub(crate) enum Writable {
    No,
    /// Every lock is open: advertised and accepted.
    Yes(Account),
    /// The collection was made `read_only` less than a day ago: writes are still accepted as
    /// held changes, but not advertised.
    Grace(Account),
}

impl Writable {
    pub fn advertised(&self) -> bool {
        matches!(self, Self::Yes(_))
    }

    pub fn account(&self) -> Option<&Account> {
        match self {
            Self::No => None,
            Self::Yes(a) | Self::Grace(a) => Some(a),
        }
    }
}

/// What a `sync-collection` reports.
pub(crate) struct Synced {
    pub present: Vec<Res>,
    pub removed: Vec<String>,
    pub token: String,
    pub truncated: bool,
}

pub(crate) fn pim_kind(
    collection: CollectionKind,
    item: Option<holtmail_store::ItemKind>,
) -> ItemKind {
    match (collection, item) {
        (CollectionKind::Contacts, _) => ItemKind::Contact,
        (_, Some(kind)) => ItemKind::parse(kind.as_str()).unwrap_or(ItemKind::Other),
        (_, None) => ItemKind::Event,
    }
}

fn schedule_tag(stored: &Collection, effective: &Effective, etag: &str) -> Option<String> {
    if stored.kind != CollectionKind::Calendar {
        return None;
    }
    Some(
        effective
            .item
            .as_ref()
            .and_then(|i| i.item.schedule_tag.clone())
            .unwrap_or_else(|| etag.to_owned()),
    )
}

pub(crate) fn token_of(key: &CollectionKey, value: &str) -> String {
    format!("urn:holtmail:sync:{}:{value}", key.segment())
}

/// The value of a sync token of `key`; `None` for a token of another collection or not ours.
pub(crate) fn token_value<'a>(key: &CollectionKey, token: &'a str) -> Option<&'a str> {
    let rest = token.trim().strip_prefix("urn:holtmail:sync:")?;
    let (segment, value) = rest.rsplit_once(':')?;
    (segment == key.segment()).then_some(value)
}

fn props_key(user: &User, key: &CollectionKey) -> String {
    format!("dav.props.{}.{}", user.id, key.segment())
}

impl Shared {
    /// The calendars or address books the caller sees: "People" first in the address books,
    /// then the stored collections of every archive the device covers, as the store orders
    /// them. Collections removed upstream are not served.
    pub(crate) async fn collections(&self, caller: &Caller, home: Home) -> R<Vec<DavCollection>> {
        let mut out = Vec::new();
        let several = caller.archives.len() > 1;
        if home == Home::AddressBooks {
            for archive in &caller.archives {
                let name = match (several, self.store.get_archive(*archive).await?) {
                    (true, Some(a)) => format!("People ({})", a.name),
                    _ => "People".to_owned(),
                };
                out.push(DavCollection {
                    key: CollectionKey::People(*archive),
                    home,
                    archive: *archive,
                    name,
                    color: None,
                    stored: None,
                });
            }
        }
        let kind = match home {
            Home::Calendars => CollectionKind::Calendar,
            Home::AddressBooks => CollectionKind::Contacts,
        };
        for archive in &caller.archives {
            for summary in self.store.list_collections(*archive).await? {
                let c = summary.collection;
                if c.kind != kind || c.removed_at.is_some() {
                    continue;
                }
                out.push(DavCollection {
                    key: CollectionKey::Stored(c.id),
                    home,
                    archive: c.archive_id,
                    name: c.name.clone(),
                    color: c.color.clone(),
                    stored: Some(c),
                });
            }
        }
        Ok(out)
    }

    /// One collection the caller sees, or `None`.
    pub(crate) async fn collection(
        &self,
        caller: &Caller,
        home: Home,
        key: &CollectionKey,
    ) -> R<Option<DavCollection>> {
        match key {
            CollectionKey::People(archive) => {
                if home != Home::AddressBooks || !caller.archives.contains(archive) {
                    return Ok(None);
                }
                Ok(self
                    .collections(caller, home)
                    .await?
                    .into_iter()
                    .find(|c| c.key == *key))
            }
            CollectionKey::Stored(id) => {
                let Some(c) = self.store.get_collection(*id).await? else {
                    return Ok(None);
                };
                let kind = match home {
                    Home::Calendars => CollectionKind::Calendar,
                    Home::AddressBooks => CollectionKind::Contacts,
                };
                if c.kind != kind
                    || c.removed_at.is_some()
                    || !caller.archives.contains(&c.archive_id)
                {
                    return Ok(None);
                }
                Ok(Some(DavCollection {
                    key: *key,
                    home,
                    archive: c.archive_id,
                    name: c.name.clone(),
                    color: c.color.clone(),
                    stored: Some(c),
                }))
            }
        }
    }

    /// Whether the caller may change the collection: `[serve.dav] edits`, an account behind it,
    /// a write mode other than `read_only` (or `read_only` for less than a day after `held`) and every lock
    /// `holtmail-access` checks for `pim.edit`.
    pub(crate) async fn writable(&self, caller: &Caller, coll: &DavCollection) -> R<Writable> {
        if !self.options.edits {
            return Ok(Writable::No);
        }
        let Some(stored) = &coll.stored else {
            return Ok(Writable::No);
        };
        let Some(account) = stored.account_id else {
            return Ok(Writable::No);
        };
        let account = match self
            .access
            .may_write(&caller.user, &caller.scopes, account, Capability::PimEdit)
            .await
        {
            Ok(account) => account,
            Err(AccessError::Store(error)) => return Err(error.into()),
            Err(_) => return Ok(Writable::No),
        };
        if stored.write_mode != WriteMode::ReadOnly {
            return Ok(Writable::Yes(account));
        }
        if Self::recently_read_only(stored) {
            return Ok(Writable::Grace(account));
        }
        Ok(Writable::No)
    }

    fn recently_read_only(collection: &Collection) -> bool {
        collection.previous_write_mode == Some(WriteMode::Held)
            && collection
                .write_mode_changed_at
                .is_some_and(|at| Timestamp::now().as_millis() - at.as_millis() < GRACE_MS)
    }

    /// What is served for `effective`, or `None` when nothing is (deleted, a held delete, or
    /// bytes that cannot be read). The ETag is BLAKE3 over the served bytes; an unedited
    /// iCalendar or vCard version is served as stored, so its blob id is its ETag and its bytes
    /// are read only when `bytes` asks.
    pub(crate) async fn resolve(
        &self,
        coll: &DavCollection,
        effective: Effective,
        bytes: bool,
    ) -> R<Option<Res>> {
        if !effective.served {
            return Ok(None);
        }
        let Some(stored) = &coll.stored else {
            return Ok(None);
        };
        let kind = pim_kind(stored.kind, effective.item.as_ref().map(|i| i.item.kind));
        let open = effective
            .change
            .as_ref()
            .is_some_and(|c| c.op != ChangeOp::Delete);
        if !open
            && let Some(latest) = &effective.latest
            && matches!(
                Format::stored(&latest.content_type, kind),
                Format::VCard | Format::ICalendar
            )
        {
            let raw = if bytes {
                match self.blobs.get(&latest.blob_id).await? {
                    Some(raw) => Some(raw),
                    None => return Ok(None),
                }
            } else {
                None
            };
            let etag = latest.blob_id.to_string();
            return Ok(Some(Res {
                href: effective.href.clone(),
                kind,
                schedule_tag: schedule_tag(stored, &effective, &etag),
                etag,
                bytes: raw,
            }));
        }
        let Some(served) = self.edits.served(&effective).await? else {
            return Ok(None);
        };
        let etag = BlobId::of(&served.raw).to_string();
        Ok(Some(Res {
            href: effective.href.clone(),
            kind,
            schedule_tag: schedule_tag(stored, &effective, &etag),
            etag,
            bytes: Some(served.raw),
        }))
    }

    /// UIDs held by the archive's calendars other than `except`, for "From mail".
    async fn other_uids(&self, archive: ArchiveId, except: CollectionId) -> R<HashSet<String>> {
        let calendars: HashSet<CollectionId> = self
            .store
            .list_collections(archive)
            .await?
            .into_iter()
            .map(|s| s.collection)
            .filter(|c| {
                c.kind == CollectionKind::Calendar && c.removed_at.is_none() && c.id != except
            })
            .map(|c| c.id)
            .collect();
        let mut uids = HashSet::new();
        for calendar in calendars {
            let mut after = None;
            loop {
                let page = self
                    .store
                    .list_items(&ItemQuery {
                        archive_id: archive,
                        kind: None,
                        collection_id: Some(calendar),
                        removed: RemovedFilter::Hide,
                        order: ItemOrder::Title,
                        after,
                        limit: 500,
                    })
                    .await?;
                uids.extend(page.items.into_iter().filter_map(|i| i.item.key));
                match page.next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
        }
        Ok(uids)
    }

    fn hidden(effective: &Effective, others: Option<&HashSet<String>>) -> bool {
        let Some(others) = others else {
            return false;
        };
        effective
            .item
            .as_ref()
            .and_then(|i| i.item.key.as_ref())
            .is_some_and(|uid| others.contains(uid))
    }

    /// Every resource served in the collection: its live items, items removed upstream that
    /// still have an open change, and held creates, each once by href.
    pub(crate) async fn members(
        &self,
        caller: &Caller,
        coll: &DavCollection,
        bytes: bool,
    ) -> R<Vec<Res>> {
        let Some(stored) = &coll.stored else {
            return self.people(caller, coll).await;
        };
        let others = match coll.is_from_mail() {
            true => Some(self.other_uids(coll.archive, stored.id).await?),
            false => None,
        };
        let mut targets: BTreeMap<String, ChangeTarget> = BTreeMap::new();
        let mut after = None;
        loop {
            let page = self
                .store
                .list_items(&ItemQuery {
                    archive_id: coll.archive,
                    kind: None,
                    collection_id: Some(stored.id),
                    removed: RemovedFilter::Hide,
                    order: ItemOrder::Title,
                    after,
                    limit: 500,
                })
                .await?;
            for summary in page.items {
                targets.insert(
                    summary.item.href().to_owned(),
                    ChangeTarget::Item {
                        item: summary.item.id,
                    },
                );
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        let mut open = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .store
                .open_changes(&ItemChangeQuery {
                    archive_id: coll.archive,
                    collection_id: Some(stored.id),
                    states: Vec::new(),
                    flagged_only: false,
                    after,
                    limit: 500,
                })
                .await?;
            let full = page.len() == 500;
            after = page.last().map(|c| c.id);
            open.extend(page);
            if !full {
                break;
            }
        }
        let mut extra = Vec::new();
        for change in open {
            let target = match (change.item_id, &change.href) {
                (Some(item), _) => ChangeTarget::Item { item },
                (None, Some(href)) => ChangeTarget::Href {
                    collection: stored.id,
                    href: href.clone(),
                },
                (None, None) => continue,
            };
            if !targets.values().any(|t| *t == target) && !extra.contains(&target) {
                extra.push(target);
            }
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for target in targets.into_values().chain(extra) {
            let Some(effective) = self.store.effective(&target).await? else {
                continue;
            };
            if Self::hidden(&effective, others.as_ref()) || !seen.insert(effective.href.clone()) {
                continue;
            }
            if let Some(res) = self.resolve(coll, effective, bytes).await? {
                out.push(res);
            }
        }
        Ok(out)
    }

    /// The resource served under `href`, or `None`.
    pub(crate) async fn resource(
        &self,
        caller: &Caller,
        coll: &DavCollection,
        href: &str,
        bytes: bool,
    ) -> R<Option<Res>> {
        let Some(stored) = &coll.stored else {
            return Ok(self
                .people(caller, coll)
                .await?
                .into_iter()
                .find(|r| r.href == href));
        };
        let Some(effective) = self
            .store
            .effective(&ChangeTarget::Href {
                collection: stored.id,
                href: href.to_owned(),
            })
            .await?
        else {
            return Ok(None);
        };
        if coll.is_from_mail() {
            let others = self.other_uids(coll.archive, stored.id).await?;
            if Self::hidden(&effective, Some(&others)) {
                return Ok(None);
            }
        }
        self.resolve(coll, effective, bytes).await
    }

    /// The generated "People" book of an archive: one vCard 3.0 per person with a card.
    async fn people(&self, _caller: &Caller, coll: &DavCollection) -> R<Vec<Res>> {
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .store
                .list_people(&PersonQuery {
                    archive_id: coll.archive,
                    text: None,
                    source: Some(PersonSource::Contacts),
                    differences: false,
                    after,
                    limit: 500,
                })
                .await?;
            for person in page.items.iter().filter(|p| !p.removed) {
                let raw = Bytes::from(people::vcard(person));
                out.push(Res {
                    href: format!("{}.vcf", person.id),
                    kind: ItemKind::Contact,
                    etag: BlobId::of(&raw).to_string(),
                    bytes: Some(raw),
                    schedule_tag: None,
                });
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok(out)
    }

    /// The collection's current sync token: the archive's newest item change for a stored
    /// collection, a digest of every resource for "People".
    pub(crate) async fn token(&self, caller: &Caller, coll: &DavCollection) -> R<String> {
        match &coll.stored {
            Some(_) => Ok(token_of(
                &coll.key,
                &self.head(coll.archive).await?.to_string(),
            )),
            None => {
                let members = self.people(caller, coll).await?;
                Ok(token_of(&coll.key, &people_digest(&members)))
            }
        }
    }

    pub(crate) async fn head(&self, archive: ArchiveId) -> R<u64> {
        Ok(self
            .store
            .archive_heads(archive, None)
            .await?
            .get(&ChangeKind::Item)
            .copied()
            .unwrap_or(0))
    }

    /// The answer to `sync-collection`: `Ok(None)` when the token is not one of this
    /// collection's, is newer than anything served, or older than the pruned change log.
    pub(crate) async fn sync(
        &self,
        caller: &Caller,
        coll: &DavCollection,
        token: Option<&str>,
        bytes: bool,
    ) -> R<Option<Synced>> {
        let token = token.map(str::trim).filter(|t| !t.is_empty());
        let Some(stored) = &coll.stored else {
            let members = self.people(caller, coll).await?;
            let digest = people_digest(&members);
            let present = match token {
                None => members,
                Some(t) if token_value(&coll.key, t) == Some(digest.as_str()) => Vec::new(),
                Some(_) => return Ok(None),
            };
            return Ok(Some(Synced {
                present,
                removed: Vec::new(),
                token: token_of(&coll.key, &digest),
                truncated: false,
            }));
        };
        let Some(token) = token else {
            let head = self.head(coll.archive).await?;
            let present = self.members(caller, coll, bytes).await?;
            return Ok(Some(Synced {
                present,
                removed: Vec::new(),
                token: token_of(&coll.key, &head.to_string()),
                truncated: false,
            }));
        };
        let Some(since) = token_value(&coll.key, token).and_then(|v| v.parse::<u64>().ok()) else {
            return Ok(None);
        };
        if since < self.store.pruned_through().await? || since > self.store.latest_seq().await? {
            return Ok(None);
        }
        let from_mail = coll.is_from_mail();
        let mut cursor = since;
        let mut targets: Vec<ChangeTarget> = Vec::new();
        let mut purged: HashMap<ItemId, String> = HashMap::new();
        let mut elsewhere: HashSet<ItemId> = HashSet::new();
        let mut truncated = false;
        'read: loop {
            let rows = self
                .store
                .changes_since(coll.archive, Some(cursor), 1000)
                .await?;
            let full = rows.len() == 1000;
            for row in rows {
                if from_mail
                    && row.kind == ChangeKind::Item
                    && row.collection_id != Some(stored.id)
                    && let Some(item) = row.item_id
                {
                    elsewhere.insert(item);
                }
                if row.kind == ChangeKind::Item && row.collection_id == Some(stored.id) {
                    if let (Some(item), Some(href)) = (row.item_id, &row.href) {
                        purged.insert(item, href.clone());
                    }
                    let target = match (row.item_id, row.href) {
                        (Some(item), _) => ChangeTarget::Item { item },
                        (None, Some(href)) => ChangeTarget::Href {
                            collection: stored.id,
                            href,
                        },
                        (None, None) => {
                            cursor = row.seq;
                            continue;
                        }
                    };
                    if !targets.contains(&target) {
                        if targets.len() == MAX_SYNC {
                            truncated = true;
                            break 'read;
                        }
                        targets.push(target);
                    }
                }
                cursor = row.seq;
            }
            if !full {
                break;
            }
        }
        let others = match from_mail {
            true => Some(self.other_uids(coll.archive, stored.id).await?),
            false => None,
        };
        if !elsewhere.is_empty() {
            for item in self.same_uid_items(coll, stored.id, &elsewhere).await? {
                let target = ChangeTarget::Item { item };
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
        let mut seen: HashMap<String, ()> = HashMap::new();
        let mut present = Vec::new();
        let mut removed = Vec::new();
        for target in targets {
            let Some(effective) = self.store.effective(&target).await? else {
                if let ChangeTarget::Item { item } = &target
                    && let Some(href) = purged.get(item)
                    && seen.insert(href.clone(), ()).is_none()
                {
                    removed.push(href.clone());
                }
                continue;
            };
            if seen.insert(effective.href.clone(), ()).is_some() {
                continue;
            }
            let href = effective.href.clone();
            let res = match Self::hidden(&effective, others.as_ref()) {
                true => None,
                false => self.resolve(coll, effective, bytes).await?,
            };
            match res {
                Some(res) => present.push(res),
                None => removed.push(href),
            }
        }
        Ok(Some(Synced {
            present,
            removed,
            token: token_of(&coll.key, &cursor.to_string()),
            truncated,
        }))
    }

    /// Items of "From mail" `from_mail` whose UID one of `changed` (items of other collections
    /// of the archive) holds or held: their visibility may have changed.
    async fn same_uid_items(
        &self,
        coll: &DavCollection,
        from_mail: CollectionId,
        changed: &HashSet<ItemId>,
    ) -> R<Vec<ItemId>> {
        let mut uids = HashSet::new();
        for item in changed {
            if let Some(summary) = self.store.get_item(*item).await?
                && summary.item.collection_id != from_mail
                && let Some(uid) = summary.item.key
            {
                uids.insert(uid);
            }
        }
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .store
                .list_items(&ItemQuery {
                    archive_id: coll.archive,
                    kind: None,
                    collection_id: Some(from_mail),
                    removed: RemovedFilter::Hide,
                    order: ItemOrder::Title,
                    after,
                    limit: 500,
                })
                .await?;
            out.extend(
                page.items
                    .into_iter()
                    .filter(|i| i.item.key.as_ref().is_some_and(|k| uids.contains(k)))
                    .map(|i| i.item.id),
            );
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok(out)
    }

    /// The caller's own display properties of the collection (`PROPPATCH`), by `{ns}name`.
    pub(crate) async fn display_props(
        &self,
        caller: &Caller,
        key: &CollectionKey,
    ) -> R<BTreeMap<String, String>> {
        let raw = self.store.setting(&props_key(&caller.user, key)).await?;
        Ok(raw
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default())
    }

    pub(crate) async fn set_display_props(
        &self,
        caller: &Caller,
        key: &CollectionKey,
        props: &BTreeMap<String, String>,
    ) -> R<()> {
        let value = match props.is_empty() {
            true => None,
            false => serde_json::to_string(props).ok(),
        };
        self.store
            .set_setting(&props_key(&caller.user, key), value.as_deref())
            .await?;
        Ok(())
    }

    /// The component types a calendar holds: `VTODO` when it holds tasks, `VEVENT` when it
    /// holds events or nothing else.
    pub(crate) async fn components(&self, coll: &DavCollection) -> R<Vec<&'static str>> {
        let Some(stored) = &coll.stored else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (kind, name) in [
            (holtmail_store::ItemKind::Event, "VEVENT"),
            (holtmail_store::ItemKind::Task, "VTODO"),
        ] {
            let page = self
                .store
                .list_items(&ItemQuery {
                    archive_id: coll.archive,
                    kind: Some(kind),
                    collection_id: Some(stored.id),
                    removed: RemovedFilter::Include,
                    order: ItemOrder::Title,
                    after: None,
                    limit: 1,
                })
                .await?;
            if !page.items.is_empty() {
                out.push(name);
            }
        }
        if !out.contains(&"VEVENT") && (out.is_empty() || stored.account_id.is_none()) {
            out.insert(0, "VEVENT");
        }
        Ok(out)
    }
}

fn people_digest(members: &[Res]) -> String {
    let mut all = String::new();
    for m in members {
        all.push_str(&m.href);
        all.push(' ');
        all.push_str(&m.etag);
        all.push('\n');
    }
    BlobId::of(all.as_bytes()).to_string()[..16].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_core::CollectionId;

    #[test]
    fn tokens_belong_to_their_collection() {
        let key = CollectionKey::Stored(CollectionId::new());
        let other = CollectionKey::Stored(CollectionId::new());
        let token = token_of(&key, "42");
        assert_eq!(token_value(&key, &token), Some("42"));
        assert_eq!(token_value(&other, &token), None);
        assert_eq!(token_value(&key, "http://example.com/sync/42"), None);
    }
}
