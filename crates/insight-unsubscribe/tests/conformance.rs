//! The unsubscribe extractor on input-level fixtures, and the shared conformance suite.

use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{
    Confidence, HeaderField, HtmlView, InsightExtractor, InsightInput, InsightKind, PartRef,
    Provenance, Segment,
};
use holtmail_insight_unsubscribe::UnsubscribeExtractor;

const LU: &str = "List-Unsubscribe";
const LUP: &str = "List-Unsubscribe-Post";
const ONE_CLICK: &str = "List-Unsubscribe=One-Click";
const LINKS: &str =
    "<mailto:leave@news.example.com?subject=unsubscribe>, <https://news.example.com/u/a1b2c3>";

#[derive(Debug, PartialEq, Eq)]
struct Out {
    https: Option<String>,
    mailto: Option<String>,
    http: Option<String>,
    one_click: bool,
    list_id: Option<String>,
}

fn h(name: &str, value: &str) -> HeaderField {
    HeaderField::new(name, value)
}

fn input(headers: Vec<HeaderField>) -> InsightInput {
    let mut all = vec![
        h("From", "Example News <news@news.example.com>"),
        h("To", "ada@example.org"),
        h("Subject", "This week at Example News"),
        h("List-Id", "Example News <weekly.news.example.com>"),
    ];
    all.extend(headers);
    InsightInput {
        headers: all,
        subject: Some("This week at Example News".into()),
        text: "Three stories from the invented newsroom.".into(),
        ..InsightInput::default()
    }
}

fn dkim(h_list: &str, selector: &str, extra: &str) -> HeaderField {
    h(
        "DKIM-Signature",
        &format!(
            "v=1; a=rsa-sha256; c=relaxed/relaxed; d=news.example.com; s={selector}; \
             h={h_list}{extra}; bh=YmFzZTY0Ym9keWhhc2g=; b=c2lnbmF0dXJlYnl0ZXNmb3J0ZXN0"
        ),
    )
}

fn ar(d: &str, selector: &str) -> HeaderField {
    h(
        "Authentication-Results",
        &format!("mx.example.org; dkim=pass header.d={d} header.s={selector} header.b=c2lnbmF0"),
    )
}

const H_BOTH: &str = "from:to:subject:date:list-unsubscribe:list-unsubscribe-post";

fn fixtures() -> Vec<(&'static str, InsightInput, Option<Out>)> {
    let one_click = |list_id: bool| Out {
        https: Some("https://news.example.com/u/a1b2c3".into()),
        mailto: Some("mailto:leave@news.example.com?subject=unsubscribe".into()),
        http: None,
        one_click: true,
        list_id: list_id.then(|| "weekly.news.example.com".into()),
    };
    vec![
        (
            "unsub-one-click-covered",
            input(vec![
                ar("news.example.com", "s1"),
                dkim(H_BOTH, "s1", ""),
                h(LU, LINKS),
                h(LUP, ONE_CLICK),
            ]),
            Some(one_click(true)),
        ),
        (
            "unsub-one-click-h-missing-post",
            input(vec![
                ar("news.example.com", "s1"),
                dkim("from:to:subject:date:list-unsubscribe", "s1", ""),
                h(LU, LINKS),
                h(LUP, ONE_CLICK),
            ]),
            Some(one_click(true)),
        ),
        (
            "unsub-one-click-other-selector",
            input(vec![
                ar("news.example.com", "s1"),
                dkim(H_BOTH, "s2", ""),
                h(LU, LINKS),
                h(LUP, ONE_CLICK),
            ]),
            Some(one_click(true)),
        ),
        (
            "unsub-one-click-unaligned",
            input(vec![
                ar("esp.example.org", "s1"),
                dkim(H_BOTH, "s1", ""),
                h(LU, LINKS),
                h(LUP, ONE_CLICK),
            ]),
            Some(one_click(true)),
        ),
        (
            "unsub-prepended-second-field",
            input(vec![
                h(LU, "<https://attacker.example.org/u/zz>"),
                ar("news.example.com", "s1"),
                dkim(H_BOTH, "s1", ""),
                h(LU, LINKS),
                h(LUP, ONE_CLICK),
            ]),
            Some(Out {
                https: Some("https://attacker.example.org/u/zz".into()),
                mailto: None,
                http: None,
                one_click: false,
                list_id: Some("weekly.news.example.com".into()),
            }),
        ),
        (
            "unsub-not-oversigned",
            input(vec![
                ar("news.example.com", "s1"),
                dkim(H_BOTH, "s1", ""),
                h(LU, LINKS),
                h(LUP, ONE_CLICK),
            ]),
            Some(one_click(true)),
        ),
        (
            "unsub-bad-item-stops",
            input(vec![h(
                LU,
                "<mailto:leave@news.example.com>, unsubscribe here, <https://news.example.com/u/x>",
            )]),
            Some(Out {
                https: None,
                mailto: Some("mailto:leave@news.example.com".into()),
                http: None,
                one_click: false,
                list_id: Some("weekly.news.example.com".into()),
            }),
        ),
        (
            "unsub-file-scheme",
            input(vec![
                h(
                    LU,
                    "<file:///home/ada/.ssh/id_ed25519>, <https://news.example.com/u/a1b2c3>",
                ),
                h(LUP, ONE_CLICK),
            ]),
            Some(Out {
                https: Some("https://news.example.com/u/a1b2c3".into()),
                mailto: None,
                http: None,
                one_click: true,
                list_id: Some("weekly.news.example.com".into()),
            }),
        ),
    ]
}

