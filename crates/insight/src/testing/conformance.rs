//! The suite every [`InsightExtractor`] runs in its own tests.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::{Case, Expect, hostile_corpus, personal_mail};
use crate::limits::{
    MAX_DESTINATION_BYTES, MAX_DISPLAY_CHARS, MAX_ITEMS_PER_EXTRACTOR, MAX_URL_BYTES,
};
use crate::{
    Confidence, Explanation, Insight, InsightExtractor, InsightInput, InsightKind, Items, PartRef,
    Reservation, Url, UrlScheme, fits_identifier,
};

const MAX_URL_TOKENS: usize = 20_000;
const MAX_KEYWORD_CHARS: usize = 64;

/// Checks that `extractor` keeps the [`InsightExtractor`] contract. Panics on the first violation,
/// naming the case and the rule.
///
/// `cases` are the implementation's own inputs. The suite adds [`personal_mail`] (which must give
/// nothing) and [`hostile_corpus`], and checks for every input:
///
/// - `id()` is a non-empty lowercase name and `version()` is non-empty and never changes;
/// - `extract` does not panic, and two runs give byte-equal items JSON;
/// - at most 16 items; scores in `0..=1000`, and `1000` for `Certain`;
/// - every item round-trips through [`Items`] JSON;
/// - identifiers fit [`fits_identifier`], display text fits [`MAX_DISPLAY_CHARS`];
/// - URLs within their byte limits, `mailto` strings that parse as `mailto:` URLs, `https` and
///   `http` unsubscribe links of their own scheme, `one_click` only with an `https` link;
/// - attachment references that exist in the input;
/// - no identifier or URL is a prefix of a longer value in the input;
/// - the case's [`Expect`];
/// - `explain` does not panic, and its rule names, skeletons and keywords contain none of the
///   values, URLs or hosts the extractor returned for that input.
pub fn extractor(extractor: &dyn InsightExtractor, cases: &[Case]) {
    let id = extractor.id().to_owned();
    assert!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
        "id {id:?} is not a lowercase name"
    );
    let version = extractor.version();
    assert!(!version.is_empty(), "{id}: empty version");
    assert_eq!(extractor.version(), version, "{id}: version changed");

    for case in personal_mail() {
        run(extractor, &case);
    }
    for case in cases {
        run(extractor, case);
    }
    for case in hostile_corpus() {
        run(extractor, &case);
    }

    assert_eq!(extractor.version(), version, "{id}: version changed");
    assert_eq!(extractor.id(), id, "id changed");
}

fn run(extractor: &dyn InsightExtractor, case: &Case) {
    let name = format!("{}/{}", extractor.id(), case.name);
    let first = extract(extractor, &case.input, &name);
    let second = extract(extractor, &case.input, &name);
    let json = Items {
        items: first.clone(),
    }
    .to_json();
    assert_eq!(
        json,
        Items { items: second }.to_json(),
        "{name}: two runs differ"
    );
    check_output(&name, &case.input, &first, &json);
    match case.expect {
        Expect::Nothing => assert!(
            first.is_empty(),
            "{name}: expected no items, got {}",
            first.len()
        ),
        Expect::Found => assert!(!first.is_empty(), "{name}: expected items, got none"),
        Expect::Any => {}
    }
    let steps = match catch_unwind(AssertUnwindSafe(|| extractor.explain(&case.input))) {
        Ok(steps) => steps,
        Err(_) => panic!("{name}: explain panicked"),
    };
    check_explanation(&name, &first, &steps);
}

