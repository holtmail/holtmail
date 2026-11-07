use std::collections::BTreeMap;

use holtmail_insight::{
    Confidence, Explanation, Insight, InsightInput, InsightKind, PartRef, Provenance, Skeleton,
    fits_identifier,
};

use crate::check::check_digit;
use crate::data::{Carrier, Data, Keyword};
use crate::doc::{Doc, FOOTER, FORWARD, fold, is_cjk};

const MAX_ITEMS: usize = 4;
const MAX_CANDIDATES: usize = 64;
const MAX_STEPS: usize = 512;
const MAX_WORDS: usize = 200_000;

/// Explanation steps, collected only when asked for.
pub(crate) struct Log {
    on: bool,
    steps: Vec<Explanation>,
}

impl Log {
    pub(crate) fn off() -> Self {
        Self {
            on: false,
            steps: Vec::new(),
        }
    }

    pub(crate) fn on() -> Self {
        Self {
            on: true,
            steps: Vec::new(),
        }
    }

    fn push(&mut self, step: impl FnOnce() -> Explanation) {
        if self.on && self.steps.len() < MAX_STEPS {
            self.steps.push(step());
        }
    }

    pub(crate) fn into_steps(self) -> Vec<Explanation> {
        self.steps
    }
}

#[derive(Debug, Clone, Copy)]
struct Word {
    start: usize,
    end: usize,
    digit: bool,
}

fn words(doc: &Doc) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    let mut open: Option<Word> = None;
    for (i, c) in doc.norm.iter().enumerate() {
        if c.is_ascii_alphanumeric() {
            let word = open.get_or_insert(Word {
                start: i,
                end: i,
                digit: false,
            });
            word.end = i + 1;
            word.digit |= c.is_ascii_digit();
        } else if let Some(word) = open.take() {
            out.push(word);
            if out.len() >= MAX_WORDS {
                return out;
            }
        }
    }
    out.extend(open);
    out
}

struct Window {
    carrier: usize,
    number: String,
    start: usize,
    end: usize,
}

fn joined(doc: &Doc, left: &Word, right: &Word) -> bool {
    left.end + 1 == right.start && doc.at(left.end) == ' '
}

fn fits(carrier: &Carrier, number: &str) -> bool {
    carrier.patterns.iter().any(|pattern| {
        pattern.len() == number.len()
            && pattern
                .iter()
                .zip(number.bytes())
                .all(|(shape, b)| shape.fits(b))
    })
}

fn bounded(doc: &Doc, start: usize, end: usize) -> bool {
    let before = start.checked_sub(1).map_or('\0', |i| doc.at(i));
    let before2 = start.checked_sub(2).map_or('\0', |i| doc.at(i));
    let after = doc.at(end);
    let after2 = doc.at(end + 1);
    let joins = |c: char| matches!(c, '-' | '/' | '.' | ',' | '+');
    !(before == '+'
        || matches!(before, '(' | ')')
        || (joins(before) && before2.is_ascii_digit())
        || (joins(after) && after2.is_ascii_digit()))
}

fn windows(data: &Data, doc: &Doc, words: &[Word], log: &mut Log) -> Vec<Window> {
    let mut out = Vec::new();
    let part = doc.part.clone().unwrap_or(PartRef::Text);
    for (i, first) in words.iter().enumerate() {
        let mut number = String::new();
        let mut last = *first;
        for k in 0..data.context.max_group_words {
            let Some(word) = words.get(i + k) else {
                break;
            };
            if k > 0 && !joined(doc, &last, word) {
                break;
            }
            last = *word;
            number.extend(
                doc.norm
                    .get(word.start..word.end)
                    .unwrap_or_default()
                    .iter()
                    .map(char::to_ascii_uppercase),
            );
            if number.len() > data.max_len {
                break;
            }
            let matching: Vec<usize> = data
                .carriers
                .iter()
                .enumerate()
                .filter(|(_, carrier)| fits(carrier, &number))
                .map(|(index, _)| index)
                .collect();
            if matching.is_empty() {
                continue;
            }
            let (start, end) = (first.start, word.end);
            let grouped_run = i
                .checked_sub(1)
                .and_then(|p| words.get(p))
                .is_some_and(|prev| prev.digit && joined(doc, prev, first))
                || words
                    .get(i + k + 1)
                    .is_some_and(|next| next.digit && joined(doc, word, next));
            if grouped_run || !bounded(doc, start, end) {
                log.push(|| {
                    Explanation::new("boundary")
                        .part(part.clone())
                        .at(start)
                        .skeleton(Skeleton::of(&number))
                });
                continue;
            }
            for carrier in matching {
                if out.len() >= MAX_CANDIDATES {
                    log.push(|| Explanation::new("candidates_capped").count(MAX_CANDIDATES));
                    return out;
                }
                out.push(Window {
                    carrier,
                    number: number.clone(),
                    start,
                    end,
                });
            }
        }
    }
    out
}

