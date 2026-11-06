//! The derived calendar of an archive: which event items are copies of one event, the stored
//! instances of each item, the instances of a span with copies folded together, and how copies
//! differ.
//!
//! Events in several calendars, and the events found in mail, are copies of one event when they
//! share an iCalendar `UID`; they form a group whose id is computed from the archive and the
//! `UID`. Among copies the leading one has the highest `SEQUENCE`, then the latest `DTSTAMP` or
//! `LAST-MODIFIED`. Everything here is derived from the items' normalized fields: [`rebuild`]
//! replaces what one item contributes and gives the same result for the same data.
//!
//! A library, not a contract: there is one calendar. Nothing here logs.

mod copies;

use std::collections::HashMap;

use holtmail_core::{ArchiveId, EventGroupId, ItemId, Timestamp};
use holtmail_pim::{CalendarObject, Normalized};
use holtmail_recur::Window;
use holtmail_store::{
    EventSeries, ItemKind, ItemSummary, NewOccurrence, Occurrence, OccurrenceQuery, Store,
    StoreError,
};

pub use copies::{AttendeeAnswers, CopyDifference, answers, differences};

/// The `remote_id` of each archive's calendar of events found in mail: one per archive, with no
/// account, named "From mail".
pub const MAIL_COLLECTION: &str = "holtmail:from-mail";

/// How far before and after the time of a rebuild the instances of an endless or long series are
/// stored: two years each way. Spans outside are worked out when asked for.
pub const WINDOW_MS: i64 = 2 * 366 * 86_400_000;

/// A finite series with at most this many instances is stored whole.
pub const STORED_WHOLE: usize = 1_000;