fn rules() -> Vec<(&'static str, InsightInput, Option<Out>)> {
    let only = |https: Option<&str>, mailto: Option<&str>, http: Option<&str>, one_click| Out {
        https: https.map(Into::into),
        mailto: mailto.map(Into::into),
        http: http.map(Into::into),
        one_click,
        list_id: Some("weekly.news.example.com".into()),
    };
    vec![
        ("no-list-unsubscribe", input(vec![h(LUP, ONE_CLICK)]), None),
        ("empty-field", input(vec![h(LU, "")]), None),
        (
            "only-refused-schemes",
            input(vec![h(
                LU,
                "<file:///etc/passwd>, <javascript:alert(1)>, <ftp://example.com/u>",
            )]),
            None,
        ),
        (
            "unbracketed-first",
            input(vec![
                h(LU, "https://example.org/u, <https://news.example.com/u>"),
                h(LUP, ONE_CLICK),
            ]),
            None,
        ),
        (
            "post-without-https",
            input(vec![
                h(
                    LU,
                    "<mailto:leave@news.example.com>, <http://news.example.com/u>",
                ),
                h(LUP, ONE_CLICK),
            ]),
            Some(only(
                None,
                Some("mailto:leave@news.example.com"),
                Some("http://news.example.com/u"),
                false,
            )),
        ),
        (
            "first-of-each-scheme",
            input(vec![h(
                LU,
                "<https://news.example.com/u/1>, <http://news.example.com/u/2>, \
                 <https://news.example.com/u/3>, <mailto:a@news.example.com>, \
                 <http://news.example.com/u/4>, <mailto:b@news.example.com>",
            )]),
            Some(only(
                Some("https://news.example.com/u/1"),
                Some("mailto:a@news.example.com"),
                Some("http://news.example.com/u/2"),
                false,
            )),
        ),
        (
            "no-post-field",
            input(vec![h(LU, "<https://news.example.com/u/1>")]),
            Some(only(
                Some("https://news.example.com/u/1"),
                None,
                None,
                false,
            )),
        ),
        (
            "post-wrong-case",
            input(vec![
                h(LU, "<https://news.example.com/u/1>"),
                h(LUP, "list-unsubscribe=one-click"),
            ]),
            Some(only(
                Some("https://news.example.com/u/1"),
                None,
                None,
                false,
            )),
        ),
        (
            "post-extra-text",
            input(vec![
                h(LU, "<https://news.example.com/u/1>"),
                h(LUP, "List-Unsubscribe=One-Click&x=1"),
            ]),
            Some(only(
                Some("https://news.example.com/u/1"),
                None,
                None,
                false,
            )),
        ),
        (
            "post-padded",
            input(vec![
                h(LU, "<https://news.example.com/u/1>"),
                h(LUP, "  List-Unsubscribe=One-Click  "),
            ]),
            Some(only(Some("https://news.example.com/u/1"), None, None, true)),
        ),
        (
            "two-post-fields",
            input(vec![
                h(LU, "<https://news.example.com/u/1>"),
                h(LUP, ONE_CLICK),
                h(LUP, ONE_CLICK),
            ]),
            Some(only(
                Some("https://news.example.com/u/1"),
                None,
                None,
                false,
            )),
        ),
        (
            "field-names-any-case",
            input(vec![
                h("list-unsubscribe", "<https://news.example.com/u/1>"),
                h("LIST-UNSUBSCRIBE-POST", ONE_CLICK),
            ]),
            Some(only(Some("https://news.example.com/u/1"), None, None, true)),
        ),
        (
            "whitespace-inside-brackets",
            input(vec![
                h(
                    LU,
                    "< https://news.example.com/u/ 1 > (one click), <mailto:a@news.example.com>",
                ),
                h(LUP, ONE_CLICK),
            ]),
            Some(only(
                Some("https://news.example.com/u/1"),
                Some("mailto:a@news.example.com"),
                None,
                true,
            )),
        ),
        (
            "userinfo-refused",
            input(vec![h(
                LU,
                "<https://ada:pw@news.example.com/u>, <mailto:a@news.example.com>",
            )]),
            Some(only(None, Some("mailto:a@news.example.com"), None, false)),
        ),
    ]
}

