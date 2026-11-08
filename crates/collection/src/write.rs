use async_trait::async_trait;
use bytes::Bytes;

use crate::{CollectionError, RemoteItem};

/// What a writer can do in one collection. The default is nothing, which is also the answer for
/// a collection the writer does not know.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteCaps {
    /// [`CollectionWriter::put`] without an id creates items.
    pub create: bool,
    /// [`CollectionWriter::put`] with an id changes items.
    pub update: bool,
    /// [`CollectionWriter::delete`] removes items.
    pub delete: bool,
    /// [`CollectionWriter::respond`] answers invitations.
    pub respond: bool,
    /// Updates send only [`ItemPatch::fields`] (Microsoft Graph, JMAP), so properties the
    /// client never saw are kept; `false` when the whole item is replaced (CalDAV, CardDAV).
    pub field_patch: bool,
    /// The server keeps version tags, so [`Precondition::IfMatch`] works; without them callers
    /// use [`Precondition::IfUnchanged`].
    pub tags: bool,
    /// The largest [`ItemPatch::raw`] the server takes, when it says.
    pub max_size: Option<u64>,
    /// A create keeps [`ItemPatch::uid`] on the server. `false` when the server gives new items
    /// a `UID` of its own (Microsoft Graph events); the item as written, and as read back, then
    /// carries the server's `UID`.
    pub keeps_uid: bool,
}

/// A part of a contact or event that a change touched, as `pim-edit` sees it. Field-patching
/// writers send exactly these; the others ignore them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Field {
    /// An event's title.
    Summary,
    /// Start, with its time zone.
    Start,
    /// End or duration, with its time zone.
    End,
    /// Recurrence rules, dates and exceptions.
    Recurrence,
    /// Location.
    Location,
    /// Description or notes of an event.
    Description,
    /// Status, such as cancelled.
    Status,
    /// The organizer.
    Organizer,
    /// The attendees with their answers.
    Attendees,
    /// Alarms.
    Alarms,
    /// Categories.
    Categories,
    /// Busy or free.
    Transparency,
    /// Public, private or confidential.
    Privacy,
    /// A contact's names.
    Name,
    /// Email addresses.
    Emails,
    /// Phone numbers.
    Phones,
    /// Postal addresses.
    Addresses,
    /// Organization and department.
    Organization,
    /// Job title and role.
    Title,
    /// A contact's notes.
    Notes,
    /// Birthday and anniversaries.
    Dates,
    /// The photo.
    Photo,
    /// Web addresses.
    Urls,
    /// An iCalendar or vCard property by name, upper case, for anything else.
    Property(String),
}

/// A change to write: the whole item as it should be after the write, plus the parts that
/// differ from the upstream version it was based on. Computed by `pim-edit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemPatch {
    /// The media type of `raw`: [`crate::media::ICALENDAR`] or [`crate::media::VCARD`],
    /// whatever the server speaks; writers for JSON servers convert.
    pub content_type: String,
    /// The whole item after the change. Replace-only writers send these bytes unchanged.
    pub raw: Bytes,
    /// The item's `UID`, which a create keeps on the server when [`WriteCaps::keeps_uid`].
    pub uid: Option<String>,
    /// The parts that differ from the base; `None` means every part. A field-patching writer
    /// sends only these, read from `raw`; a field it cannot set fails with
    /// [`CollectionError::Refused`] before anything changes.
    pub fields: Option<Vec<Field>>,
}

/// The condition a write is made under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// Whatever the server holds: the last writer wins.
    None,
    /// The item must not exist; a create always behaves this way.
    Absent,
    /// The server's version tag must still be this one.
    IfMatch(String),
    /// The item must still be exactly these bytes, as [`crate::CollectionSource::fetch`] gave
    /// them, for servers without version tags.
    IfUnchanged(Bytes),
}

/// An attendee's answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Partstat {
    /// Not answered.
    NeedsAction,
    /// Accepted.
    Accepted,
    /// Tentatively accepted.
    Tentative,
    /// Declined.
    Declined,
}

impl Partstat {
    /// The iCalendar spelling, such as `ACCEPTED`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Accepted => "ACCEPTED",
            Self::Tentative => "TENTATIVE",
            Self::Declined => "DECLINED",
        }
    }

    /// Parses [`Partstat::as_str`], ignoring ASCII case.
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::NeedsAction,
            Self::Accepted,
            Self::Tentative,
            Self::Declined,
        ]
        .into_iter()
        .find(|p| p.as_str().eq_ignore_ascii_case(s))
    }
}

/// What a successful write left on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The item's id in its collection, as [`crate::CollectionSource`] lists it.
    pub id: String,
    /// Its version tag now, when the server gave one.
    pub etag: Option<String>,
    /// The item as the server now holds it, when the answer carried it; otherwise callers fetch
    /// it.
    pub item: Option<RemoteItem>,
}

/// Writes changes to one account's address books and calendars. Separate from
/// [`crate::CollectionSource`], which stays read-only; the server hands one out only for
/// collections in `write_through` mode on accounts allowed to write. `Send + Sync`, usable as
/// `dyn CollectionWriter`.
///
/// A call that fails with [`CollectionError::PreconditionFailed`], [`CollectionError::Refused`],
/// [`CollectionError::Auth`] or [`CollectionError::Gone`] changed nothing. A call that fails
/// otherwise may or may not have reached the server; repeating it is safe, because creates are
/// idempotent by key, updates replace or patch to the same result, and deletes of a missing
/// item succeed.
#[async_trait]
pub trait CollectionWriter: Send + Sync {
    /// What the writer can do in `collection`; nothing for one it knows is read-only or does
    /// not exist. A writer that has not listed its collections yet may answer from the id alone.
    fn capabilities(&self, collection: &str) -> WriteCaps;

    /// Creates an item (`id` is `None`) or changes item `id`. A create is conditional on the
    /// item not existing and idempotent by `idempotency`: the same key gives the same item,
    /// and a repeat after success answers with it instead of creating a second one. An update
    /// holds `precondition`, else fails with `PreconditionFailed` carrying the server's current
    /// tag (`None` when the item is gone or the server keeps no tags). `Refused` when the
    /// server will not take it (read-only, invalid, too large, a field it cannot set); `Gone`
    /// for an unknown collection.
    async fn put(
        &self,
        collection: &str,
        id: Option<&str>,
        change: &ItemPatch,
        precondition: Precondition,
        idempotency: &str,
    ) -> Result<Written, CollectionError>;

    /// Deletes item `id`. One already gone succeeds. With `if_match`, an item whose tag differs
    /// fails with `PreconditionFailed`.
    async fn delete(
        &self,
        collection: &str,
        id: &str,
        if_match: Option<&str>,
    ) -> Result<(), CollectionError>;

    /// Sets the account's own answer to the invitation `id`, for the whole series or the
    /// occurrence `recurrence_id` (as iCalendar writes it, such as `20261201T100000Z`), and
    /// asks the server to tell the organizer when `notify_organizer` is set. Other attendees'
    /// answers and the rest of the event stay. A missing item fails with `PreconditionFailed`
    /// and no tag; `Refused` when the account is not an attendee or the server cannot answer.
    async fn respond(
        &self,
        collection: &str,
        id: &str,
        recurrence_id: Option<&str>,
        answer: Partstat,
        notify_organizer: bool,
    ) -> Result<Written, CollectionError>;
}
