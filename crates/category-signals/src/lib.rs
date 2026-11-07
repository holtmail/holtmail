//! A [`Categorizer`] that reads a message's stored header signals and insight kinds: the kind
//! from the header rules, `notification` from content when the content gate allows it, and the
//! topics the insight kinds imply. The rules are `docs/INSIGHTS.md` §4.2.

#![forbid(unsafe_code)]

use std::sync::Arc;

use holtmail_category::{
    Categorizer, CategoryInput, CategorySnapshot, CategorySource, CategoryVote, KindTag,
    MessageKind, ReservationKind, Signal, Topic, TopicSet,
};

/// The categorizer's id.
pub const ID: &str = "signals";

/// The version of the rules. Bumped whenever any rule changes.
pub const VERSION: &str = "1";

/// The header-signal and insight-kind categorizer (id [`ID`], version [`VERSION`]).
///
/// It always votes: the kind from the header rules, or `notification` from content, and the
/// topics the insight kinds imply (possibly none). Its data is compiled in, so every snapshot is
/// the same.
#[derive(Debug, Clone, Default)]
pub struct SignalsCategorizer {
    snapshot: Arc<SignalsSnapshot>,
}

#[derive(Debug, Default)]
struct SignalsSnapshot;

impl SignalsCategorizer {
    /// The categorizer.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Categorizer for SignalsCategorizer {
    fn id(&self) -> &str {
        ID
    }

    fn snapshot(&self) -> Arc<dyn CategorySnapshot> {
        self.snapshot.clone()
    }
}

impl CategorySnapshot for SignalsSnapshot {
    fn version(&self) -> String {
        VERSION.to_owned()
    }

    fn categorize(&self, input: &CategoryInput) -> Option<CategoryVote> {
        let header = header_kind(input);
        let (kind, kind_source) = if content_notification(input, header) {
            (MessageKind::Notification, CategorySource::Content)
        } else {
            (header, CategorySource::Headers)
        };
        Some(CategoryVote {
            kind: Some(kind),
            kind_source,
            topics: content_topics(input),
            source: CategorySource::Content,
        })
    }
}

/// The kind from the header rules alone, first match wins.
pub fn header_kind(input: &CategoryInput) -> MessageKind {
    let s = input.signals;
    let has = |signal| s.contains(signal);
    if input.own || has(Signal::AutoReplied) {
        MessageKind::Personal
    } else if has(Signal::ListId) && has(Signal::ListPost) {
        MessageKind::List
    } else if has(Signal::Unsubscribe)
        && (has(Signal::PrecedenceBulk)
            || has(Signal::FeedbackId)
            || has(Signal::Esp)
            || (has(Signal::ListId) && !has(Signal::ListPost)))
    {
        MessageKind::Bulk
    } else if has(Signal::AutoSubmitted)
        || has(Signal::Noreply)
        || (has(Signal::Esp) && !has(Signal::Unsubscribe))
    {
        MessageKind::Notification
    } else {
        MessageKind::Personal
    }
}

fn content_notification(input: &CategoryInput, header: MessageKind) -> bool {
    let evidence = input.structured
        || input.kinds.iter().any(|kind| kind != KindTag::Unsubscribe)
        || !input.reservations.is_empty();
    !input.own
        && !input.forwarded
        && evidence
        && (input.structured || matches!(header, MessageKind::Bulk | MessageKind::Notification))
}

/// The topics the insight kinds imply. Applies to forwarded messages too, since `kinds` and
/// `reservations` already leave forwarded items out. Never gated by trust.
pub fn content_topics(input: &CategoryInput) -> TopicSet {
    let mut topics = TopicSet::EMPTY;
    for reservation in input.reservations.iter() {
        match reservation {
            ReservationKind::Flight
            | ReservationKind::Lodging
            | ReservationKind::Train
            | ReservationKind::Bus
            | ReservationKind::RentalCar => topics.insert(Topic::Travel),
            ReservationKind::Event => topics.insert(Topic::Events),
            ReservationKind::Food => {}
        }
    }
    if input.kinds.contains(KindTag::Order) || input.kinds.contains(KindTag::Parcel) {
        topics.insert(Topic::Purchases);
    }
    if input.kinds.contains(KindTag::Invoice) {
        topics.insert(Topic::Finance);
    }
    topics
}

#[cfg(test)]
mod tests;
