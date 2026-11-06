use bytes::Bytes;
use holtmail_core::{Address, ArchiveId, CollectionId, ItemId, MessageId, Timestamp};
use holtmail_index::{DocumentKind, ItemDocument};
use holtmail_pim::{Method, Normalized, Participant};
use holtmail_store::{
    CollectionKind, ItemKind, ItemSummary, MailVersionInfo, NewCollection, NewItemVersion,
    NewMailVersion, PimIndex, RecordedVersion, StoreError,
};

use crate::{Ingest, IngestError};

/// One raw contact, event or task as a source or a file gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingItem {
    /// The collection it belongs to.
    pub collection: CollectionId,
    /// The source's id for it, unique in the collection.
    pub remote_id: String,
    /// What it is.
    pub kind: holtmail_pim::ItemKind,
    /// How the bytes are written; stored as their media type.
    pub format: holtmail_pim::Format,
    /// The bytes, stored exactly as given.
    pub raw: Bytes,
    /// The source's version tag, when it has one.
    pub etag: Option<String>,
    /// When it was seen.
    pub seen_at: Timestamp,
}

/// The normalized fields the store lists and matches items by, read from `normalized`, which was
/// read from version `version`.
pub fn pim_index(normalized: &Normalized, version: u32) -> PimIndex {
    let main = match normalized {
        Normalized::Calendar(calendar) => calendar.main(),
        _ => None,
    };
    let start = main.and_then(|e| e.start.as_ref());
    PimIndex {
        version,
        readable: !matches!(normalized, Normalized::Unreadable),
        title: normalized.title(),
        starts_at: start.and_then(|s| s.utc).map(Timestamp::from_millis),
        ends_at: main
            .and_then(|e| e.end.as_ref())
            .and_then(|e| e.utc)
            .map(Timestamp::from_millis),
        all_day: main.is_some_and(|e| e.all_day),
        time_zone: start.and_then(|s| s.time_zone.clone()),
        recurring: main.is_some_and(|e| e.recurring),
        sequence: main.and_then(|e| e.sequence),
        uid: match normalized {
            Normalized::Contact(contact) => contact.uid.clone(),
            _ => main.and_then(|e| e.uid.clone()),
        },
        organizer: main
            .and_then(|e| e.organizer.as_ref())
            .and_then(|o| o.email.clone()),
        emails: normalized.emails(),
        phones: normalized.phones(),
        data: serde_json::to_value(normalized).unwrap_or(serde_json::Value::Null),
    }
}

fn address(p: &Participant) -> Option<Address> {
    if p.email.is_none() && p.name.is_none() {
        return None;
    }
    Some(Address {
        name: p.name.clone(),
        email: p.email.clone().unwrap_or_default(),
    })
}

