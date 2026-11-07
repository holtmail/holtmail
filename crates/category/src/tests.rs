use super::*;

#[test]
fn persisted_bits_never_move() {
    let topics: Vec<_> = Topic::ALL.iter().map(|t| (t.bit(), t.name())).collect();
    assert_eq!(
        topics,
        [
            (0, "purchases"),
            (1, "travel"),
            (2, "finance"),
            (3, "health"),
            (4, "social"),
            (5, "jobs"),
            (6, "events"),
            (7, "government"),
        ]
    );
    for topic in Topic::ALL {
        assert_eq!(Topic::from_bit(topic.bit()), Some(*topic));
        assert_eq!(Topic::from_name(topic.name()), Some(*topic));
    }
    assert_eq!(Topic::from_bit(8), None);
    assert_eq!(Topic::from_name("Travel"), None);
}

#[test]
fn names_are_stable() {
    let kinds: Vec<_> = MessageKind::ALL.iter().map(|k| k.name()).collect();
    assert_eq!(kinds, ["personal", "list", "bulk", "notification"]);
    for kind in MessageKind::ALL {
        assert_eq!(MessageKind::from_name(kind.name()), Some(*kind));
        assert_eq!(
            serde_json::to_string(kind).unwrap(),
            format!("\"{}\"", kind.name())
        );
    }
    assert_eq!(MessageKind::from_name("other"), None);

    let sources: Vec<_> = CategorySource::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(sources, ["override", "content", "domain", "headers"]);
    for source in CategorySource::ALL {
        assert_eq!(CategorySource::from_name(source.name()), Some(*source));
    }

    let trust = [
        TrustState::Aligned {
            domain: "example.com".into(),
        },
        TrustState::Relayed {
            provider: "relay.example.org".into(),
        },
        TrustState::Unverified,
        TrustState::Unknown,
    ];
    let names: Vec<_> = trust.iter().map(TrustState::name).collect();
    assert_eq!(names, ["aligned", "relayed", "unverified", "unknown"]);
    let aligned: Vec<_> = trust.iter().map(TrustState::is_aligned).collect();
    assert_eq!(aligned, [true, false, false, false]);

    let reservations: Vec<_> = ReservationKind::ALL.iter().map(|r| r.name()).collect();
    assert_eq!(
        reservations,
        [
            "flight",
            "lodging",
            "train",
            "bus",
            "event",
            "food",
            "rental_car"
        ]
    );
    assert_eq!(TAXONOMY_VERSION, "1");
}

#[test]
fn rank_follows_the_precedence_table() {
    let ranks: Vec<_> = CategorySource::ALL.iter().map(|s| s.rank()).collect();
    assert!(ranks.windows(2).all(|w| w[0] > w[1]), "{ranks:?}");
}

#[test]
fn sensitive_topics() {
    let sensitive: Vec<_> = Topic::ALL
        .iter()
        .filter(|t| t.is_sensitive())
        .map(|t| t.name())
        .collect();
    assert_eq!(sensitive, ["finance", "health", "government"]);
}

#[test]
fn topic_sets_keep_unknown_bits() {
    let stored = TopicSet::from_bits((1 << 40) | 0b10);
    assert!(stored.contains(Topic::Travel));
    assert_eq!(stored.iter().collect::<Vec<_>>(), [Topic::Travel]);
    assert_eq!(stored.known(), Topic::Travel.into());
    let json = serde_json::to_string(&stored).unwrap();
    assert_eq!(json, (stored.bits()).to_string());
    assert_eq!(serde_json::from_str::<TopicSet>(&json).unwrap(), stored);

    let mut set: TopicSet = [Topic::Jobs, Topic::Purchases].into_iter().collect();
    assert_eq!(set.bits(), 0b10_0001);
    set.remove(Topic::Jobs);
    set.insert(Topic::Events);
    assert_eq!(set | Topic::Travel.into(), TopicSet::from_bits(0b100_0011));
}