fn check_explanation(name: &str, items: &[Insight], steps: &[Explanation]) {
    let mut secrets: Vec<String> = Vec::new();
    for item in items {
        let fields = Fields::of(&item.kind);
        secrets.extend(fields.identifiers.iter().map(|v| (*v).to_owned()));
        secrets.extend(fields.display.iter().map(|v| (*v).to_owned()));
        for url in fields.urls {
            secrets.push(url.as_str().to_owned());
            secrets.extend(url.host().map(str::to_owned));
        }
        match &item.kind {
            InsightKind::Code {
                origin_bound: Some(origin),
                ..
            } => secrets.push(origin.clone()),
            InsightKind::Unsubscribe {
                mailto, list_id, ..
            } => {
                if let Some(mailto) = mailto {
                    secrets.push(mailto.clone());
                    let address = mailto.trim_start_matches("mailto:");
                    let address = address.split('?').next().unwrap_or(address);
                    secrets.push(address.to_owned());
                    secrets.extend(address.rsplit_once('@').map(|(_, host)| host.to_owned()));
                }
                secrets.extend(list_id.clone());
            }
            _ => {}
        }
    }
    secrets.retain(|s| !s.trim().is_empty());

    for (index, step) in steps.iter().enumerate() {
        let at = format!("{name}: explanation {index}");
        assert!(
            !step.rule.is_empty()
                && step.rule.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'_' | b':' | b'-')),
            "{at}: rule {:?} is not a fixed name",
            step.rule
        );
        if let Some(skeleton) = &step.skeleton {
            assert!(
                skeleton
                    .as_str()
                    .chars()
                    .all(|c| !c.is_alphanumeric() || c == 'D' || c == 'a'),
                "{at}: skeleton keeps content"
            );
        }
        for keyword in &step.keywords {
            assert!(
                keyword.chars().count() <= MAX_KEYWORD_CHARS,
                "{at}: keyword longer than {MAX_KEYWORD_CHARS} chars"
            );
        }
        let texts = std::iter::once(step.rule)
            .chain(step.skeleton.as_ref().map(|s| s.as_str()))
            .chain(step.keywords.iter().map(String::as_str));
        for text in texts {
            for secret in &secrets {
                assert!(
                    !text.contains(secret.as_str()),
                    "{at}: explanation contains a value of the output"
                );
            }
        }
    }
}

fn extract(extractor: &dyn InsightExtractor, input: &InsightInput, name: &str) -> Vec<Insight> {
    match catch_unwind(AssertUnwindSafe(|| extractor.extract(input))) {
        Ok(items) => items,
        Err(_) => panic!("{name}: extract panicked"),
    }
}

fn check_output(name: &str, input: &InsightInput, items: &[Insight], json: &str) {
    assert!(
        items.len() <= MAX_ITEMS_PER_EXTRACTOR,
        "{name}: {} items, more than {MAX_ITEMS_PER_EXTRACTOR}",
        items.len()
    );
    let read = Items::from_json(json).unwrap_or_else(|_| panic!("{name}: items JSON unreadable"));
    assert_eq!(read.skipped, 0, "{name}: an item does not read back");
    assert_eq!(read.items.items, items, "{name}: items change through JSON");

    let haystacks = haystacks(input);
    let url_tokens = url_tokens(&haystacks);
    for (index, item) in items.iter().enumerate() {
        let at = format!("{name}#{index}");
        assert!(item.score <= 1000, "{at}: score above 1000");
        if item.confidence == Confidence::Certain {
            assert_eq!(item.score, 1000, "{at}: Certain item without score 1000");
        }
        if let PartRef::Attachment(ordinal) = item.origin {
            assert!(
                input.attachments.iter().any(|a| a.ordinal == ordinal),
                "{at}: origin names a missing attachment"
            );
        }
        let fields = Fields::of(&item.kind);
        for value in &fields.identifiers {
            assert!(fits_identifier(value), "{at}: identifier out of bounds");
            assert!(
                !only_as_prefix(&haystacks, value, |c| c.is_ascii_alphanumeric()),
                "{at}: identifier is a prefix of a longer input value"
            );
        }
        for text in &fields.display {
            assert!(
                text.chars().count() <= MAX_DISPLAY_CHARS,
                "{at}: display text longer than {MAX_DISPLAY_CHARS} chars"
            );
        }
        for url in &fields.urls {
            assert!(url.as_str().len() <= MAX_URL_BYTES, "{at}: url too long");
            assert!(
                !truncated_url(&url_tokens, url),
                "{at}: url is a prefix of a longer input url"
            );
        }
        match &item.kind {
            InsightKind::Action {
                destination, url, ..
            } => {
                assert!(
                    destination.as_str().len() <= MAX_DESTINATION_BYTES,
                    "{at}: destination longer than {MAX_DESTINATION_BYTES} bytes"
                );
                assert_ne!(url.scheme(), UrlScheme::Mailto, "{at}: mailto action");
            }
            InsightKind::Unsubscribe {
                https,
                mailto,
                http,
                one_click,
                ..
            } => {
                if let Some(https) = https {
                    assert_eq!(https.scheme(), UrlScheme::Https, "{at}: https slot");
                }
                if let Some(http) = http {
                    assert_eq!(http.scheme(), UrlScheme::Http, "{at}: http slot");
                }
                if let Some(mailto) = mailto {
                    let parsed = Url::parse(mailto)
                        .unwrap_or_else(|_| panic!("{at}: mailto does not parse"));
                    assert_eq!(parsed.scheme(), UrlScheme::Mailto, "{at}: mailto slot");
                    assert!(
                        !truncated_url(&url_tokens, &parsed),
                        "{at}: mailto is a prefix of a longer input url"
                    );
                }
                assert!(
                    !one_click || https.is_some(),
                    "{at}: one_click without an https link"
                );
            }
            InsightKind::Pass { attachment, .. } => assert!(
                input.attachments.iter().any(|a| a.ordinal == *attachment),
                "{at}: pass names a missing attachment"
            ),
            _ => {}
        }
    }
}

