use holtmail_category::{KindTags, ReservationKinds, Signals, TrustState};

use super::*;

fn input(signals: &[Signal]) -> CategoryInput {
    CategoryInput {
        signals: signals.iter().copied().collect::<Signals>(),
        kinds: KindTags::EMPTY,
        reservations: ReservationKinds::EMPTY,
        structured: false,
        forwarded: false,
        own: false,
        trust: TrustState::Unknown,
        sender_address: Some("ada@example.com".into()),
        sender_domain: Some("example.com".into()),
    }
}

fn vote(input: &CategoryInput) -> (MessageKind, CategorySource, Vec<Topic>) {
    let vote = SignalsCategorizer::new()
        .snapshot()
        .categorize(input)
        .unwrap();
    assert_eq!(vote.source, CategorySource::Content);
    (
        vote.kind.unwrap(),
        vote.kind_source,
        vote.topics.iter().collect(),
    )
}

fn kind(signals: &[Signal]) -> MessageKind {
    header_kind(&input(signals))
}

use CategorySource::{Content, Headers};
use MessageKind::{Bulk, List, Notification, Personal};

#[test]
fn plain_mail_is_personal() {
    assert_eq!(vote(&input(&[])), (Personal, Headers, vec![]));
}

#[test]
fn list_needs_a_list_post_other_than_no() {
    assert_eq!(kind(&[Signal::ListId, Signal::ListPost]), List);
    assert_eq!(kind(&[Signal::ListId, Signal::ListPostNo]), Personal);
    assert_eq!(
        kind(&[Signal::ListId, Signal::ListPostNo, Signal::Unsubscribe]),
        Bulk
    );
    assert_eq!(kind(&[Signal::ListPost]), Personal);
}

#[test]
fn precedence_alone_is_personal() {
    assert_eq!(kind(&[Signal::PrecedenceBulk]), Personal);
    assert_eq!(kind(&[Signal::FeedbackId]), Personal);
}

#[test]
fn list_unsubscribe_alone_is_personal() {
    assert_eq!(kind(&[Signal::Unsubscribe]), Personal);
    assert_eq!(
        kind(&[Signal::Unsubscribe, Signal::UnsubscribePost]),
        Personal
    );
}

#[test]
fn bulk_needs_unsubscribe_and_one_more() {
    for other in [
        Signal::PrecedenceBulk,
        Signal::FeedbackId,
        Signal::Esp,
        Signal::ListId,
    ] {
        assert_eq!(kind(&[Signal::Unsubscribe, other]), Bulk, "{other}");
    }
}

#[test]
fn auto_replied_is_personal() {
    assert_eq!(
        kind(&[
            Signal::AutoReplied,
            Signal::Unsubscribe,
            Signal::PrecedenceBulk,
            Signal::Noreply
        ]),
        Personal
    );
}

#[test]
fn notification_rules() {
    assert_eq!(kind(&[Signal::AutoSubmitted]), Notification);
    assert_eq!(kind(&[Signal::Noreply]), Notification);
    assert_eq!(kind(&[Signal::Esp]), Notification);
    assert_eq!(kind(&[Signal::Esp, Signal::Unsubscribe]), Bulk);
}

#[test]
fn own_beats_everything() {
    let mut own = input(Signal::ALL);
    own.own = true;
    own.structured = true;
    own.kinds = [KindTag::Order, KindTag::Code].into_iter().collect();
    assert_eq!(vote(&own), (Personal, Headers, vec![Topic::Purchases]));
}

#[test]
fn structured_receipt_through_an_esp_is_notification() {
    let mut receipt = input(&[Signal::Esp, Signal::Unsubscribe, Signal::FeedbackId]);
    receipt.structured = true;
    receipt.kinds = [KindTag::Order, KindTag::Unsubscribe].into_iter().collect();
    assert_eq!(
        vote(&receipt),
        (Notification, Content, vec![Topic::Purchases])
    );
}

#[test]
fn structured_mail_without_list_headers_is_notification() {
    let mut receipt = input(&[]);
    receipt.structured = true;
    receipt.kinds = KindTag::Invoice.into();
    assert_eq!(
        vote(&receipt),
        (Notification, Content, vec![Topic::Finance])
    );
}

#[test]
fn heuristic_items_in_personal_mail_stay_personal() {
    let mut mail = input(&[]);
    mail.kinds = [KindTag::Code, KindTag::Parcel].into_iter().collect();
    assert_eq!(vote(&mail), (Personal, Headers, vec![Topic::Purchases]));
}

#[test]
fn heuristic_items_in_bulk_mail_make_a_notification() {
    let mut mail = input(&[Signal::Unsubscribe, Signal::Esp]);
    mail.kinds = KindTag::Code.into();
    assert_eq!(vote(&mail), (Notification, Content, vec![]));
}

#[test]
fn unsubscribe_item_is_no_content() {
    let mut newsletter = input(&[Signal::Unsubscribe, Signal::PrecedenceBulk]);
    newsletter.kinds = KindTag::Unsubscribe.into();
    assert_eq!(vote(&newsletter), (Bulk, Headers, vec![]));
}

#[test]
fn forwards_keep_their_kind_and_their_topics() {
    let mut forward = input(&[Signal::ForwardMarker, Signal::Noreply]);
    forward.forwarded = true;
    forward.structured = true;
    forward.kinds = KindTag::Reservation.into();
    forward.reservations = ReservationKind::Flight.into();
    assert_eq!(vote(&forward), (Notification, Headers, vec![Topic::Travel]));
}

#[test]
fn reservation_topics() {
    let topics = |kind: ReservationKind| {
        let mut mail = input(&[]);
        mail.kinds = KindTag::Reservation.into();
        mail.reservations = kind.into();
        content_topics(&mail).iter().collect::<Vec<_>>()
    };
    for travel in [
        ReservationKind::Flight,
        ReservationKind::Lodging,
        ReservationKind::Train,
        ReservationKind::Bus,
        ReservationKind::RentalCar,
    ] {
        assert_eq!(topics(travel), [Topic::Travel], "{}", travel.name());
    }
    assert_eq!(topics(ReservationKind::Event), [Topic::Events]);
    assert_eq!(topics(ReservationKind::Food), []);
}

#[test]
fn votes_are_not_gated() {
    let mut invoice = input(&[]);
    invoice.kinds = KindTag::Invoice.into();
    invoice.trust = TrustState::Unverified;
    assert_eq!(vote(&invoice).2, [Topic::Finance]);
}

#[test]
fn id_and_version() {
    let categorizer = SignalsCategorizer::new();
    assert_eq!(categorizer.id(), "signals");
    assert_eq!(categorizer.snapshot().version(), "1");
}
