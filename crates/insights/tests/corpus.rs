//! The precision gate of docs/INSIGHTS.md §15: stage A with every built extractor over the
//! invented corpus in `tests/corpus`, compared with `labels.toml`; then stage B with the
//! `signals` categorizer and no trusted authserv-id, for the kind and topics.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use common::{Label, LinkLabel, ParcelLabel, StructuredLabel, UnsubscribeLabel};
use holtmail_category::{MessageKind, Topic};
use holtmail_category_signals::SignalsCategorizer;
use holtmail_core::{ArchiveId, MessageId};
use holtmail_insight::{Confidence, Insight, InsightKind, Provenance, Reservation};
use holtmail_insights::{DeriveConfig, DeriveContext, Deriver, Extracted};
use holtmail_store::DeriveRow;

const MIN_CASES: usize = 300;
const MIN_HARD_NEGATIVES: usize = 100;
const CODE_PRECISION: f64 = 0.98;
const CODE_RECALL: f64 = 0.85;
const UNSUBSCRIBE_PRECISION: f64 = 1.0;
const UNSUBSCRIBE_RECALL: f64 = 1.0;
const LINK_PRECISION: f64 = 0.95;
const PARCEL_PRECISION: f64 = 0.99;
const STRUCTURED_PRECISION: f64 = 1.0;
const STRUCTURED_RECALL: f64 = 1.0;
const KIND_ACCURACY: f64 = 0.97;
const TOPIC_PRECISION: f64 = 1.0;

#[derive(Default)]
struct Categories {
    kinds: BTreeMap<(MessageKind, MessageKind), usize>,
    kind_misses: Vec<String>,
    topics_right: usize,
    topics_spurious: Vec<String>,
    topics_missed: Vec<String>,
}

impl Categories {
    fn record(&mut self, case: &str, label: &Label, derived: (MessageKind, Vec<Topic>)) {
        let expected = label
            .kind
            .as_deref()
            .and_then(MessageKind::from_name)
            .expect("checked");
        let (kind, topics) = derived;
        *self.kinds.entry((expected, kind)).or_default() += 1;
        if expected != kind {
            self.kind_misses
                .push(format!("{case} ({expected} as {kind})"));
        }
        let wanted: Vec<Topic> = label
            .topics
            .iter()
            .flatten()
            .filter_map(|name| Topic::from_name(name))
            .collect();
        for topic in &topics {
            if wanted.contains(topic) {
                self.topics_right += 1;
            } else {
                self.topics_spurious.push(format!("{case} ({topic})"));
            }
        }
        for topic in wanted.iter().filter(|t| !topics.contains(t)) {
            self.topics_missed.push(format!("{case} ({topic})"));
        }
    }

    fn accuracy(&self) -> f64 {
        let total: usize = self.kinds.values().sum();
        let right: usize = self
            .kinds
            .iter()
            .filter(|((expected, found), _)| expected == found)
            .map(|(_, n)| n)
            .sum();
        if total == 0 {
            return 1.0;
        }
        right as f64 / total as f64
    }

    fn topic_precision(&self) -> f64 {
        let predicted = self.topics_right + self.topics_spurious.len();
        if predicted == 0 {
            return 1.0;
        }
        self.topics_right as f64 / predicted as f64
    }

    fn topic_recall(&self) -> f64 {
        let expected = self.topics_right + self.topics_missed.len();
        if expected == 0 {
            return 1.0;
        }
        self.topics_right as f64 / expected as f64
    }

    fn summary(&self, out: &mut String) {
        let _ = writeln!(out, "kind: accuracy {:.3}", self.accuracy());
        let _ = write!(out, "  {:<14}", "expected\\found");
        for found in MessageKind::ALL {
            let _ = write!(out, "{:>14}", found.name());
        }
        let _ = writeln!(out);
        for expected in MessageKind::ALL {
            let _ = write!(out, "  {:<14}", expected.name());
            for found in MessageKind::ALL {
                let n = self.kinds.get(&(*expected, *found)).copied().unwrap_or(0);
                let _ = write!(out, "{n:>14}");
            }
            let _ = writeln!(out);
        }
        let _ = writeln!(
            out,
            "topics: precision {:.3}, recall {:.3}; {} right, {} spurious, {} missed",
            self.topic_precision(),
            self.topic_recall(),
            self.topics_right,
            self.topics_spurious.len(),
            self.topics_missed.len(),
        );
        for (name, cases) in [
            ("kind wrong", &self.kind_misses),
            ("topic spurious", &self.topics_spurious),
            ("topic missed", &self.topics_missed),
        ] {
            if !cases.is_empty() {
                let _ = writeln!(out, "  {name}: {}", cases.join(", "));
            }
        }
    }
}