/// The search document of a stored contact or event, from its normalized fields; `None` for
/// tasks, other items and items that cannot be read.
pub fn item_document(summary: &ItemSummary) -> Option<ItemDocument> {
    let index = summary.index.as_ref()?;
    let normalized: Normalized = serde_json::from_value(index.data.clone()).ok()?;
    let join = |parts: Vec<Option<&String>>| {
        parts
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut document = ItemDocument {
        item_id: summary.item.id,
        archive_id: summary.item.archive_id,
        kind: DocumentKind::Event,
        title: normalized.title().unwrap_or_default(),
        organizer: Vec::new(),
        people: Vec::new(),
        text: String::new(),
        date: index.starts_at,
    };
    match (&normalized, summary.item.kind) {
        (Normalized::Calendar(calendar), ItemKind::Event) => {
            let main = calendar.main()?;
            document.organizer = main.organizer.iter().filter_map(address).collect();
            document.people = main.attendees.iter().filter_map(address).collect();
            let mut text = vec![main.description.as_ref(), main.location.as_ref()];
            for instance in calendar.components.iter().skip(1) {
                text.extend([instance.summary.as_ref(), instance.location.as_ref()]);
            }
            document.text = join(text);
        }
        (Normalized::Contact(contact), ItemKind::Contact) => {
            document.kind = DocumentKind::Contact;
            document.date = None;
            let name = contact.display_name();
            document.people = contact
                .emails
                .iter()
                .map(|e| Address {
                    name: name.clone(),
                    email: e.address.clone(),
                })
                .collect();
            if document.people.is_empty() {
                document.people.push(Address {
                    name: name.clone(),
                    email: String::new(),
                });
            }
            let mut text: Vec<Option<&String>> = vec![
                contact.organization.as_ref(),
                contact.title.as_ref(),
                contact.note.as_ref(),
            ];
            text.extend(contact.nicknames.iter().map(Some));
            text.extend(contact.phones.iter().map(|p| Some(&p.number)));
            text.extend(contact.phones.iter().map(|p| p.e164.as_ref()));
            let addresses: Vec<String> = contact.addresses.iter().map(|a| a.formatted()).collect();
            text.extend(addresses.iter().map(Some));
            document.text = join(text);
        }
        _ => return None,
    }
    Some(document)
}

fn store_kind(kind: holtmail_pim::ItemKind) -> ItemKind {
    ItemKind::parse(kind.as_str()).unwrap_or(ItemKind::Other)
}

impl Ingest {
    /// Archives one raw version of a contact, event or task.
    ///
    /// The bytes are stored unchanged before any metadata refers to them; a picture embedded in a
    /// vCard is stored again as a blob of its own, so the same picture in many cards costs one
    /// blob. A version is added only when the bytes differ from the item's latest; then its
    /// normalized fields replace the item's. Bytes that cannot be read are still archived, as an
    /// unreadable version. Idempotent.
    pub async fn ingest_item(&self, item: IncomingItem) -> Result<RecordedVersion, IngestError> {
        let read = holtmail_pim::read_as(item.format, item.kind, &item.raw);
        let photo_blob_id = match read.photo {
            Some(photo) => Some(self.blobs.put(Bytes::from(photo.bytes)).await?),
            None => None,
        };
        let blob_id = self.blobs.put(item.raw).await?;
        let key = match &read.normalized {
            Normalized::Contact(contact) => contact.uid.clone(),
            Normalized::Calendar(calendar) => calendar.main().and_then(|e| e.uid.clone()),
            Normalized::Unreadable => None,
        };
        let recorded = self
            .store
            .record_version(NewItemVersion {
                collection_id: item.collection,
                remote_id: item.remote_id,
                kind: store_kind(item.kind),
                key,
                blob_id,
                content_type: item.format.content_type().to_owned(),
                etag: item.etag,
                seen_at: item.seen_at,
                photo_blob_id,
                index: pim_index(&read.normalized, 0),
            })
            .await?;
        if recorded.new_version {
            self.derive_item(recorded.item_id).await?;
        }
        Ok(recorded)
    }

    /// Rebuilds what is derived from an item's normalized fields: its part of the calendar
    /// (see [`holtmail_calendar::rebuild`]) and its search document, which becomes searchable
    /// after [`Ingest::commit`].
    pub async fn derive_item(&self, item: ItemId) -> Result<(), IngestError> {
        let summary = self
            .store
            .get_item(item)
            .await?
            .ok_or(StoreError::NotFound("item"))?;
        match holtmail_calendar::series(&summary, Timestamp::now()) {
            Some((series, occurrences)) => {
                self.store
                    .replace_series(item, Some(&series), &occurrences)
                    .await?
            }
            None => self.store.replace_series(item, None, &[]).await?,
        }
        match item_document(&summary) {
            Some(document) => self.index.put_items(vec![document]).await?,
            None => self.index.remove_items(&[item]).await?,
        }
        Ok(())
    }

    /// Rebuilds the normalized fields of an item: from the raw bytes of its latest version, or,
    /// for an item read from mail, from every version merged by [`holtmail_pim::merge`]; then
    /// what is derived from them, as [`Ingest::derive_item`] does.
    pub async fn rebuild_item(&self, item: ItemId) -> Result<(), IngestError> {
        let stored = self
            .store
            .get_item(item)
            .await?
            .ok_or(StoreError::NotFound("item"))?;
        let versions = self.store.item_versions(item).await?;
        let latest = versions
            .last()
            .ok_or(StoreError::NotFound("item version"))?;
        let kind = holtmail_pim::ItemKind::parse(stored.item.kind.as_str())
            .unwrap_or(holtmail_pim::ItemKind::Other);
        let normalized = if versions.iter().any(|v| v.mail.is_some()) {
            let mut read = Vec::with_capacity(versions.len());
            for version in &versions {
                let raw = self
                    .blobs
                    .get(&version.blob_id)
                    .await?
                    .ok_or(StoreError::NotFound("item bytes"))?;
                let method = version.mail.as_ref().and_then(|m| m.method.as_deref());
                let format = holtmail_pim::Format::stored(&version.content_type, kind);
                read.push((
                    Method::of(method),
                    holtmail_pim::normalize_as(format, kind, &raw),
                ));
            }
            let pairs: Vec<_> = read.iter().map(|(m, n)| (*m, n)).collect();
            holtmail_pim::merge(&pairs)
        } else {
            let raw = self
                .blobs
                .get(&latest.blob_id)
                .await?
                .ok_or(StoreError::NotFound("item bytes"))?;
            let format = holtmail_pim::Format::stored(&latest.content_type, kind);
            holtmail_pim::normalize_as(format, kind, &raw)
        };
        self.store
            .set_pim_index(item, &pim_index(&normalized, latest.number))
            .await?;
        self.derive_item(item).await
    }

    /// Reads the calendar parts of a stored message again and records them, as
    /// [`Ingest::ingest`] does for a new message; for mail archived before invitations were
    /// read. Idempotent. Returns how many versions were added.
    pub async fn record_invitations(&self, message: MessageId) -> Result<u32, IngestError> {
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
        self.invitations(
            stored.archive_id,
            message,
            stored.sent_at,
            &parsed.calendars,
        )
        .await
    }

    pub(crate) async fn invitations(
        &self,
        archive: ArchiveId,
        message: MessageId,
        sent_at: Option<Timestamp>,
        parts: &[Bytes],
    ) -> Result<u32, IngestError> {
        let mut collection = None;
        let mut added = 0;
        for part in parts {
            let method = holtmail_pim::calendar_method(part);
            for raw in holtmail_pim::split(part) {
                let remote_id = match (raw.kind, &raw.uid) {
                    (holtmail_pim::ItemKind::Event, Some(uid)) => uid.clone(),
                    (holtmail_pim::ItemKind::Task, Some(uid)) => format!("task:{uid}"),
                    _ => continue,
                };
                let collection = match collection {
                    Some(id) => id,
                    None => {
                        let id = self.mail_collection(archive).await?;
                        collection = Some(id);
                        id
                    }
                };
                let normalized = holtmail_pim::normalize(raw.kind, &raw.raw);
                let main = match &normalized {
                    Normalized::Calendar(calendar) => calendar.main(),
                    _ => None,
                };
                let mail = MailVersionInfo {
                    method: method.clone(),
                    sequence: main.and_then(|e| e.sequence),
                    dtstamp: main.and_then(|e| e.dtstamp).map(Timestamp::from_millis),
                    sent_at,
                };
                let blob_id = self.blobs.put(Bytes::from(raw.raw)).await?;
                let recorded = self
                    .store
                    .record_mail_version(NewMailVersion {
                        version: NewItemVersion {
                            collection_id: collection,
                            remote_id,
                            kind: store_kind(raw.kind),
                            key: raw.uid.clone(),
                            blob_id,
                            content_type: raw.kind.content_type().to_owned(),
                            etag: None,
                            seen_at: Timestamp::now(),
                            photo_blob_id: None,
                            index: pim_index(&normalized, 0),
                        },
                        message_id: message,
                        mail,
                    })
                    .await?;
                if recorded.new_version {
                    added += 1;
                    self.rebuild_item(recorded.item_id).await?;
                }
            }
        }
        Ok(added)
    }

    async fn mail_collection(&self, archive: ArchiveId) -> Result<CollectionId, IngestError> {
        Ok(self
            .store
            .upsert_collection(NewCollection {
                archive_id: archive,
                account_id: None,
                kind: CollectionKind::Calendar,
                remote_id: MAIL_COLLECTION.into(),
                name: "From mail".into(),
                color: None,
            })
            .await?
            .id)
    }
}

pub use holtmail_calendar::MAIL_COLLECTION;
