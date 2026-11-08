use holtmail_blob::BlobId;
use holtmail_core::{AccountId, ArchiveId, CollectionId, ItemId, MessageId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::PageCursor;

/// What a collection holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    /// An address book.
    Contacts,
    /// A calendar, which may also hold tasks.
    Calendar,
}

impl CollectionKind {
    /// The lowercase name used in storage and the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contacts => "contacts",
            Self::Calendar => "calendar",
        }
    }

    /// Parses [`CollectionKind::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contacts" => Self::Contacts,
            "calendar" => Self::Calendar,
            _ => return None,
        })
    }
}

/// An address book or a calendar in an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    /// Id.
    pub id: CollectionId,
    /// The archive it belongs to; access to it follows the archive.
    pub archive_id: ArchiveId,
    /// The account it was read from; `None` once that account was removed.
    pub account_id: Option<AccountId>,
    /// Address book or calendar.
    pub kind: CollectionKind,
    /// The source's id for it: a DAV path, a provider id, or the path of an imported file.
    pub remote_id: String,
    /// Name shown to people.
    pub name: String,
    /// Color the source gave it, as written.
    pub color: Option<String>,
    /// The source's sync token, opaque to the store.
    pub sync_token: Option<String>,
    /// The source's tag for the collection's state as of the last sync (a CardDAV or CalDAV
    /// `getctag`), opaque to the store; unchanged means nothing in it changed.
    pub ctag: Option<String>,
    /// When the last sync of it from its source ended.
    pub synced_at: Option<Timestamp>,
    /// When it was first stored.
    pub created_at: Timestamp,
    /// When it was noticed gone from the source; it stays readable.
    pub removed_at: Option<Timestamp>,
    /// Whether devices and the web app may change it, and whether changes go upstream.
    pub write_mode: crate::WriteMode,
}

/// Fields of a collection to create or update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCollection {
    /// The archive.
    pub archive_id: ArchiveId,
    /// The account it is read from.
    pub account_id: Option<AccountId>,
    /// Address book or calendar.
    pub kind: CollectionKind,
    /// The source's id for it.
    pub remote_id: String,
    /// Name shown to people.
    pub name: String,
    /// Color, as written.
    pub color: Option<String>,
}

/// A collection with counts of its items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSummary {
    /// The collection.
    pub collection: Collection,
    /// Items not marked removed.
    pub items: u64,
    /// Items marked removed.
    pub removed_items: u64,
}

/// What an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A contact.
    Contact,
    /// A calendar event with its changed instances.
    Event,
    /// A task.
    Task,
    /// Anything else a calendar holds, such as a journal entry.
    Other,
}

impl ItemKind {
    /// The lowercase name used in storage and the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contact => "contact",
            Self::Event => "event",
            Self::Task => "task",
            Self::Other => "other",
        }
    }

    /// Parses [`ItemKind::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contact" => Self::Contact,
            "event" => Self::Event,
            "task" => Self::Task,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// A contact, event or task in a collection, known by the source's id for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Id.
    pub id: ItemId,
    /// The collection.
    pub collection_id: CollectionId,
    /// The collection's archive.
    pub archive_id: ArchiveId,
    /// The source's id for the item, unique in its collection.
    pub remote_id: String,
    /// What it is, as of its latest version.
    pub kind: ItemKind,
    /// The stable key across collections, such as an iCalendar `UID`, as of its latest version.
    pub key: Option<String>,
    /// When it was first seen.
    pub first_seen_at: Timestamp,
    /// When it was noticed gone from the source; it stays readable.
    pub removed_at: Option<Timestamp>,
    /// How many versions are stored; the latest has this number.
    pub versions: u32,
    /// The href a client created it under, kept for good once the upstream id is known;
    /// `None` for items that came from upstream.
    pub dav_name: Option<String>,
}

impl Item {
    /// The name it is served under: its DAV name, else its source id.
    pub fn href(&self) -> &str {
        self.dav_name.as_deref().unwrap_or(&self.remote_id)
    }
}

/// One stored version of an item: raw bytes exactly as the source gave them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemVersion {
    /// The item.
    pub item_id: ItemId,
    /// 1 for the first version, counting up.
    pub number: u32,
    /// The raw bytes.
    pub blob_id: BlobId,
    /// Their media type, such as `text/vcard`.
    pub content_type: String,
    /// The source's version tag (ETag, change key), when it has one.
    pub etag: Option<String>,
    /// When this version was first seen.
    pub seen_at: Timestamp,
    /// A picture embedded in the raw bytes, kept as a blob of its own.
    pub photo_blob_id: Option<BlobId>,
    /// For a version read from mail: its iTIP method and the fields it is ordered by.
    pub mail: Option<MailVersionInfo>,
    /// Whether a source showed it or a writeback recorded it.
    pub origin: crate::VersionOrigin,
    /// For a writeback version: the change that was sent.
    pub change_id: Option<holtmail_core::ItemChangeId>,
}