/// Stage B for one message of the corpus: the signals categorizer, no trusted authserv-id and
/// no own address, so trust is unknown.
fn categories(
    deriver: &Deriver,
    ctx: &DeriveContext,
    extracted: &Extracted,
) -> (MessageKind, Vec<Topic>) {
    let row = DeriveRow {
        message: MessageId::new(),
        archive: ctx.archive(),
        from_address: extracted.from_address.clone(),
        signals: extracted.signals.bits(),
        kinds: extracted.kinds.bits(),
        auth: extracted.auth.clone(),
        items: extracted.items.to_json(),
        alias: None,
        accounts: Vec::new(),
    };
    let write = deriver.derive(ctx, &row);
    (write.derived_kind, write.derived_topics.iter().collect())
}

#[derive(Default)]
struct Confusion {
    hit: Vec<String>,
    wrong: Vec<String>,
    missed: Vec<String>,
    spurious: Vec<String>,
    quiet: usize,
    possible: Vec<String>,
}

impl Confusion {
    fn precision(&self) -> f64 {
        let predicted = self.hit.len() + self.wrong.len() + self.spurious.len();
        if predicted == 0 {
            return 1.0;
        }
        self.hit.len() as f64 / predicted as f64
    }

    fn recall(&self) -> f64 {
        let expected = self.hit.len() + self.wrong.len() + self.missed.len();
        if expected == 0 {
            return 1.0;
        }
        self.hit.len() as f64 / expected as f64
    }

    fn summary(&self, kind: &str, out: &mut String) {
        let _ = writeln!(
            out,
            "{kind}: precision {:.3}, recall {:.3}; {} right, {} wrong value, {} missed, {} spurious, {} correctly empty",
            self.precision(),
            self.recall(),
            self.hit.len(),
            self.wrong.len(),
            self.missed.len(),
            self.spurious.len(),
            self.quiet,
        );
        for (name, cases) in [
            ("wrong value", &self.wrong),
            ("missed", &self.missed),
            ("spurious", &self.spurious),
            ("only Possible", &self.possible),
        ] {
            if !cases.is_empty() {
                let _ = writeln!(out, "  {name}: {}", cases.join(", "));
            }
        }
    }
}

fn record(confusion: &mut Confusion, case: &str, expected: bool, found: bool, equal: bool) {
    let case = case.to_owned();
    match (expected, found) {
        (true, true) if equal => confusion.hit.push(case),
        (true, true) => confusion.wrong.push(case),
        (true, false) => confusion.missed.push(case),
        (false, true) => confusion.spurious.push(case),
        (false, false) => confusion.quiet += 1,
    }
}

fn unsubscribe_label(kind: &InsightKind) -> Option<UnsubscribeLabel> {
    match kind {
        InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            list_id,
        } => Some(UnsubscribeLabel {
            https: https.as_ref().map(|u| u.as_str().to_owned()),
            http: http.as_ref().map(|u| u.as_str().to_owned()),
            mailto: mailto.clone(),
            one_click: *one_click,
            list_id: list_id.clone(),
        }),
        _ => None,
    }
}

fn shown(item: &Insight) -> bool {
    item.confidence >= Confidence::Likely && !item.forwarded
}

fn name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The shown action with the highest score, the first one on a tie.
fn top_link(items: &[Insight]) -> Option<LinkLabel> {
    let mut best: Option<(u16, LinkLabel)> = None;
    for item in items.iter().filter(|item| shown(item)) {
        if let InsightKind::Action {
            action,
            destination,
            ..
        } = &item.kind
            && best.as_ref().is_none_or(|(score, _)| item.score > *score)
        {
            let label = LinkLabel {
                action: name(action),
                host: destination.host().unwrap_or_default().to_owned(),
            };
            best = Some((item.score, label));
        }
    }
    best.map(|(_, label)| label)
}