#[derive(Default)]
struct Fields<'a> {
    identifiers: Vec<&'a str>,
    display: Vec<&'a str>,
    urls: Vec<&'a Url>,
}

impl<'a> Fields<'a> {
    fn of(kind: &'a InsightKind) -> Self {
        let mut f = Self::default();
        match kind {
            InsightKind::Code { value, .. } => f.identifiers.push(value),
            InsightKind::Action {
                url,
                destination,
                label,
                ..
            } => {
                f.urls.extend([url, destination]);
                f.display.extend(label.as_deref());
            }
            InsightKind::Unsubscribe { https, http, .. } => {
                f.urls.extend(https.iter().chain(http));
            }
            InsightKind::Reservation(reservation) => f.reservation(reservation),
            InsightKind::Order {
                number,
                merchant,
                status,
                url,
                ..
            } => {
                f.identifiers.extend(number.as_deref());
                f.display
                    .extend(merchant.as_deref().into_iter().chain(status.as_deref()));
                f.urls.extend(url);
            }
            InsightKind::Invoice {
                number, payee, url, ..
            } => {
                f.identifiers.extend(number.as_deref());
                f.display.extend(payee.as_deref());
                f.urls.extend(url);
            }
            InsightKind::Parcel {
                number, track_url, ..
            } => {
                f.identifiers.push(number);
                f.urls.extend(track_url);
            }
            InsightKind::Pass {
                organization,
                description,
                boarding,
                ..
            } => {
                f.display
                    .extend([organization.as_str(), description.as_str()]);
                if let Some(boarding) = boarding {
                    f.identifiers.extend(boarding.pnr.as_deref());
                    f.display.extend(boarding.passenger.as_deref());
                }
            }
        }
        f
    }