#[test]
fn list_topic_skips_health() {
    assert_eq!(TopicSet::EMPTY.list_topic(), None);
    assert_eq!(TopicSet::from(Topic::Health).list_topic(), None);
    assert_eq!(TopicSet::from_bits(1 << 40).list_topic(), None);
    let set: TopicSet = [Topic::Health, Topic::Social, Topic::Government]
        .into_iter()
        .collect();
    assert_eq!(set.list_topic(), Some(Topic::Social));
    let set: TopicSet = [Topic::Finance, Topic::Purchases].into_iter().collect();
    assert_eq!(set.list_topic(), Some(Topic::Purchases));
}

#[test]
fn gated_drops_sensitive_topics_unless_aligned() {
    let all: TopicSet = Topic::ALL.iter().copied().collect();
    let aligned = TrustState::Aligned {
        domain: "bank.example.com".into(),
    };
    assert_eq!(all.gated(&aligned, true), all);
    assert_eq!(
        all.gated(&aligned, false),
        Topic::ALL
            .iter()
            .copied()
            .filter(|t| *t != Topic::Health)
            .collect()
    );
    let open: TopicSet = [
        Topic::Purchases,
        Topic::Travel,
        Topic::Social,
        Topic::Jobs,
        Topic::Events,
    ]
    .into_iter()
    .collect();
    for trust in [
        TrustState::Relayed {
            provider: "relay.example.org".into(),
        },
        TrustState::Unverified,
        TrustState::Unknown,
    ] {
        assert_eq!(all.gated(&trust, true), open, "{}", trust.name());
        assert_eq!(all.gated(&trust, false), open, "{}", trust.name());
    }
    assert_eq!(
        TopicSet::from_bits(all.bits() | (1 << 40)).gated(&aligned, true),
        all
    );
}

#[test]
fn reservation_kinds_set() {
    let mut set: ReservationKinds = [ReservationKind::Flight, ReservationKind::RentalCar]
        .into_iter()
        .collect();
    assert!(set.contains(ReservationKind::RentalCar));
    set.remove(ReservationKind::Flight);
    set.insert(ReservationKind::Event);
    assert_eq!(
        set.iter().collect::<Vec<_>>(),
        [ReservationKind::Event, ReservationKind::RentalCar]
    );
    assert!(ReservationKinds::EMPTY.is_empty());
}

#[test]
fn errors_carry_no_content() {
    assert_eq!(
        CategoryError::Unavailable.to_string(),
        "category data unavailable"
    );
    assert_eq!(
        CategoryError::Data("header row").to_string(),
        "invalid category data: header row"
    );
}

#[cfg(test)]
mod fake {
    use std::collections::HashMap;

    use super::*;
    use crate::testing::{FakeCategorizer, conformance};

    fn vote(topic: Topic) -> CategoryVote {
        CategoryVote {
            kind: None,
            kind_source: CategorySource::Domain,
            topics: topic.into(),
            source: CategorySource::Domain,
        }
    }

    fn input(domain: &str) -> CategoryInput {
        CategoryInput {
            signals: Signals::EMPTY,
            kinds: KindTags::EMPTY,
            reservations: ReservationKinds::EMPTY,
            structured: false,
            forwarded: false,
            own: false,
            trust: TrustState::Unknown,
            sender_address: Some(format!("ada@{domain}")),
            sender_domain: Some(domain.into()),
        }
    }

    #[test]
    fn fake_keeps_the_contract_and_swaps() {
        let fake = FakeCategorizer::new(
            "fake",
            "v1",
            HashMap::from([("shop.example.com".into(), vote(Topic::Purchases))]),
        );
        conformance::categorizer(
            &fake,
            &[
                (input("shop.example.com"), Some(vote(Topic::Purchases))),
                (input("example.org"), None),
            ],
        );
        let before = fake.snapshot();
        fake.swap(
            "v2",
            HashMap::from([("shop.example.com".into(), vote(Topic::Travel))]),
        );
        let after = fake.snapshot();
        assert_eq!(before.version(), "v1");
        assert_eq!(after.version(), "v2");
        assert_eq!(
            before.categorize(&input("shop.example.com")),
            Some(vote(Topic::Purchases))
        );
        assert_eq!(
            after.categorize(&input("shop.example.com")),
            Some(vote(Topic::Travel))
        );
    }
}
