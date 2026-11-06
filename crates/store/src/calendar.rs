use holtmail_core::{ArchiveId, CollectionId, EventGroupId, ItemId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::ItemSummary;

/// What the calendar knows about one event item: its group of copies, what ranks it among
/// them, and when its instances are. Derived from the item's normalized fields and rebuilt with
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSeries {
    /// The item.
    pub item_id: ItemId,
    /// The copies of the same event it belongs to.
    pub group_id: EventGroupId,
    /// iCalendar `SEQUENCE` of the event, the first thing copies are ranked by.
    pub sequence: Option<i64>,
    /// The later of `DTSTAMP` and `LAST-MODIFIED`, the second thing copies are ranked by.
    pub stamp: Option<Timestamp>,
    /// The earliest start of its instances.
    pub starts_at: Option<Timestamp>,
    /// The latest end of its instances; `None` with `starts_at` set means it repeats without end.
    pub ends_at: Option<Timestamp>,
    /// Whether it repeats.
    pub recurring: bool,
    /// The span its stored instances cover; `None` when every instance is stored.
    pub window: Option<(Timestamp, Timestamp)>,
}

/// One stored instance of an event item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewOccurrence {
    /// The original start naming the instance among the copies, such as `2025-03-10T08:00:00Z`
    /// or `2025-03-10`; `None` for an event that does not repeat.
    pub recurrence_id: Option<String>,
    /// Start in UTC.
    pub starts_at: Timestamp,
    /// End in UTC, never before the start.
    pub ends_at: Timestamp,
    /// The index of the component the instance takes its fields from.
    pub component: u32,
    /// Start as a wall-clock time in its own zone.
    pub start_local: String,
    /// End as a wall-clock time in its own zone.
    pub end_local: String,
    /// The start's `TZID` as written.
    pub time_zone: Option<String>,
    /// Whether it lasts whole days.
    pub all_day: bool,
    /// Whether its times belong to no zone.
    pub floating: bool,
    /// Whether it is cancelled.
    pub cancelled: bool,
    /// Whether a changed instance replaced it.
    pub changed: bool,
}

/// A stored instance with the item it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    /// The item.
    pub item_id: ItemId,
    /// The item's collection.
    pub collection_id: CollectionId,
    /// The item's group of copies.
    pub group_id: EventGroupId,
    /// Whether the item, or its collection, was removed from its source.
    pub removed: bool,
    /// The item's [`EventSeries::sequence`].
    pub sequence: Option<i64>,
    /// The item's [`EventSeries::stamp`].
    pub stamp: Option<Timestamp>,
    /// The instance.
    pub occurrence: NewOccurrence,
}

/// A repeating item whose stored instances do not cover a span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UncoveredSeries {
    /// The item with its normalized fields.
    pub summary: ItemSummary,
    /// Its calendar facts.
    pub series: EventSeries,
    /// Whether the item, or its collection, was removed from its source.
    pub removed: bool,
}

/// Which instances to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceQuery {
    /// The archive.
    pub archive_id: ArchiveId,
    /// Instances ending after this time (or starting at it, when they last no time)…
    pub from: Timestamp,
    /// …and starting before this one.
    pub to: Timestamp,
    /// Only items of these collections; `None` for every collection.
    pub collections: Option<Vec<CollectionId>>,
    /// Whether items and collections removed from their source are included.
    pub removed: bool,
}

/// An item of a group of copies with its calendar facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMember {
    /// The item with its normalized fields.
    pub summary: ItemSummary,
    /// Its calendar facts.
    pub series: EventSeries,
    /// When its collection was removed from its source.
    pub collection_removed_at: Option<Timestamp>,
}