/// What a calendar object found in mail says about itself, and the fields that order it among
/// the other versions of its item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailVersionInfo {
    /// The iTIP `METHOD`, uppercase, such as `REQUEST` or `REPLY`; `None` when the object has
    /// none.
    pub method: Option<String>,
    /// iCalendar `SEQUENCE` of the object's main component.
    pub sequence: Option<i64>,
    /// iCalendar `DTSTAMP` of the object's main component.
    pub dtstamp: Option<Timestamp>,
    /// The `Date` of the message it came in.
    pub sent_at: Option<Timestamp>,
}

/// A version found in a message, to record with [`crate::PimStore::record_mail_version`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewMailVersion {
    /// The version itself.
    pub version: NewItemVersion,
    /// The message it was found in.
    pub message_id: MessageId,
    /// Its method and order.
    pub mail: MailVersionInfo,
}

/// A message an item version was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VersionSource {
    /// The item.
    pub item_id: ItemId,
    /// The version's current number.
    pub number: u32,
    /// The message.
    pub message_id: MessageId,
}

/// Normalized fields of an item's latest version, for listing and matching. Derived: rebuilt
/// from the raw versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PimIndex {
    /// The version these fields were read from.
    pub version: u32,
    /// Whether the version could be read at all.
    pub readable: bool,
    /// A contact's name or an event's summary.
    pub title: Option<String>,
    /// Start in UTC.
    pub starts_at: Option<Timestamp>,
    /// End in UTC.
    pub ends_at: Option<Timestamp>,
    /// Whether the start is a date without a time.
    pub all_day: bool,
    /// The start's original time zone.
    pub time_zone: Option<String>,
    /// Whether the event repeats.
    pub recurring: bool,
    /// iCalendar `SEQUENCE`.
    pub sequence: Option<i64>,
    /// iCalendar `UID`, or a contact's `UID`.
    pub uid: Option<String>,
    /// The organizer's address.
    pub organizer: Option<String>,
    /// Normalized email addresses: a contact's, or an event's organizer and attendees.
    pub emails: Vec<String>,
    /// A contact's phone numbers in E.164 form.
    pub phones: Vec<String>,
    /// The whole normalized view, as its owner module serializes it.
    pub data: serde_json::Value,
}

/// A version to record for an item.
#[derive(Debug, Clone, PartialEq)]
pub struct NewItemVersion {
    /// The collection.
    pub collection_id: CollectionId,
    /// The source's id for the item.
    pub remote_id: String,
    /// What it is.
    pub kind: ItemKind,
    /// Its stable key.
    pub key: Option<String>,
    /// The raw bytes, already in the blob store.
    pub blob_id: BlobId,
    /// Their media type.
    pub content_type: String,
    /// The source's version tag.
    pub etag: Option<String>,
    /// When it was seen.
    pub seen_at: Timestamp,
    /// A picture split off the raw bytes, already in the blob store.
    pub photo_blob_id: Option<BlobId>,
    /// Normalized fields of this version.
    pub index: PimIndex,
}

/// What [`crate::PimStore::record_version`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedVersion {
    /// The item.
    pub item_id: ItemId,
    /// The item's latest version number after the call.
    pub number: u32,
    /// Whether the item was new.
    pub created_item: bool,
    /// Whether a version was added; `false` when the bytes equal the latest version's.
    pub new_version: bool,
    /// Whether the item had been marked removed and is back.
    pub restored: bool,
}

/// Which items a list holds by their removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemovedFilter {
    /// Only items not marked removed.
    #[default]
    Hide,
    /// Every item.
    Include,
    /// Only items marked removed.
    Only,
}

/// How a list of items is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemOrder {
    /// By title without regard to letter case, items without a title last, then by id.
    #[default]
    Title,
    /// By start, items without a start last, then by id.
    Start,
}

/// Which items to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemQuery {
    /// The archive.
    pub archive_id: ArchiveId,
    /// Only items of this kind.
    pub kind: Option<ItemKind>,
    /// Only items of this collection.
    pub collection_id: Option<CollectionId>,
    /// Removed items or not.
    pub removed: RemovedFilter,
    /// The order.
    pub order: ItemOrder,
    /// Continue after this point of a previous page.
    pub after: Option<PageCursor>,
    /// At most this many items; implementations cap it at 500.
    pub limit: u32,
}

/// An item with the normalized fields of its latest version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemSummary {
    /// The item.
    pub item: Item,
    /// Its normalized fields; `None` until they are built.
    pub index: Option<PimIndex>,
}
