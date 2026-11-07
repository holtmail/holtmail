//! The §15 `.eml` fixtures, through `holtmail_mime`, stage A of the runner over the real
//! extractors, and the categorizer. The trust of each fixture is given here; the runner's own
//! tests compute it from the same files and apply the gate.

use std::sync::Arc;

use holtmail_category::{
    Categorizer, CategoryInput, CategorySource, MessageKind, ReservationKind, Signal, Topic,
    TopicSet, TrustState,
};
use holtmail_category_signals::SignalsCategorizer;
use holtmail_insight::{Confidence, InsightExtractor, InsightKind};
use holtmail_insight_code::CodeExtractor;
use holtmail_insight_links::LinksExtractor;
use holtmail_insight_parcel::ParcelExtractor;
use holtmail_insight_pass::PassExtractor;
use holtmail_insight_schemaorg::SchemaOrgExtractor;
use holtmail_insight_unsubscribe::UnsubscribeExtractor;
use holtmail_insights::Runner;

use CategorySource::{Content, Headers};
use MessageKind::{Bulk, List, Notification, Personal};

struct Fixture {
    name: &'static str,
    own: bool,
    trust: TrustState,
    kind: MessageKind,
    kind_source: CategorySource,
    topics: &'static [Topic],
}

fn aligned(domain: &str) -> TrustState {
    TrustState::Aligned {
        domain: domain.into(),
    }
}

fn fixtures() -> Vec<Fixture> {
    let f = |name, own, trust, kind, kind_source, topics| Fixture {
        name,
        own,
        trust,
        kind,
        kind_source,
        topics,
    };
    vec![
        f(
            "kind-transactional-via-esp",
            false,
            aligned("shop.example.com"),
            Notification,
            Content,
            &[Topic::Purchases],
        ),
        f(
            "kind-list-discussion",
            false,
            TrustState::Unknown,
            List,
            Headers,
            &[],
        ),
        f(
            "kind-newsletter-travel",
            false,
            aligned("travel.example.com"),
            Bulk,
            Headers,
            &[],
        ),
        f(
            "kind-unsub-only-transactional",
            false,
            TrustState::Unknown,
            Personal,
            Headers,
            &[],
        ),
        f(
            "kind-auto-replied",
            false,
            TrustState::Unknown,
            Personal,
            Headers,
            &[],
        ),
        f(
            "own-sent-order",
            true,
            TrustState::Unknown,
            Personal,
            Headers,
            &[Topic::Purchases],
        ),
        f(
            "finance-spoofed-unverified",
            false,
            TrustState::Unverified,
            Notification,
            Content,
            &[Topic::Finance],
        ),
        f(
            "finance-override-spoofed",
            false,
            TrustState::Unverified,
            Personal,
            Headers,
            &[],
        ),
    ]
}

fn runner() -> Runner {
    let extractors: Vec<Arc<dyn InsightExtractor>> = vec![
        Arc::new(UnsubscribeExtractor::new()),
        Arc::new(SchemaOrgExtractor::new()),
        Arc::new(PassExtractor::new()),
        Arc::new(ParcelExtractor::new()),
        Arc::new(LinksExtractor::new()),
        Arc::new(CodeExtractor::new()),
    ];
    Runner::new(extractors)
}

fn category_input(name: &str, own: bool, trust: TrustState) -> CategoryInput {
    let path = format!("{}/tests/fixtures/{name}.eml", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap_or_else(|_| panic!("{path} is missing"));
    let message = holtmail_mime::parse(&raw).unwrap_or_else(|_| panic!("{name} does not parse"));
    let extracted = runner().extract(&message);
    let reservations = extracted
        .items
        .items
        .iter()
        .filter(|item| item.confidence >= Confidence::Likely && !item.forwarded)
        .filter_map(|item| match &item.kind {
            InsightKind::Reservation(reservation) => Some(ReservationKind::from(reservation)),
            _ => None,
        })
        .collect();
    let sender_domain = extracted
        .from_address
        .as_deref()
        .and_then(|address| address.rsplit_once('@'))
        .map(|(_, domain)| domain.to_owned());
    CategoryInput {
        signals: extracted.signals,
        kinds: extracted.kinds,
        reservations,
        structured: extracted.signals.contains(Signal::Structured),
        forwarded: extracted.signals.contains(Signal::ForwardMarker),
        own,
        trust,
        sender_address: extracted.from_address,
        sender_domain,
    }
}

#[test]
fn every_fixture_gets_its_vote() {
    let snapshot = SignalsCategorizer::new().snapshot();
    for fixture in fixtures() {
        let name = fixture.name;
        let input = category_input(name, fixture.own, fixture.trust);
        let vote = snapshot
            .categorize(&input)
            .unwrap_or_else(|| panic!("{name}: no vote"));
        assert_eq!(vote.kind, Some(fixture.kind), "{name}");
        assert_eq!(vote.kind_source, fixture.kind_source, "{name}");
        assert_eq!(
            vote.topics,
            fixture.topics.iter().copied().collect::<TopicSet>(),
            "{name}"
        );
        assert_eq!(vote.source, Content, "{name}");
    }
}

#[test]
fn spoofed_finance_is_voted_and_left_to_the_gate() {
    let snapshot = SignalsCategorizer::new().snapshot();
    let input = category_input("finance-spoofed-unverified", false, TrustState::Unverified);
    assert!(input.structured);
    let vote = snapshot.categorize(&input).unwrap();
    assert!(vote.topics.contains(Topic::Finance));
    assert!(vote.topics.gated(&input.trust, true).is_empty());
    assert_eq!(
        vote.topics.gated(&aligned("bank.example.com"), true),
        Topic::Finance.into()
    );

    let input = category_input("finance-override-spoofed", false, TrustState::Unverified);
    let filed_by_user = TopicSet::from(Topic::Finance);
    assert!(filed_by_user.gated(&input.trust, true).is_empty());
}

#[test]
fn signals_behind_the_votes() {
    let has = |name, signal| {
        category_input(name, false, TrustState::Unknown)
            .signals
            .contains(signal)
    };
    assert!(has("kind-transactional-via-esp", Signal::Esp));
    assert!(has("kind-transactional-via-esp", Signal::Structured));
    assert!(has("kind-newsletter-travel", Signal::ListPostNo));
    assert!(!has("kind-newsletter-travel", Signal::ListPost));
    assert!(has("kind-list-discussion", Signal::ListPost));
    assert!(has("kind-auto-replied", Signal::AutoReplied));
    assert!(has("own-sent-order", Signal::Structured));
}
