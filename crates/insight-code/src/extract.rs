use std::collections::{BTreeMap, BTreeSet};

use holtmail_insight::{
    Confidence, Explanation, Insight, InsightInput, InsightKind, PartRef, Provenance, Skeleton,
    fits_identifier,
};

use crate::data::Data;
use crate::doc::{Doc, FORWARD, URL, is_cjk};
use crate::matcher::{Hits, List};
use crate::origin;

const MAX_CONTEXT_CHARS: usize = 256;
const PHONE_REACH: usize = 32;
const PHONE_DIGITS: usize = 10;

/// A hard-negative rule: an occurrence it matches scores 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    YearNearMonth,
    Money,
    Phone,
    Postcode,
    ReferencePrefix,
    PromoKeyword,
    InUrl,
    Footer,
    Forwarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Digits,
    Alphanumeric,
}

#[derive(Debug, Clone)]
struct Candidate {
    start: usize,
    end: usize,
    value: String,
    shape: Shape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Subject,
    Text,
    Html,
}

struct Scan {
    doc: Doc,
    hits: Hits,
    footer_start: Option<usize>,
    part: Part,
}

impl Scan {
    fn new(doc: Doc, data: &Data, part: Part) -> Self {
        let hits = data.matcher.hits(&doc.fold);
        let footer_start = if part == Part::Text {
            hits.first(List::Footer)
        } else {
            doc.footer_start
        };
        Self {
            doc,
            hits,
            footer_start,
            part,
        }
    }
}

#[derive(Debug, Clone)]
struct Group {
    best: i32,
    origin: PartRef,
    positive: i32,
    shape: Shape,
    skeleton: Skeleton,
}

impl Rule {
    fn name(self) -> &'static str {
        match self {
            Self::YearNearMonth => "year_near_month",
            Self::Money => "money",
            Self::Phone => "phone",
            Self::Postcode => "postcode",
            Self::ReferencePrefix => "reference_prefix",
            Self::PromoKeyword => "promo_keyword",
            Self::InUrl => "in_url",
            Self::Footer => "footer",
            Self::Forwarded => "forwarded",
        }
    }
}

impl Part {
    fn part_ref(self) -> PartRef {
        match self {
            Self::Subject => PartRef::Subject,
            Self::Text => PartRef::Text,
            Self::Html => PartRef::Html,
        }
    }
}

/// The steps of one run, recorded only when explaining.
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
        if self.on {
            self.steps.push(step());
        }
    }

    pub(crate) fn into_steps(self) -> Vec<Explanation> {
        self.steps
    }
}

/// The table's own spelling of the keyword hit at `[start, end)` of `doc`.
fn keyword_name(data: &Data, doc: &Doc, (start, end): (usize, usize)) -> Option<String> {
    let text: String = doc.fold.get(start..end)?.iter().collect();
    data.keywords.get(&text).cloned()
}

fn skeleton(doc: &Doc, c: &Candidate) -> Skeleton {
    let text: String = doc
        .norm
        .get(c.start..c.end)
        .unwrap_or_default()
        .iter()
        .collect();
    Skeleton::of(&text)
}