fn parcels(items: &[Insight]) -> Vec<ParcelLabel> {
    items
        .iter()
        .filter(|item| shown(item))
        .filter_map(|item| match &item.kind {
            InsightKind::Parcel {
                carrier, number, ..
            } => Some(ParcelLabel {
                carrier: carrier.as_str().to_owned(),
                number: number.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// The shown structured facts: reservations, orders, invoices, parcels and passes from JSON-LD,
/// microdata or a pkpass, by type and reference.
fn structured(items: &[Insight]) -> Vec<StructuredLabel> {
    let label = |kind: &str, reference: &str| {
        Some(StructuredLabel {
            kind: kind.to_owned(),
            reference: reference.to_owned(),
        })
    };
    let either = |first: &Option<String>, second: &str| first.clone().unwrap_or(second.to_owned());
    items
        .iter()
        .filter(|item| shown(item))
        .filter(|item| {
            matches!(
                item.provenance,
                Provenance::JsonLd | Provenance::Microdata | Provenance::PkPass
            )
        })
        .filter_map(|item| match &item.kind {
            InsightKind::Reservation(reservation) => match reservation {
                Reservation::Flight(r) => label(
                    "flight",
                    &either(
                        &r.reservation_number,
                        r.flight_number.as_deref().unwrap_or(""),
                    ),
                ),
                Reservation::Lodging(r) => label("lodging", &r.reservation_number),
                Reservation::Train(r) => label(
                    "train",
                    &either(
                        &r.reservation_number,
                        r.train_number.as_deref().unwrap_or(""),
                    ),
                ),
                Reservation::Bus(r) => label(
                    "bus",
                    &either(&r.reservation_number, r.bus_number.as_deref().unwrap_or("")),
                ),
                Reservation::Event(r) => label("event", &either(&r.reservation_number, &r.name)),
                Reservation::Food(r) => label("food", &either(&r.reservation_number, &r.name)),
                Reservation::RentalCar(r) => label("rental_car", &r.reservation_number),
            },
            InsightKind::Order {
                number, merchant, ..
            } => label("order", &either(number, merchant.as_deref().unwrap_or(""))),
            InsightKind::Invoice { number, payee, .. } => {
                label("invoice", &either(number, payee.as_deref().unwrap_or("")))
            }
            InsightKind::Parcel { number, .. } => label("parcel", number),
            InsightKind::Pass { organization, .. } => label("pass", organization),
            _ => None,
        })
        .collect()
}

#[test]
fn corpus_meets_the_precision_gate() {
    let labels = common::labels();
    check_labels(&labels);
    let runner = common::runner();
    let deriver = Deriver::new(
        vec![Arc::new(SignalsCategorizer::new())],
        DeriveConfig::default(),
    );
    let ctx = deriver.context_from(ArchiveId::new(), &[], &BTreeMap::new(), &[]);

    let mut kinds = Categories::default();
    let mut codes = Confusion::default();
    let mut unsubscribe = Confusion::default();
    let mut links = Confusion::default();
    let mut parcel = Confusion::default();
    let mut facts = Confusion::default();
    for label in &labels {
        let case = label.file.trim_end_matches(".eml");
        let message = common::parse(&label.file);
        let first = runner.extract(&message);
        let second = runner.extract(&message);
        assert_eq!(first, second, "{case}: two runs differ");
        assert_eq!(
            first.items.to_json(),
            second.items.to_json(),
            "{case}: two runs differ"
        );

        kinds.record(case, label, categories(&deriver, &ctx, &first));

        let mut shown = None;
        let mut found_unsubscribe = None;
        for item in &first.items.items {
            match &item.kind {
                InsightKind::Code {
                    value,
                    origin_bound,
                } => {
                    if item.confidence >= Confidence::Likely && !item.forwarded {
                        assert!(shown.is_none(), "{case}: more than one code");
                        shown = Some((value.clone(), origin_bound.clone()));
                    } else {
                        codes.possible.push(case.to_owned());
                    }
                }
                kind @ InsightKind::Unsubscribe { .. } => {
                    assert!(
                        found_unsubscribe.is_none(),
                        "{case}: more than one unsubscribe item"
                    );
                    found_unsubscribe = unsubscribe_label(kind);
                }
                _ => {}
            }
        }

        let expected_code = label.code.clone().map(|code| (code, label.origin.clone()));
        record(
            &mut codes,
            case,
            expected_code.is_some(),
            shown.is_some(),
            shown == expected_code,
        );
        record(
            &mut unsubscribe,
            case,
            label.unsubscribe.is_some(),
            found_unsubscribe.is_some(),
            found_unsubscribe == label.unsubscribe,
        );

        let expected_link = label
            .links
            .as_ref()
            .and_then(|links| links.first().cloned());
        let found_link = top_link(&first.items.items);
        let equal = found_link == expected_link;
        record(
            &mut links,
            case,
            expected_link.is_some(),
            found_link.is_some(),
            equal,
        );
        if let (Some(found), false) = (&found_link, equal) {
            println!(
                "{case}: link expected {expected_link:?}, found {}/{}",
                found.action, found.host
            );
        }

        let expected_parcels = label.parcel.clone().unwrap_or_default();
        let found_parcels = parcels(&first.items.items);
        for found in &found_parcels {
            if expected_parcels.contains(found) {
                parcel.hit.push(case.to_owned());
            } else {
                parcel.spurious.push(format!("{case} ({})", found.carrier));
            }
        }
        for expected in &expected_parcels {
            if !found_parcels.contains(expected) {
                parcel.missed.push(case.to_owned());
            }
        }
        if expected_parcels.is_empty() && found_parcels.is_empty() {
            parcel.quiet += 1;
        }

        let mut expected_facts = label.structured.clone().unwrap_or_default();
        let found_facts = structured(&first.items.items);
        if expected_facts.is_empty() && found_facts.is_empty() {
            facts.quiet += 1;
        }
        for found in found_facts {
            match expected_facts.iter().position(|e| *e == found) {
                Some(at) => {
                    expected_facts.remove(at);
                    facts.hit.push(case.to_owned());
                }
                None => facts
                    .spurious
                    .push(format!("{case} ({} {})", found.kind, found.reference)),
            }
        }
        for missed in expected_facts {
            facts
                .missed
                .push(format!("{case} ({} {})", missed.kind, missed.reference));
        }
    }

    let mut summary = format!("corpus: {} cases\n", labels.len());
    codes.summary("codes", &mut summary);
    unsubscribe.summary("unsubscribe", &mut summary);
    links.summary("links (top-1)", &mut summary);
    parcel.summary("parcels", &mut summary);
    facts.summary("structured", &mut summary);
    kinds.summary(&mut summary);
    println!("{summary}");

    assert!(
        codes.precision() >= CODE_PRECISION && codes.recall() >= CODE_RECALL,
        "codes below the gate ({CODE_PRECISION} / {CODE_RECALL})\n{summary}"
    );
    assert!(
        unsubscribe.precision() >= UNSUBSCRIBE_PRECISION
            && unsubscribe.recall() >= UNSUBSCRIBE_RECALL,
        "unsubscribe below the gate\n{summary}"
    );
    assert!(
        links.precision() >= LINK_PRECISION,
        "links below the gate ({LINK_PRECISION})\n{summary}"
    );
    assert!(
        parcel.precision() >= PARCEL_PRECISION,
        "parcels below the gate ({PARCEL_PRECISION})\n{summary}"
    );
    assert!(
        facts.precision() >= STRUCTURED_PRECISION && facts.recall() >= STRUCTURED_RECALL,
        "structured facts below the gate ({STRUCTURED_PRECISION} / {STRUCTURED_RECALL})\n{summary}"
    );
    assert!(
        kinds.accuracy() >= KIND_ACCURACY,
        "kinds below the gate ({KIND_ACCURACY})\n{summary}"
    );
    assert!(
        kinds.topic_precision() >= TOPIC_PRECISION,
        "topics below the gate ({TOPIC_PRECISION})\n{summary}"
    );
}

fn check_labels(labels: &[Label]) {
    let files: BTreeSet<String> = common::files().into_iter().collect();
    let labelled: BTreeSet<String> = labels.iter().map(|l| l.file.clone()).collect();
    assert_eq!(labelled.len(), labels.len(), "a file is labelled twice");
    assert_eq!(files, labelled, "every .eml file needs exactly one label");
    assert!(
        labels.len() >= MIN_CASES,
        "{} cases, the gate needs {MIN_CASES}",
        labels.len()
    );
    for label in labels {
        assert!(
            label.code.is_none() || label.negative.is_none(),
            "{}: a code and a negative family",
            label.file
        );
        assert!(
            label.origin.is_none() || label.code.is_some(),
            "{}: an origin without a code",
            label.file
        );
        assert!(
            label
                .kind
                .as_deref()
                .and_then(MessageKind::from_name)
                .is_some(),
            "{}: no kind or an unknown one",
            label.file
        );
        let topics = label
            .topics
            .as_ref()
            .unwrap_or_else(|| panic!("{}: no topics", label.file));
        for name in topics {
            let topic = Topic::from_name(name)
                .unwrap_or_else(|| panic!("{}: unknown topic {name}", label.file));
            assert!(
                !topic.is_sensitive(),
                "{}: {name} needs aligned trust, the corpus has none",
                label.file
            );
        }
    }
    let negatives = labels.iter().filter(|l| l.negative.is_some()).count();
    assert!(
        negatives >= MIN_HARD_NEGATIVES,
        "{negatives} hard negatives, the gate needs {MIN_HARD_NEGATIVES}"
    );
}