fn keyword_at(hay: &[char], at: usize, keyword: &[char]) -> bool {
    if hay.get(at..at + keyword.len()) != Some(keyword) {
        return false;
    }
    let bounded = |edge: Option<&char>, outside: Option<&char>| {
        let Some(edge) = edge else {
            return true;
        };
        if !edge.is_alphanumeric() || is_cjk(*edge) {
            return true;
        }
        outside.is_none_or(|c| !c.is_alphanumeric() || is_cjk(*c))
    };
    let before = at.checked_sub(1).and_then(|i| hay.get(i));
    bounded(keyword.first(), before) && bounded(keyword.last(), hay.get(at + keyword.len()))
}

#[derive(Clone, Copy)]
struct Near<'d> {
    keyword: &'d Keyword,
    distance: usize,
    before: bool,
}

fn nearest<'d>(
    doc: &Doc,
    keywords: impl Iterator<Item = &'d Keyword>,
    start: usize,
    end: usize,
    window: usize,
    before_only: bool,
) -> Option<Near<'d>> {
    let mut best: Option<Near<'d>> = None;
    for keyword in keywords {
        let len = keyword.folded.len();
        let from = start.saturating_sub(window + len);
        let to = (end + window).min(doc.len());
        for at in from..to {
            if at + len > doc.len() {
                break;
            }
            if !keyword_at(&doc.fold, at, &keyword.folded) || doc.flagged(at, at + len, FORWARD) {
                continue;
            }
            let near = if at + len <= start {
                Near {
                    keyword,
                    distance: start - (at + len),
                    before: true,
                }
            } else if at >= end {
                Near {
                    keyword,
                    distance: at - end,
                    before: false,
                }
            } else {
                continue;
            };
            if (before_only && !near.before) || near.distance > window {
                continue;
            }
            if best.is_none_or(|b| near.distance < b.distance) {
                best = Some(near);
            }
        }
    }
    best
}

fn in_text<'d>(text: &str, keywords: impl Iterator<Item = &'d Keyword>) -> Option<&'d Keyword> {
    let hay = fold(text);
    keywords
        .into_iter()
        .find(|k| (0..hay.len()).any(|at| keyword_at(&hay, at, &k.folded)))
}