/// The group of copies of an event in `archive`: from its `UID`, or, for an event without one,
/// from the item alone. A UUIDv8 from the BLAKE3 hash of the archive id and the key.
pub fn group_id(archive: ArchiveId, uid: Option<&str>, item: ItemId) -> EventGroupId {
    let key = match uid.map(str::trim).filter(|u| !u.is_empty()) {
        Some(uid) => format!("uid:{uid}"),
        None => format!("item:{item}"),
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(archive.to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    EventGroupId::from_hash(bytes)
}

/// The group of a stored item, when it is an event: from the `UID` of its normalized fields.
pub fn group_of(summary: &ItemSummary) -> Option<EventGroupId> {
    if summary.item.kind != ItemKind::Event {
        return None;
    }
    let uid = summary
        .index
        .as_ref()
        .and_then(|i| i.uid.clone())
        .or_else(|| summary.item.key.clone());
    Some(group_id(
        summary.item.archive_id,
        uid.as_deref(),
        summary.item.id,
    ))
}

/// The calendar object of a stored item's normalized fields, when it has one.
pub fn object(summary: &ItemSummary) -> Option<CalendarObject> {
    let data = summary.index.as_ref()?.data.clone();
    match serde_json::from_value::<Normalized>(data).ok()? {
        Normalized::Calendar(calendar) => Some(calendar),
        _ => None,
    }
}

fn stored(o: &holtmail_recur::Occurrence) -> NewOccurrence {
    NewOccurrence {
        recurrence_id: o.recurrence_id.clone(),
        starts_at: Timestamp::from_millis(o.start),
        ends_at: Timestamp::from_millis(o.end),
        component: u32::try_from(o.component).unwrap_or(u32::MAX),
        start_local: o.start_local.clone(),
        end_local: o.end_local.clone(),
        time_zone: o.time_zone.clone(),
        all_day: o.all_day,
        floating: o.floating,
        cancelled: o.cancelled,
        changed: o.changed,
    }
}

/// What a stored item contributes to the calendar, as of `now`: its series facts and the
/// instances to store. `None` for an item that is no event or whose fields cannot be read.
///
/// An event that does not repeat, or repeats at most [`STORED_WHOLE`] times, has every instance
/// stored; a longer or endless one has the instances within [`WINDOW_MS`] of `now`.
pub fn series(summary: &ItemSummary, now: Timestamp) -> Option<(EventSeries, Vec<NewOccurrence>)> {
    let group_id = group_of(summary)?;
    let object = object(summary)?;
    let main = object.main()?;
    let recurring = object
        .components
        .iter()
        .any(|e| e.recurrence_id.is_none() && (e.rrule.is_some() || !e.rdates.is_empty()));
    let span = holtmail_recur::span(&object);
    let stamp = [main.dtstamp, main.last_modified]
        .into_iter()
        .flatten()
        .max();
    let whole = (!recurring || span.is_some_and(|s| s.end.is_some()))
        .then(|| holtmail_recur::expand(&object, Window::ALL))
        .filter(|e| !e.truncated && e.occurrences.len() <= STORED_WHOLE);
    let (window, expansion) = match whole {
        Some(all) => (None, all),
        None => {
            let window = Window {
                from: now.as_millis() - WINDOW_MS,
                to: now.as_millis() + WINDOW_MS,
            };
            (
                Some((
                    Timestamp::from_millis(window.from),
                    Timestamp::from_millis(window.to),
                )),
                holtmail_recur::expand(&object, window),
            )
        }
    };
    Some((
        EventSeries {
            item_id: summary.item.id,
            group_id,
            sequence: main.sequence,
            stamp: stamp.map(Timestamp::from_millis),
            starts_at: span.map(|s| Timestamp::from_millis(s.start)),
            ends_at: span.and_then(|s| s.end).map(Timestamp::from_millis),
            recurring,
            window,
        },
        expansion.occurrences.iter().map(stored).collect(),
    ))
}

/// Replaces what the item contributes to the calendar with what its current normalized fields
/// give, as of `now`. Idempotent for the same fields and `now`. `NotFound` for an unknown item.
pub async fn rebuild(store: &dyn Store, item: ItemId, now: Timestamp) -> Result<(), StoreError> {
    let summary = store
        .get_item(item)
        .await?
        .ok_or(StoreError::NotFound("item"))?;
    match series(&summary, now) {
        Some((series, occurrences)) => {
            store
                .replace_series(item, Some(&series), &occurrences)
                .await
        }
        None => store.replace_series(item, None, &[]).await,
    }
}

/// Every instance of the archive's events in the query's span: the stored ones, and for items
/// whose stored instances do not cover the span, the ones worked out from their fields. By
/// start, then item id, then `recurrence_id`.
pub async fn instances(
    store: &dyn Store,
    query: &OccurrenceQuery,
) -> Result<Vec<Occurrence>, StoreError> {
    let mut out = store.occurrences(query).await?;
    let window = Window {
        from: query.from.as_millis(),
        to: query.to.as_millis(),
    };
    for uncovered in store.uncovered_series(query).await? {
        let Some(object) = object(&uncovered.summary) else {
            continue;
        };
        for o in holtmail_recur::expand(&object, window).occurrences {
            out.push(Occurrence {
                item_id: uncovered.summary.item.id,
                collection_id: uncovered.summary.item.collection_id,
                group_id: uncovered.series.group_id,
                removed: uncovered.removed,
                sequence: uncovered.series.sequence,
                stamp: uncovered.series.stamp,
                occurrence: stored(&o),
            });
        }
    }
    out.sort_by(|a, b| {
        (
            a.occurrence.starts_at,
            a.item_id,
            &a.occurrence.recurrence_id,
        )
            .cmp(&(
                b.occurrence.starts_at,
                b.item_id,
                &b.occurrence.recurrence_id,
            ))
    });
    Ok(out)
}

/// How copies are ranked: highest `SEQUENCE` first, then the latest stamp, then copies still on
/// their source before removed ones, then by item id. Missing values rank last.
pub fn leads(
    a: (Option<i64>, Option<Timestamp>, bool, ItemId),
    b: (Option<i64>, Option<Timestamp>, bool, ItemId),
) -> std::cmp::Ordering {
    b.0.cmp(&a.0)
        .then(b.1.cmp(&a.1))
        .then(a.2.cmp(&b.2))
        .then(a.3.cmp(&b.3))
}

fn rank(o: &Occurrence) -> (Option<i64>, Option<Timestamp>, bool, ItemId) {
    (o.sequence, o.stamp, o.removed, o.item_id)
}

/// One instance of an event with all its copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folded {
    /// The copies of the instance, the leading one first (see [`leads`]).
    pub copies: Vec<Occurrence>,
}

impl Folded {
    /// The leading copy.
    pub fn leading(&self) -> &Occurrence {
        &self.copies[0]
    }
}

/// Folds the copies of each instance together: instances of the same group with the same
/// `recurrence_id` are one, shown as their leading copy. By the leading copy's start, then group,
/// then `recurrence_id`.
pub fn fold(instances: Vec<Occurrence>) -> Vec<Folded> {
    let mut groups: HashMap<(EventGroupId, Option<String>), Vec<Occurrence>> = HashMap::new();
    for instance in instances {
        groups
            .entry((instance.group_id, instance.occurrence.recurrence_id.clone()))
            .or_default()
            .push(instance);
    }
    let mut out: Vec<Folded> = groups
        .into_values()
        .map(|mut copies| {
            copies.sort_by(|a, b| leads(rank(a), rank(b)));
            copies.dedup_by(|a, b| a.item_id == b.item_id);
            Folded { copies }
        })
        .collect();
    out.sort_by(|a, b| {
        let (a, b) = (a.leading(), b.leading());
        (
            a.occurrence.starts_at,
            a.group_id,
            &a.occurrence.recurrence_id,
        )
            .cmp(&(
                b.occurrence.starts_at,
                b.group_id,
                &b.occurrence.recurrence_id,
            ))
    });
    out
}

#[cfg(test)]
mod tests;