pub(crate) fn extract(data: &Data, input: &InsightInput, log: &mut Log) -> Option<Insight> {
    if let Some((value, host)) = origin::bound(input) {
        log.push(|| {
            Explanation::new("origin_bound")
                .part(PartRef::Text)
                .score(1000)
                .skeleton(Skeleton::of(&value))
        });
        return Some(Insight {
            kind: InsightKind::Code {
                value,
                origin_bound: Some(host),
            },
            provenance: Provenance::OriginBound,
            confidence: Confidence::Certain,
            score: 1000,
            origin: PartRef::Text,
            forwarded: false,
        });
    }
    if listed(input) {
        log.push(|| Explanation::new("gate:list_id").part(PartRef::Header("List-Id".into())));
        return None;
    }
    let w = &data.weights;

    let subject = input
        .subject
        .as_deref()
        .map(|s| Scan::new(Doc::line(s), data, Part::Subject));
    let text = (!input.text.trim().is_empty())
        .then(|| Scan::new(Doc::text(&input.text, input.forward_at), data, Part::Text));
    let html = input
        .html
        .as_ref()
        .filter(|view| view.segments.iter().any(|s| !s.text.trim().is_empty()))
        .map(|view| Scan::new(Doc::segments(view), data, Part::Html));
    let (body, other, part) = match (html, text) {
        (Some(html), text) => (Some(html), text, PartRef::Html),
        (None, text) => (text, None, PartRef::Text),
    };

    fn early(scan: &Option<Scan>, list: List, limit: usize) -> Option<(&Scan, (usize, usize))> {
        scan.as_ref()
            .and_then(|s| s.hits.first_hit(list).map(|hit| (s, hit)))
            .filter(|(_, (start, _))| *start < limit)
    }
    let keyword =
        early(&subject, List::Code, usize::MAX).or_else(|| early(&body, List::Code, w.gate_chars));
    let backup = early(&subject, List::Backup, usize::MAX)
        .or_else(|| early(&body, List::Backup, w.gate_chars));
    let Some((scan, hit)) = keyword else {
        log.push(|| Explanation::new("gate:no_keyword"));
        return None;
    };
    log.push(|| {
        let step = Explanation::new("gate:keyword")
            .part(scan.part.part_ref())
            .at(hit.0);
        match keyword_name(data, &scan.doc, hit) {
            Some(name) => step.keyword(name),
            None => step,
        }
    });
    if let Some((scan, hit)) = backup {
        log.push(|| {
            let step = Explanation::new("gate:backup_list")
                .part(scan.part.part_ref())
                .at(hit.0);
            match keyword_name(data, &scan.doc, hit) {
                Some(name) => step.keyword(name),
                None => step,
            }
        });
        return None;
    }
    let alphanumeric = !bulk(data, input);
    if !alphanumeric {
        log.push(|| Explanation::new("gate:bulk_digits_only"));
    }

    let subject_found = match &subject {
        Some(scan) => scored(data, scan, alphanumeric, log)?,
        None => Vec::new(),
    };
    let body_found = match &body {
        Some(scan) => scored(data, scan, alphanumeric, log)?,
        None => Vec::new(),
    };
    let other_values: BTreeSet<String> = match &other {
        Some(scan) => positive_values(&scored(data, scan, alphanumeric, log)?),
        None => BTreeSet::new(),
    };
    let subject_values = positive_values(&subject_found);

    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut add = |candidate: Candidate, score: i32, origin: PartRef, skeleton: Skeleton| {
        let group = groups.entry(candidate.value).or_insert(Group {
            best: 0,
            origin: origin.clone(),
            positive: 0,
            shape: candidate.shape,
            skeleton,
        });
        if score > 0 {
            group.positive += 1;
        }
        if score > group.best {
            group.best = score;
            group.origin = origin;
        }
    };
    let body_doc = body.as_ref().map(|scan| &scan.doc);
    for (candidate, score) in body_found {
        let mut score = score;
        if score > 0 {
            if subject_values.contains(&candidate.value) {
                score += w.subject;
            }
            if other_values.contains(&candidate.value) {
                score += w.both_parts;
            }
        }
        let shape = body_doc.map_or_else(|| Skeleton::of(""), |doc| skeleton(doc, &candidate));
        add(candidate, clamp(score), part.clone(), shape);
    }
    let subject_doc = subject.as_ref().map(|scan| &scan.doc);
    for (candidate, score) in subject_found {
        let score = if score > 0 { score + w.subject } else { 0 };
        let shape = subject_doc.map_or_else(|| Skeleton::of(""), |doc| skeleton(doc, &candidate));
        add(candidate, clamp(score), PartRef::Subject, shape);
    }

    let mut ranked: Vec<(String, i32, Group)> = groups
        .into_iter()
        .map(|(value, group)| {
            let extra = (w.extra_occurrence * (group.positive - 1).max(0)).min(w.extra_cap);
            let total = if group.best > 0 {
                clamp(group.best + extra)
            } else {
                0
            };
            (value, total, group)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (rank, (_, total, group)) in ranked.iter().enumerate() {
        log.push(|| {
            Explanation::new("value")
                .part(group.origin.clone())
                .at(rank)
                .score(*total)
                .count(usize::try_from(group.positive).unwrap_or(0))
                .skeleton(group.skeleton.clone())
        });
    }
    let mut ranked = ranked.into_iter();
    let Some((value, score, group)) = ranked.next() else {
        log.push(|| Explanation::new("outcome:no_candidate"));
        return None;
    };
    let runner_up: Vec<_> = ranked.collect();
    let second = runner_up.first().map_or(0, |r| r.1);
    if score <= 0 {
        log.push(|| Explanation::new("outcome:all_negative"));
        return None;
    }
    if score - second < w.margin {
        log.push(|| Explanation::new("outcome:tie").score(score - second));
        return None;
    }
    let same_shape = runner_up
        .iter()
        .filter(|(v, s, g)| *s > 0 && g.shape == group.shape && v.len() == value.len())
        .count();
    if same_shape + 1 >= w.list_values {
        log.push(|| Explanation::new("outcome:list").count(same_shape + 1));
        return None;
    }
    if !fits_identifier(&value) {
        log.push(|| Explanation::new("outcome:not_an_identifier"));
        return None;
    }
    let score = u16::try_from(score.clamp(0, 999)).unwrap_or(0);
    let confidence = if i32::from(score) >= w.likely {
        Confidence::Likely
    } else {
        Confidence::Possible
    };
    log.push(|| {
        Explanation::new(match confidence {
            Confidence::Likely => "outcome:likely",
            _ => "outcome:possible",
        })
        .part(group.origin.clone())
        .score(i32::from(score))
        .skeleton(group.skeleton.clone())
    });
    Some(Insight {
        kind: InsightKind::Code {
            value,
            origin_bound: None,
        },
        provenance: Provenance::Heuristic,
        confidence,
        score,
        origin: group.origin,
        forwarded: false,
    })
}

fn clamp(score: i32) -> i32 {
    score.clamp(0, 1000)
}

fn positive_values(found: &[(Candidate, i32)]) -> BTreeSet<String> {
    found
        .iter()
        .filter(|(_, score)| *score > 0)
        .map(|(c, _)| c.value.clone())
        .collect()
}

fn listed(input: &InsightInput) -> bool {
    let list_id = input.header_values("List-Id").next().is_some();
    let automatic = input
        .header_values("Auto-Submitted")
        .any(|v| !v.trim().eq_ignore_ascii_case("no"));
    list_id && !automatic
}

fn bulk(data: &Data, input: &InsightInput) -> bool {
    let precedence = input.header_values("Precedence").any(|v| {
        let v = v.trim().to_ascii_lowercase();
        data.precedence.contains(&v)
    });
    let feedback = input.header_values("Feedback-ID").next().is_some();
    let esp = input.headers.iter().any(|h| {
        let name = h.name.to_ascii_lowercase();
        data.esp_prefixes
            .iter()
            .any(|p| name.starts_with(p.as_str()))
    });
    let unsubscribe = input.header_values("List-Unsubscribe").next().is_some();
    precedence || feedback || (esp && unsubscribe)
}

/// Every candidate of `scan` with its score before cross-part bonuses; `None` when there are too
/// many to be a code message.
fn scored(
    data: &Data,
    scan: &Scan,
    alphanumeric: bool,
    log: &mut Log,
) -> Option<Vec<(Candidate, i32)>> {
    let Some(found) = candidates(data, &scan.doc, alphanumeric, data.weights.max_occurrences)
    else {
        log.push(|| {
            Explanation::new("gate:too_many_candidates")
                .part(scan.part.part_ref())
                .count(data.weights.max_occurrences)
        });
        return None;
    };
    Some(
        found
            .into_iter()
            .map(|c| {
                let negative = rule(data, scan, &c);
                let score = match negative {
                    Some(_) => 0,
                    None => score(data, scan, &c),
                };
                log.push(|| {
                    let step = Explanation::new(negative.map_or("candidate", Rule::name))
                        .part(scan.part.part_ref())
                        .at(c.start)
                        .score(score)
                        .skeleton(skeleton(&scan.doc, &c));
                    let nearest =
                        scan.hits
                            .nearest(List::Code, c.start, c.end, data.weights.keyword_window);
                    match nearest.and_then(|(_, hit)| keyword_name(data, &scan.doc, hit)) {
                        Some(name) => step.keyword(name),
                        None => step,
                    }
                });
                (c, score)
            })
            .collect(),
    )
}

fn score(data: &Data, scan: &Scan, c: &Candidate) -> i32 {
    let w = &data.weights;
    let mut score = w.base;
    if let Some(d) = keyword_distance(data, scan, c) {
        let window = i32::try_from(w.keyword_window).unwrap_or(i32::MAX);
        let d = i32::try_from(d).unwrap_or(i32::MAX);
        if window > 0 && d < window {
            score += w.keyword * (window - d) / window;
        }
    }
    if scan.doc.emphasis(c.start) {
        score += w.emphasis;
    }
    if scan.doc.alone(c.start, c.end) {
        score += w.alone_on_line;
    }
    if scan.part != Part::Subject && c.start < w.early_chars {
        score += w.early;
    }
    if c.shape == Shape::Alphanumeric {
        score += w.alphanumeric;
    }
    if year_like(c) {
        score += w.year_like;
    }
    clamp(score)
}

fn year_like(c: &Candidate) -> bool {
    c.shape == Shape::Digits
        && c.value.len() == 4
        && c.value
            .parse::<u32>()
            .is_ok_and(|y| (1900..=2099).contains(&y))
}

fn keyword_distance(data: &Data, scan: &Scan, c: &Candidate) -> Option<usize> {
    scan.hits
        .distance(List::Code, c.start, c.end, data.weights.keyword_window)
}

fn is_ascii_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn candidates(data: &Data, doc: &Doc, alphanumeric: bool, max: usize) -> Option<Vec<Candidate>> {
    let mut out = Vec::new();
    let len = doc.len();
    let mut i = 0;
    while i < len {
        if !is_ascii_alnum(doc.at(i)) {
            i += 1;
            continue;
        }
        let start = i;
        while i < len && is_ascii_alnum(doc.at(i)) {
            i += 1;
        }
        let end = i;
        if end == len && doc.cut {
            break;
        }
        let run = end - start;
        let digits = (start..end).all(|j| doc.at(j).is_ascii_digit());
        let candidate = if digits && (4..=8).contains(&run) {
            Some(Candidate {
                start,
                end,
                value: value(doc, start, end),
                shape: Shape::Digits,
            })
        } else if digits && run == 3 {
            split(doc, start, end)
        } else if alphanumeric && (6..=8).contains(&run) && !digits {
            alnum(data, doc, start, end)
        } else {
            None
        };
        if let Some(candidate) = candidate {
            if out.len() >= max {
                return None;
            }
            out.push(candidate);
        }
    }
    Some(out)
}

fn value(doc: &Doc, start: usize, end: usize) -> String {
    doc.norm
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn split(doc: &Doc, start: usize, end: usize) -> Option<Candidate> {
    let sep = doc.at(end);
    if !matches!(sep, ' ' | '-') {
        return None;
    }
    let second = end + 1;
    let mut stop = second;
    while is_ascii_alnum(doc.at(stop)) {
        stop += 1;
    }
    if stop - second != 3 || !(second..stop).all(|j| doc.at(j).is_ascii_digit()) {
        return None;
    }
    if stop == doc.len() && doc.cut {
        return None;
    }
    let joined_before =
        start >= 2 && doc.before(start) == sep && doc.before(start - 1).is_ascii_digit();
    let joined_after = doc.at(stop) == sep && doc.at(stop + 1).is_ascii_digit();
    if joined_before || joined_after {
        return None;
    }
    Some(Candidate {
        start,
        end: stop,
        value: value(doc, start, stop),
        shape: Shape::Digits,
    })
}

fn alnum(data: &Data, doc: &Doc, start: usize, end: usize) -> Option<Candidate> {
    let folded: String = doc.fold.get(start..end)?.iter().collect();
    if !folded.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let hex = folded.chars().all(|c| c.is_ascii_hexdigit());
    if hex && doc.before(start) == '#' {
        return None;
    }
    if data.never.contains(&folded) {
        return None;
    }
    Some(Candidate {
        start,
        end,
        value: value(doc, start, end),
        shape: Shape::Alphanumeric,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenClass {
    Word,
    Cjk,
}

fn class(c: char) -> Option<TokenClass> {
    if is_cjk(c) {
        Some(TokenClass::Cjk)
    } else if c.is_alphanumeric() {
        Some(TokenClass::Word)
    } else {
        None
    }
}

/// The start of the span holding the `n` tokens before `at` on its line, with the separators
/// before them.
fn context_before(doc: &Doc, at: usize, n: usize) -> usize {
    let floor = at.saturating_sub(MAX_CONTEXT_CHARS);
    let mut pos = at;
    for _ in 0..n {
        let mut start = pos;
        while start > floor && class(doc.before(start)).is_none() {
            if doc.before(start) == '\n' {
                return start;
            }
            start -= 1;
        }
        let Some(kind) = class(doc.before(start)).filter(|_| start > floor) else {
            return start;
        };
        start -= 1;
        if kind == TokenClass::Word {
            while start > floor && class(doc.before(start)) == Some(kind) {
                start -= 1;
            }
        }
        pos = start;
    }
    while pos > floor && class(doc.before(pos)).is_none() && doc.before(pos) != '\n' {
        pos -= 1;
    }
    pos
}

/// The end of the span holding the `n` tokens after `at` on its line, with the separators after
/// them.
fn context_after(doc: &Doc, at: usize, n: usize) -> usize {
    let ceiling = (at + MAX_CONTEXT_CHARS).min(doc.len());
    let mut pos = at;
    for _ in 0..n {
        let mut end = pos;
        while end < ceiling && class(doc.at(end)).is_none() {
            if doc.at(end) == '\n' {
                return end;
            }
            end += 1;
        }
        let Some(kind) = class(doc.at(end)).filter(|_| end < ceiling) else {
            return end;
        };
        end += 1;
        if kind == TokenClass::Word {
            while end < ceiling && class(doc.at(end)) == Some(kind) {
                end += 1;
            }
        }
        pos = end;
    }
    while pos < ceiling && class(doc.at(pos)).is_none() && doc.at(pos) != '\n' {
        pos += 1;
    }
    pos
}

fn rule(data: &Data, scan: &Scan, c: &Candidate) -> Option<Rule> {
    let doc = &scan.doc;
    let hits = &scan.hits;
    let w = &data.weights;
    if doc.flag(c.start, FORWARD) {
        return Some(Rule::Forwarded);
    }
    if scan.footer_start.is_some_and(|f| c.start >= f) {
        return Some(Rule::Footer);
    }
    if (c.start..c.end).any(|i| doc.flag(i, URL)) {
        return Some(Rule::InUrl);
    }
    let before = context_before(doc, c.start, w.context_tokens);
    let after = context_after(doc, c.end, w.context_tokens);

    if c.shape == Shape::Digits && c.end - c.start == 4 {
        let year = year_like(c);
        let date_before = matches!(doc.before(c.start), '-' | '.' | '/')
            && doc.before(c.start.saturating_sub(1)).is_ascii_digit();
        let date_after =
            matches!(doc.at(c.end), '-' | '.' | '/') && doc.at(c.end + 1).is_ascii_digit();
        let month = hits
            .overlapping(List::Month, before, after)
            .next()
            .is_some();
        if year && (month || date_before || date_after) {
            return Some(Rule::YearNearMonth);
        }
    }

    if hits
        .overlapping(List::Money, before, after)
        .next()
        .is_some()
        || decimal(doc, c)
    {
        return Some(Rule::Money);
    }

    if c.shape == Shape::Digits && phone(doc, c) {
        return Some(Rule::Phone);
    }

    if hits
        .overlapping(List::Address, before, after)
        .next()
        .is_some()
    {
        return Some(Rule::Postcode);
    }

    let reference = hits
        .overlapping(List::Reference, before, c.start)
        .filter(|&(_, end)| end <= c.start)
        .any(|(_, end)| hits.overlapping(List::Code, end, c.start).next().is_none());
    if reference {
        return Some(Rule::ReferencePrefix);
    }

    if let Some(promo) = hits.distance(List::Promo, c.start, c.end, w.promo_window) {
        let code = keyword_distance(data, scan, c);
        if code.is_none_or(|d| promo <= d) {
            return Some(Rule::PromoKeyword);
        }
    }
    None
}

fn decimal(doc: &Doc, c: &Candidate) -> bool {
    let grouped_before = c.start >= 2
        && matches!(doc.before(c.start), '.' | ',')
        && doc.before(c.start - 1).is_ascii_digit();
    let fraction = matches!(doc.at(c.end), '.' | ',')
        && doc.at(c.end + 1).is_ascii_digit()
        && doc.at(c.end + 2).is_ascii_digit()
        && !doc.at(c.end + 3).is_ascii_digit();
    grouped_before || fraction
}

fn phone(doc: &Doc, c: &Candidate) -> bool {
    let joins = |ch: char| ch.is_ascii_digit() || matches!(ch, ' ' | '-' | '.' | '(' | ')' | '+');
    let mut start = c.start;
    let floor = c.start.saturating_sub(PHONE_REACH);
    while start > floor && joins(doc.before(start)) {
        start -= 1;
    }
    let mut end = c.end;
    let ceiling = (c.end + PHONE_REACH).min(doc.len());
    while end < ceiling && joins(doc.at(end)) {
        end += 1;
    }
    let plus = (start..c.start).any(|i| doc.at(i) == '+');
    let digits = (start..end).filter(|&i| doc.at(i).is_ascii_digit()).count();
    plus || digits >= PHONE_DIGITS
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::data::{DATA, Data};

    pub(crate) fn data() -> &'static Data {
        DATA.as_ref().unwrap()
    }

    fn rules(text: &str) -> Vec<(String, Option<Rule>)> {
        let data = data();
        let scan = Scan::new(Doc::text(text, None), data, Part::Text);
        candidates(data, &scan.doc, true, 100)
            .unwrap()
            .into_iter()
            .map(|c| {
                let rule = rule(data, &scan, &c);
                (c.value, rule)
            })
            .collect()
    }

    fn rule_of(text: &str, value: &str) -> Option<Rule> {
        rules(text)
            .into_iter()
            .find(|(v, _)| v == value)
            .unwrap_or_else(|| panic!("{value} is not a candidate in {text:?}"))
            .1
    }

    #[test]
    fn candidate_shapes() {
        let values: Vec<String> = rules(
            "code 1234 123-456 12345678 A1B2C3 abcdef 123456789 #a1b2c3 1234ab 验证码是654321",
        )
        .into_iter()
        .map(|(v, _)| v)
        .collect();
        assert_eq!(
            values,
            ["1234", "123456", "12345678", "A1B2C3", "1234ab", "654321"]
        );
        let values: Vec<String> = rules("12 345 678 and 123 456 789 and Code128")
            .into_iter()
            .map(|(v, _)| v)
            .collect();
        assert!(values.is_empty(), "{values:?}");
    }

    #[test]
    fn year_near_month() {
        assert_eq!(
            rule_of("code from November 2026 on", "2026"),
            Some(Rule::YearNearMonth)
        );
        assert_eq!(
            rule_of("code updated 2026-11-06", "2026"),
            Some(Rule::YearNearMonth)
        );
        assert_eq!(
            rule_of("код от 6 ноября 2026", "2026"),
            Some(Rule::YearNearMonth)
        );
        assert_eq!(
            rule_of("2026年11月 验证码", "2026"),
            Some(Rule::YearNearMonth)
        );
        assert_eq!(rule_of("Your code is 2026", "2026"), None);
    }

    #[test]
    fn money() {
        assert_eq!(
            rule_of("code: total $ 1250 today", "1250"),
            Some(Rule::Money)
        );
        assert_eq!(rule_of("code: total 4999 EUR", "4999"), Some(Rule::Money));
        assert_eq!(
            rule_of("code: total 1234.50 due", "1234"),
            Some(Rule::Money)
        );
        assert_eq!(rule_of("code: 1.2345 x", "2345"), Some(Rule::Money));
        assert_eq!(rule_of("您的验证码是123456，5分钟内有效", "123456"), None);
    }

    #[test]
    fn phone() {
        assert_eq!(rule_of("code: call +1 5550 100", "5550"), Some(Rule::Phone));
        assert_eq!(
            rule_of("code: call (555) 010-1234", "1234"),
            Some(Rule::Phone)
        );
        assert_eq!(
            rule_of("Your code is 482913 for 10 minutes", "482913"),
            None
        );
    }

    #[test]
    fn postcode() {
        assert_eq!(
            rule_of("code: 12 Example Street 94105 Springfield", "94105"),
            Some(Rule::Postcode)
        );
        assert_eq!(
            rule_of("Код: ул. Примерная, 1, индекс 123456", "123456"),
            Some(Rule::Postcode)
        );
    }

    #[test]
    fn reference_prefix() {
        assert_eq!(
            rule_of("code: Order #12345678 shipped", "12345678"),
            Some(Rule::ReferencePrefix)
        );
        assert_eq!(
            rule_of("code: Bestellung 4567890", "4567890"),
            Some(Rule::ReferencePrefix)
        );
        assert_eq!(
            rule_of("code: Заказ № 482913", "482913"),
            Some(Rule::ReferencePrefix)
        );
        assert_eq!(
            rule_of("code: Invoice no. 9081726", "9081726"),
            Some(Rule::ReferencePrefix)
        );
        assert_eq!(
            rule_of("Your Apple ID verification code: 482913", "482913"),
            None
        );
        assert_eq!(rule_of("Order verification code 482913", "482913"), None);
    }

    #[test]
    fn promo_keyword() {
        assert_eq!(
            rule_of("Use promo code SAVE2026 today", "SAVE2026"),
            Some(Rule::PromoKeyword)
        );
        assert_eq!(
            rule_of("Ваш промокод: 7734", "7734"),
            Some(Rule::PromoKeyword)
        );
        assert_eq!(
            rule_of("Dein Gutschein 55AB12 code", "55AB12"),
            Some(Rule::PromoKeyword)
        );
        assert_eq!(rule_of("优惠码 889900", "889900"), Some(Rule::PromoKeyword));
        assert_eq!(
            rule_of(
                "Your login code is 482913. Use promo code SAVE20 for 10% off.",
                "482913"
            ),
            None
        );
    }

    #[test]
    fn in_url() {
        assert_eq!(
            rule_of("code https://example.com/v?c=482913 x", "482913"),
            Some(Rule::InUrl)
        );
        assert_eq!(
            rule_of("code <a href=\"/v/482913\">", "482913"),
            Some(Rule::InUrl)
        );
    }

    #[test]
    fn footer_and_forwarded() {
        assert_eq!(
            rule_of("Your code 482913\n© 2026 Example Inc. 5566", "5566"),
            Some(Rule::Footer)
        );
        let data = data();
        let text = "See below\n--- Forwarded message ---\nYour code is 482913";
        let scan = Scan::new(Doc::text(text, Some(10)), data, Part::Text);
        let c = candidates(data, &scan.doc, true, 10)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(rule(data, &scan, &c), Some(Rule::Forwarded));
    }

    #[test]
    fn context_counts_cjk_chars_and_mixed_scripts_as_tokens() {
        assert_eq!(
            rule_of("ご注文番号：48201937", "48201937"),
            Some(Rule::ReferencePrefix)
        );
        assert_eq!(
            rule_of("주문번호 48201937 확인코드", "48201937"),
            Some(Rule::ReferencePrefix)
        );
        assert_eq!(
            rule_of("Sipariş numaranız: 4829173 kodu", "4829173"),
            Some(Rule::ReferencePrefix)
        );
        assert_eq!(
            rule_of("自2026年11月6日起，验证码", "2026"),
            Some(Rule::YearNearMonth)
        );
        assert_eq!(rule_of("Сумма: 4500 ₽\nкод", "4500"), Some(Rule::Money));
        assert_eq!(
            rule_of("Code: Musterstraße 12, 10115 Berlin", "10115"),
            Some(Rule::Postcode)
        );
        assert_eq!(
            rule_of("card ending in 4821, security code", "4821"),
            Some(Rule::ReferencePrefix)
        );
    }

    #[test]
    fn years_without_a_month_score_lower() {
        let data = data();
        let scan = Scan::new(
            Doc::text("Your code:\n2026\nYour code:\n7391", None),
            data,
            Part::Text,
        );
        let found = scored(data, &scan, true, &mut Log::off()).unwrap();
        let scores: Vec<i32> = found.iter().map(|(_, score)| *score).collect();
        assert!(matches!(scores.as_slice(), [year, pin] if year - pin == data.weights.year_like));
    }

    #[test]
    fn too_many_candidates_give_none() {
        let data = data();
        let doc = Doc::text(&"1234 ".repeat(20), None);
        assert!(candidates(data, &doc, true, 10).is_none());
    }

    #[test]
    fn cut_input_has_no_tail_candidate() {
        let mut text = "code ".to_string();
        text.push_str(&" ".repeat(holtmail_insight::limits::MAX_TEXT_BYTES - 7));
        text.push_str("12345678");
        let doc = Doc::text(&text, None);
        assert!(doc.cut);
        assert!(candidates(data(), &doc, true, 10).unwrap().is_empty());
    }
}