fn from_domains(input: &InsightInput) -> Vec<String> {
    input
        .from
        .iter()
        .filter_map(|a| a.email.rsplit_once('@'))
        .map(|(_, domain)| domain.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

fn host_hint(carrier: &Carrier, domains: &[String]) -> bool {
    domains.iter().any(|domain| {
        carrier.hosts.iter().any(|hint| {
            domain == hint
                || domain
                    .strip_suffix(hint.as_str())
                    .is_some_and(|rest| rest.ends_with('.'))
        })
    })
}

struct Found {
    carrier: usize,
    number: String,
    start: usize,
    score: u16,
    host: bool,
    own_keyword: bool,
}

pub(crate) fn extract(data: &Data, input: &InsightInput, log: &mut Log) -> Vec<Insight> {
    let doc = Doc::of(input);
    let part = doc.part.clone().unwrap_or(PartRef::Text);
    let words = words(&doc);
    let windows = windows(data, &doc, &words, log);
    let domains = from_domains(input);
    let window = data.context.keyword_window;
    let anchors: Vec<_> = input
        .html
        .iter()
        .flat_map(|view| view.anchors.iter())
        .filter(|a| !a.below_forward && !a.in_footer)
        .collect();

    let mut found: Vec<Found> = Vec::new();
    for candidate in windows {
        let Some(carrier) = data.carriers.get(candidate.carrier) else {
            continue;
        };
        let Window {
            number, start, end, ..
        } = &candidate;
        let (start, end) = (*start, *end);
        let step = |rule: &'static str| {
            Explanation::new(rule)
                .part(part.clone())
                .at(start)
                .skeleton(Skeleton::of(number))
        };
        log.push(|| step("pattern"));
        if check_digit(&carrier.check, number) != Some(true) {
            log.push(|| step("check_digit").score(0));
            continue;
        }
        log.push(|| step("check_digit").score(1));
        if doc.flagged(start, end, FORWARD) {
            log.push(|| step("forwarded"));
            continue;
        }
        if doc.flagged(start, end, FOOTER) {
            log.push(|| step("footer"));
            continue;
        }
        let closest = |before_only: bool| {
            let generic = nearest(&doc, data.keywords.iter(), start, end, window, before_only);
            let own = nearest(
                &doc,
                carrier.keywords.iter(),
                start,
                end,
                window,
                before_only,
            );
            match (generic, own) {
                (Some(g), Some(o)) => Some(if o.distance <= g.distance { o } else { g }),
                (g, o) => g.or(o),
            }
        };
        let keyword = closest(false);
        let own = nearest(&doc, carrier.keywords.iter(), start, end, window, false);
        let negative = nearest(
            &doc,
            data.negative.iter(),
            start,
            start,
            data.context.negative_window,
            true,
        );
        if let Some(negative) = negative
            && closest(true).is_none_or(|k| k.distance > negative.distance)
        {
            log.push(|| step("negative").keyword(negative.keyword.written.clone()));
            continue;
        }
        if let Some(keyword) = keyword {
            log.push(|| {
                step("keyword")
                    .score(i32::try_from(keyword.distance).unwrap_or(i32::MAX))
                    .keyword(keyword.keyword.written.clone())
            });
        }
        let lower = number.to_ascii_lowercase();
        let anchor = anchors
            .iter()
            .filter(|a| a.href.to_ascii_lowercase().contains(&lower))
            .find_map(|a| {
                let labels = data.keywords.iter().chain(carrier.keywords.iter());
                in_text(&a.text, labels.clone())
                    .or_else(|| a.aria_label.as_deref().and_then(|l| in_text(l, labels)))
            });
        if let Some(anchor) = anchor {
            log.push(|| step("anchor").keyword(anchor.written.clone()));
        }
        let host = host_hint(carrier, &domains);
        if host {
            log.push(|| step("host_hint"));
        }
        let context = keyword.is_some() || anchor.is_some();
        let score = match (context, host) {
            (true, true) => data.scores.keyword_and_host,
            (false, true) => data.scores.host,
            (true, false) => data.scores.keyword,
            (false, false) => {
                log.push(|| step("no_evidence"));
                continue;
            }
        };
        found.push(Found {
            carrier: candidate.carrier,
            number: number.clone(),
            start,
            score,
            host,
            own_keyword: own.is_some(),
        });
    }

    let mut by_place: BTreeMap<(usize, String), Vec<Found>> = BTreeMap::new();
    for item in found {
        by_place
            .entry((item.start, item.number.clone()))
            .or_default()
            .push(item);
    }
    let mut kept: Vec<Found> = Vec::new();
    for ((start, number), mut group) in by_place {
        if group.len() > 1 {
            let strength = |f: &Found| (f.host, f.own_keyword);
            let best = group.iter().map(strength).max();
            group.retain(|f| Some(strength(f)) == best);
            if group.len() != 1 {
                log.push(|| {
                    Explanation::new("ambiguous")
                        .part(part.clone())
                        .at(start)
                        .count(group.len())
                        .skeleton(Skeleton::of(&number))
                });
                continue;
            }
        }
        kept.extend(group);
    }

    kept.sort_by(|a, b| b.score.cmp(&a.score).then(a.start.cmp(&b.start)));
    let mut items: Vec<Insight> = Vec::new();
    let mut seen: Vec<(usize, String)> = Vec::new();
    for item in kept {
        if items.len() >= MAX_ITEMS {
            break;
        }
        let key = (item.carrier, item.number.clone());
        if seen.contains(&key) || !fits_identifier(&item.number) {
            continue;
        }
        let Some(carrier) = data.carriers.get(item.carrier) else {
            continue;
        };
        log.push(|| {
            Explanation::new("chosen")
                .part(part.clone())
                .at(item.start)
                .score(i32::from(item.score))
                .skeleton(Skeleton::of(&item.number))
        });
        seen.push(key);
        items.push(Insight {
            kind: InsightKind::Parcel {
                carrier: carrier.id.clone(),
                track_url: carrier.track_url(&item.number),
                number: item.number,
                check_digit: true,
            },
            provenance: Provenance::Heuristic,
            confidence: if item.score >= data.scores.likely {
                Confidence::Likely
            } else {
                Confidence::Possible
            },
            score: item.score,
            origin: part.clone(),
            forwarded: false,
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use holtmail_core::Address;
    use holtmail_insight::{Anchor, HtmlView, Segment};

    use super::*;
    use crate::data::SOURCE;

    fn data() -> Data {
        Data::parse(SOURCE).unwrap()
    }

    fn text(body: &str) -> InsightInput {
        InsightInput {
            from: vec![Address {
                name: None,
                email: "shop@example.com".into(),
            }],
            text: body.into(),
            ..InsightInput::default()
        }
    }

    fn numbers(input: &InsightInput) -> Vec<(String, String)> {
        extract(&data(), input, &mut Log::off())
            .into_iter()
            .filter_map(|item| match item.kind {
                InsightKind::Parcel {
                    carrier, number, ..
                } => Some((carrier.as_str().to_owned(), number)),
                _ => None,
            })
            .collect()
    }

    fn one(carrier: &str, number: &str) -> Vec<(String, String)> {
        vec![(carrier.to_owned(), number.to_owned())]
    }

    #[test]
    fn groupings_are_joined_and_uppercased() {
        assert_eq!(
            numbers(&text("Tracking number: 1Z 999 AA1 01 2345 6784")),
            one("ups", "1Z999AA10123456784")
        );
        assert_eq!(
            numbers(&text("Трек-номер: ra 123 456 785 ru")),
            one("s10", "RA123456785RU")
        );
        assert_eq!(
            numbers(&text("Tracking: 7985 3018 4721.")),
            one("fedex", "798530184721")
        );
    }

    #[test]
    fn runs_longer_than_the_number_give_nothing() {
        assert_eq!(
            numbers(&text("Tracking number: 1234567891")),
            one("dhl_express", "1234567891")
        );
        for body in [
            "Tracking: 1234 7985 3018 4721",
            "Tracking: 7985 3018 4721 1234",
            "Tracking: 12-798530184721",
            "Tracking: 798530184721.5",
            "Tracking: +798530184721",
            "Tracking: 47382910512",
            "Tracking number: 55 1234567891",
            "Tracking number: 1234567891 55",
            "Tracking: 0172 798530184721",
        ] {
            assert!(numbers(&text(body)).is_empty(), "{body}");
        }
    }

    #[test]
    fn keyword_must_be_near() {
        let far = format!("Tracking.{}1Z999AA10123456784", " filler".repeat(20));
        assert!(numbers(&text(&far)).is_empty());
        assert_eq!(
            numbers(&text("1Z999AA10123456784 is your tracking number")),
            one("ups", "1Z999AA10123456784")
        );
        assert!(numbers(&text("Your parcel trackingnumber1Z999AA10123456784")).is_empty());
    }

    #[test]
    fn negative_words_win_when_closer() {
        assert!(numbers(&text("Order number 798530184721, tracking follows.")).is_empty());
        assert_eq!(
            numbers(&text("Your order shipped. Tracking number 798530184721.")),
            one("fedex", "798530184721")
        );
        assert!(numbers(&text("Tracking questions? Call 4738291051")).is_empty());
        assert_eq!(
            numbers(&text(
                "Your order shipped.\nTracking: 798530184721\nTrack your parcel"
            )),
            one("fedex", "798530184721")
        );
    }

    #[test]
    fn segments_below_forward_or_in_footer_give_nothing() {
        let seg = |text: &str, below_forward| Segment {
            text: text.into(),
            below_forward,
            ..Segment::default()
        };
        let view = HtmlView::new().with_segments(vec![
            seg("Hello", false),
            seg("Tracking number RA123456785RU", true),
        ]);
        let input = InsightInput {
            html: Some(view),
            ..text("")
        };
        assert!(numbers(&input).is_empty());

        let view = HtmlView::new()
            .with_segments(vec![
                seg("Hello", false),
                seg("Tracking number RA123456785RU", false),
            ])
            .with_footer_at(Some(1));
        let input = InsightInput {
            html: Some(view),
            ..text("")
        };
        assert!(numbers(&input).is_empty());
    }

    #[test]
    fn anchors_add_context_but_never_numbers() {
        let view = HtmlView::new()
            .with_segments(vec![Segment {
                text: "Your parcel 798530184721 is on its way".into(),
                ..Segment::default()
            }])
            .with_anchors(vec![Anchor {
                href: "https://shop.example.com/t/798530184721".into(),
                text: "Track package".into(),
                ..Anchor::default()
            }]);
        let input = InsightInput {
            html: Some(view),
            ..text("")
        };
        assert_eq!(numbers(&input), one("fedex", "798530184721"));

        let view = HtmlView::new().with_anchors(vec![Anchor {
            href: "https://shop.example.com/t/798530184721".into(),
            text: "Track package".into(),
            ..Anchor::default()
        }]);
        let input = InsightInput {
            html: Some(view),
            ..text("")
        };
        assert!(numbers(&input).is_empty());
    }

    #[test]
    fn at_most_four_items_best_first() {
        let body = "Tracking numbers: RA123456785RU, CP473829103DE, EE582039171CN, \
                    LX908172638US, 798530184721, 4738291051, RA123456785RU";
        let items = extract(&data(), &text(body), &mut Log::off());
        assert_eq!(items.len(), MAX_ITEMS);
        let mut unique: Vec<_> = items.iter().map(|i| format!("{:?}", i.kind)).collect();
        unique.dedup();
        assert_eq!(unique.len(), MAX_ITEMS);
    }
}