    fn reservation(&mut self, reservation: &'a Reservation) {
        let (number, url) = match reservation {
            Reservation::Flight(r) => {
                self.display.extend(r.passengers.iter().map(String::as_str));
                (r.reservation_number.as_deref(), &r.url)
            }
            Reservation::Lodging(r) => {
                self.display.extend([r.name.as_str()]);
                self.display.extend(r.address.as_deref());
                self.display.extend(r.guests.iter().map(String::as_str));
                (Some(r.reservation_number.as_str()), &r.url)
            }
            Reservation::Train(r) => {
                self.display.extend([r.departure_station.as_str()]);
                self.display.extend(r.arrival_station.as_deref());
                self.display.extend(r.passengers.iter().map(String::as_str));
                (r.reservation_number.as_deref(), &r.url)
            }
            Reservation::Bus(r) => {
                self.display.extend([r.departure_station.as_str()]);
                self.display.extend(r.arrival_station.as_deref());
                self.display.extend(r.passengers.iter().map(String::as_str));
                (r.reservation_number.as_deref(), &r.url)
            }
            Reservation::Event(r) => {
                self.display.extend([r.name.as_str()]);
                self.display.extend(r.venue.as_deref());
                (r.reservation_number.as_deref(), &r.url)
            }
            Reservation::Food(r) => {
                self.display.extend([r.name.as_str()]);
                (r.reservation_number.as_deref(), &r.url)
            }
            Reservation::RentalCar(r) => {
                self.display.extend(r.company.as_deref());
                self.display.extend(r.pickup_location.as_deref());
                self.display.extend(r.dropoff_location.as_deref());
                (Some(r.reservation_number.as_str()), &r.url)
            }
        };
        self.identifiers.extend(number);
        self.urls.extend(url);
    }
}

fn haystacks(input: &InsightInput) -> Vec<&str> {
    let mut out = vec![input.text.as_str()];
    out.extend(input.subject.as_deref());
    out.extend(input.headers.iter().map(|h| h.value.as_str()));
    if let Some(html) = &input.html {
        out.extend(html.segments.iter().map(|s| s.text.as_str()));
        for anchor in &html.anchors {
            out.push(&anchor.href);
            out.push(&anchor.text);
            out.extend(anchor.aria_label.as_deref());
        }
        out.extend(html.ld_json.iter().map(String::as_str));
    }
    out
}

/// True when `value` occurs in the input only as the start of a longer run of `continues` chars,
/// never on its own.
fn only_as_prefix(haystacks: &[&str], value: &str, continues: impl Fn(char) -> bool) -> bool {
    let mut as_prefix = false;
    for hay in haystacks {
        for (at, _) in hay.match_indices(value) {
            let before = hay[..at].chars().next_back();
            if before.is_some_and(&continues) {
                continue;
            }
            match hay[at + value.len()..].chars().next() {
                Some(next) if continues(next) => as_prefix = true,
                _ => return false,
            }
        }
    }
    as_prefix
}

fn url_tokens<'a>(haystacks: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    for hay in haystacks {
        for token in hay.split(|c: char| c.is_whitespace() || "<>\"'`,".contains(c)) {
            if out.len() >= MAX_URL_TOKENS {
                return out;
            }
            let lower = token.get(..7).map(str::to_ascii_lowercase);
            if lower
                .as_deref()
                .is_some_and(|l| l == "mailto:" || l.starts_with("http"))
            {
                out.push(token);
            }
        }
    }
    out
}

