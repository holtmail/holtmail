use async_trait::async_trait;
use holtmail_core::{ArchiveId, ItemId, MessageId, Timestamp, UserId};

use crate::StoreError;

/// What became of an incoming iTIP message, as the schedule inbox shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InboxNote {
    /// A `REQUEST` or `CANCEL` the "From mail" copy follows.
    Update,
    /// A `REQUEST` or `CANCEL` whose `UID` a served upstream calendar already holds.
    UpstreamHolds,
    /// A `REQUEST` or `CANCEL` organized by one of the user's own addresses.
    OwnOrganizer,
    /// A `REPLY` merged into the organizer's copy.
    Applied,
    /// A `REPLY` that does not name exactly one attendee who sent it.
    NotFromAttendee,
    /// A `REPLY` without DKIM or DMARC pass for its sender; "Apply anyway" can still merge it.
    Unauthenticated,
    /// A `REPLY` to an event the user organizes no copy of.
    NoCopy,
    /// A `REPLY` the copy already holds, or one older than the copy.
    Unchanged,
}

impl InboxNote {
    /// Every value, in a fixed order.
    pub const ALL: &'static [InboxNote] = &[
        Self::Update,
        Self::UpstreamHolds,
        Self::OwnOrganizer,
        Self::Applied,
        Self::NotFromAttendee,
        Self::Unauthenticated,
        Self::NoCopy,
        Self::Unchanged,
    ];

    /// The lowercase name used in storage and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::UpstreamHolds => "upstream_holds",
            Self::OwnOrganizer => "own_organizer",
            Self::Applied => "applied",
            Self::NotFromAttendee => "not_from_attendee",
            Self::Unauthenticated => "unauthenticated",
            Self::NoCopy => "no_copy",
            Self::Unchanged => "unchanged",
        }
    }

    /// The value named by `as_str`, or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == name)
    }
}

/// One incoming iTIP message addressed to an own address: a resource of the CalDAV schedule
/// inbox. The archived message stays the truth; the entry only names its calendar part and
/// what scheduling made of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleInboxEntry {
    /// The archive the message is in.
    pub archive_id: ArchiveId,
    /// The message.
    pub message_id: MessageId,
    /// Which calendar part of the message, counting from 0.
    pub part: u32,
    /// The iTIP method, uppercase (`REQUEST`, `REPLY`, `CANCEL`).
    pub method: String,
    /// The event's `UID`.
    pub uid: String,
    /// What scheduling made of it.
    pub note: InboxNote,
    /// When it was taken in.
    pub received_at: Timestamp,
}

/// Most entries [`ScheduleStore::inbox_entries`] returns.
pub const MAX_INBOX: u32 = 1_000;

/// The scheduling state CalDAV serves next to the archive: pinned `Schedule-Tag`s and the
/// schedule inbox (SERVE.md, calendar-auto-schedule). Both are derived from archived mail and
/// recorded changes, and losing them costs a client one refetch, never mail.
#[async_trait]
pub trait ScheduleStore: Send + Sync {
    /// Pins or clears the item's `Schedule-Tag`. `NotFound` for an unknown item.
    async fn set_schedule_tag(&self, item: ItemId, tag: Option<&str>) -> Result<(), StoreError>;

    /// Records an inbox entry. Idempotent per message and part: recording the same part again
    /// replaces its method, `UID` and note and keeps `received_at` and who hid it.
    async fn put_inbox_entry(&self, entry: &ScheduleInboxEntry) -> Result<(), StoreError>;

    /// The archive's entries `user` has not hidden, newest first, at most [`MAX_INBOX`]. Entries
    /// of a message deleted from the archive are gone with it.
    async fn inbox_entries(
        &self,
        archive: ArchiveId,
        user: UserId,
    ) -> Result<Vec<ScheduleInboxEntry>, StoreError>;

    /// The entries of one message, by part; hidden ones too.
    async fn inbox_entries_of(
        &self,
        message: MessageId,
    ) -> Result<Vec<ScheduleInboxEntry>, StoreError>;

    /// Hides an entry from `user`'s schedule inbox; the message stays archived. `false` when
    /// there is no such entry; hiding twice is harmless.
    async fn hide_inbox_entry(
        &self,
        user: UserId,
        message: MessageId,
        part: u32,
    ) -> Result<bool, StoreError>;
}
