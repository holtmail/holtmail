//! The shared categorizer conformance suite.

use holtmail_category::testing::conformance;
use holtmail_category::{
    CategoryInput, CategorySource, CategoryVote, KindTag, KindTags, MessageKind, ReservationKind,
    ReservationKinds, Signal, Signals, Topic, TopicSet, TrustState,
};
use holtmail_category_signals::SignalsCategorizer;

fn input(signals: &[Signal], kinds: &[KindTag], reservations: &[ReservationKind]) -> CategoryInput {
    CategoryInput {
        signals: signals.iter().copied().collect::<Signals>(),
        kinds: kinds.iter().copied().collect::<KindTags>(),
        reservations: reservations.iter().copied().collect::<ReservationKinds>(),
        structured: false,
        forwarded: false,
        own: false,
        trust: TrustState::Unknown,
        sender_address: Some("news@news.example.com".into()),
        sender_domain: Some("example.com".into()),
    }
}

fn vote(kind: MessageKind, kind_source: CategorySource, topics: &[Topic]) -> Option<CategoryVote> {
    Some(CategoryVote {
        kind: Some(kind),
        kind_source,
        topics: topics.iter().copied().collect::<TopicSet>(),
        source: CategorySource::Content,
    })
}

#[test]
fn keeps_the_category_contract() {
    let mut structured = input(
        &[Signal::Esp, Signal::Structured],
        &[KindTag::Reservation],
        &[ReservationKind::Event],
    );
    structured.structured = true;
    let mut own = input(&[Signal::Noreply], &[KindTag::Order], &[]);
    own.own = true;
    let cases = vec![
        (
            input(&[], &[], &[]),
            vote(MessageKind::Personal, CategorySource::Headers, &[]),
        ),
        (
            input(&[Signal::ListId, Signal::ListPost], &[], &[]),
            vote(MessageKind::List, CategorySource::Headers, &[]),
        ),
        (
            input(
                &[Signal::Unsubscribe, Signal::FeedbackId],
                &[KindTag::Unsubscribe],
                &[],
            ),
            vote(MessageKind::Bulk, CategorySource::Headers, &[]),
        ),
        (
            structured,
            vote(
                MessageKind::Notification,
                CategorySource::Content,
                &[Topic::Events],
            ),
        ),
        (
            own,
            vote(
                MessageKind::Personal,
                CategorySource::Headers,
                &[Topic::Purchases],
            ),
        ),
    ];
    conformance::categorizer(&SignalsCategorizer::new(), &cases);
}