fn outcome(input: &InsightInput) -> Option<Out> {
    let items = UnsubscribeExtractor::new().extract(input);
    assert!(items.len() <= 1);
    let item = items.into_iter().next()?;
    assert_eq!(item.provenance, Provenance::Header);
    assert_eq!(item.confidence, Confidence::Certain);
    assert_eq!(item.score, 1000);
    assert_eq!(item.origin, PartRef::Header("List-Unsubscribe".into()));
    assert!(!item.forwarded);
    let InsightKind::Unsubscribe {
        https,
        mailto,
        http,
        one_click,
        list_id,
    } = item.kind
    else {
        panic!("not an unsubscribe item");
    };
    Some(Out {
        https: https.map(|u| u.as_str().to_owned()),
        mailto,
        http: http.map(|u| u.as_str().to_owned()),
        one_click,
        list_id,
    })
}

#[test]
fn fixtures_give_the_expected_links() {
    for (name, input, expected) in fixtures().into_iter().chain(rules()) {
        assert_eq!(outcome(&input), expected, "{name}");
    }
}

#[test]
fn list_id_is_optional_and_strict() {
    let mut message = input(vec![h(LU, "<https://news.example.com/u/1>")]);
    message.headers.retain(|f| f.name != "List-Id");
    assert_eq!(outcome(&message).and_then(|o| o.list_id), None);
    message
        .headers
        .push(h("List-Id", "weekly.news.example.com"));
    assert_eq!(outcome(&message).and_then(|o| o.list_id), None);
    message
        .headers
        .push(h("List-Id", "<second.news.example.com>"));
    assert_eq!(outcome(&message).and_then(|o| o.list_id), None);
}

#[test]
fn body_never_changes_the_output() {
    let base = input(vec![h(LU, LINKS), h(LUP, ONE_CLICK)]);
    let mut noisy = base.clone();
    noisy.text = "Unsubscribe: <https://evil.example.org/u>, List-Unsubscribe=One-Click".into();
    noisy.html = Some(HtmlView::new().with_segments(vec![Segment {
        text: "<https://evil.example.org/u>".into(),
        emphasis: false,
        alone_on_line: true,
        below_forward: false,
    }]));
    assert_eq!(outcome(&base), outcome(&noisy));
    assert!(outcome(&base).is_some());
}

#[test]
fn keeps_the_insight_contract() {
    let cases: Vec<Case> = fixtures()
        .into_iter()
        .chain(rules())
        .map(|(name, input, expected)| {
            let expect = if expected.is_some() {
                Expect::Found
            } else {
                Expect::Nothing
            };
            Case::new(name, input, expect)
        })
        .collect();
    conformance::extractor(&UnsubscribeExtractor::new(), &cases);
}