/// True when some input URL strictly extends `url` and no input URL reads as `url` itself.
fn truncated_url(tokens: &[&str], url: &Url) -> bool {
    let target = url.as_str();
    let extended = tokens
        .iter()
        .any(|token| token.len() > target.len() && token.starts_with(target));
    if !extended {
        return false;
    }
    let mut seen = HashSet::new();
    !tokens
        .iter()
        .filter(|token| seen.insert(**token))
        .any(|token| *token == target || Url::parse(token).is_ok_and(|parsed| parsed == *url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeaderField, LinkAction, Provenance};

    struct Nothing;

    impl InsightExtractor for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }

        fn version(&self) -> String {
            "1".into()
        }

        fn extract(&self, _: &InsightInput) -> Vec<Insight> {
            Vec::new()
        }
    }

    struct Fixed(Vec<Insight>);

    impl InsightExtractor for Fixed {
        fn id(&self) -> &str {
            "fixed"
        }

        fn version(&self) -> String {
            "1".into()
        }

        fn extract(&self, input: &InsightInput) -> Vec<Insight> {
            if input.subject.as_deref() == Some("Your code") {
                self.0.clone()
            } else {
                Vec::new()
            }
        }
    }

    struct Panics;

    impl InsightExtractor for Panics {
        fn id(&self) -> &str {
            "panics"
        }

        fn version(&self) -> String {
            "1".into()
        }

        fn extract(&self, input: &InsightInput) -> Vec<Insight> {
            let _ = &input.text[..1];
            Vec::new()
        }
    }

    fn code(value: &str) -> Insight {
        Insight {
            kind: InsightKind::Code {
                value: value.into(),
                origin_bound: None,
            },
            provenance: Provenance::Heuristic,
            confidence: Confidence::Likely,
            score: 800,
            origin: PartRef::Text,
            forwarded: false,
        }
    }

    fn case(text: &str) -> Case {
        Case::new(
            "code",
            InsightInput {
                subject: Some("Your code".into()),
                text: text.into(),
                headers: vec![HeaderField::new("Subject", "Your code")],
                ..InsightInput::default()
            },
            Expect::Found,
        )
    }

    struct Leaks;

    impl InsightExtractor for Leaks {
        fn id(&self) -> &str {
            "leaks"
        }

        fn version(&self) -> String {
            "1".into()
        }

        fn extract(&self, input: &InsightInput) -> Vec<Insight> {
            Fixed(vec![code("482913")]).extract(input)
        }

        fn explain(&self, input: &InsightInput) -> Vec<Explanation> {
            if input.subject.as_deref() == Some("Your code") {
                vec![Explanation::new("candidate").keyword("is 482913")]
            } else {
                Vec::new()
            }
        }
    }

    fn fails(extractor: &dyn InsightExtractor, cases: &[Case]) -> bool {
        catch_unwind(AssertUnwindSafe(|| super::extractor(extractor, cases))).is_err()
    }

    #[test]
    fn a_silent_extractor_passes() {
        super::extractor(&Nothing, &[]);
    }

    #[test]
    fn a_keeping_extractor_passes() {
        super::extractor(
            &Fixed(vec![code("482913")]),
            &[case("Your code is 482913.")],
        );
    }

    #[test]
    fn expectations_are_checked() {
        assert!(fails(&Nothing, &[case("Your code is 482913.")]));
    }

    #[test]
    fn panics_are_caught_and_reported() {
        assert!(fails(&Panics, &[]));
    }

    #[test]
    fn explanations_never_carry_values() {
        assert!(fails(&Leaks, &[case("Your code is 482913.")]));
    }

    #[test]
    fn caps_are_checked() {
        let cases = [case("Your code is 482913.")];
        assert!(fails(&Fixed(vec![code("482913"); 17]), &cases));
        assert!(fails(&Fixed(vec![code(&"9".repeat(65))]), &cases));
        let mut uncertain = code("482913");
        uncertain.confidence = Confidence::Certain;
        assert!(fails(&Fixed(vec![uncertain]), &cases));
    }

    #[test]
    fn cut_identifiers_are_caught() {
        assert!(fails(
            &Fixed(vec![code("482913")]),
            &[case("Your code is 48291377.")]
        ));
    }

    #[test]
    fn cut_urls_are_caught() {
        let url = Url::parse("https://example.com/verify").unwrap();
        let action = Insight {
            kind: InsightKind::Action {
                action: LinkAction::Verify,
                url: url.clone(),
                destination: url,
                via: None,
                tracked: false,
                text_host: None,
                label: None,
            },
            ..code("0")
        };
        let fixed = Fixed(vec![action]);
        super::extractor(&fixed, &[case("Open https://example.com/verify now")]);
        assert!(fails(
            &fixed,
            &[case("Open https://example.com/verify?t=1 now")]
        ));
    }

    #[test]
    fn prefix_rule_reads_tokens() {
        let hay = ["code 48291377 and 482913."];
        assert!(!only_as_prefix(&hay, "482913", |c| c.is_ascii_alphanumeric()));
        assert!(only_as_prefix(&["code 48291377"], "482913", |c| c.is_ascii_alphanumeric()));
        assert!(!only_as_prefix(&["验证码是482913"], "482913", |c| c
            .is_ascii_alphanumeric()));
    }
}
