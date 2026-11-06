//! Insight extractor for List-Unsubscribe and one-click unsubscribe (RFC 2369, RFC 8058).
//!
//! [`UnsubscribeExtractor`] reads header fields only. Whether a one-click request is covered by a
//! trusted DKIM signature is decided later, from the auth facts, and is not part of its output.

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod parse;

pub use parse::{UnsubscribeLinks, is_one_click_post, parse_list_id, parse_list_unsubscribe};

use holtmail_insight::{
    Confidence, Explanation, Insight, InsightExtractor, InsightInput, InsightKind, PartRef,
    Provenance, UrlScheme,
};
use parse::Step;

/// The extractor's [`InsightExtractor::id`].
pub const ID: &str = "unsubscribe";

/// The extractor's [`InsightExtractor::version`]. It changes whenever the output for some input
/// may change.
pub const VERSION: &str = "1";

/// The header field the links are read from.
pub const LIST_UNSUBSCRIBE: &str = "List-Unsubscribe";
/// The RFC 8058 header field that declares one-click.
pub const LIST_UNSUBSCRIBE_POST: &str = "List-Unsubscribe-Post";
/// The RFC 2919 list identifier field.
pub const LIST_ID: &str = "List-Id";

/// Finds the unsubscribe options a message declares in its header fields.
///
/// Guarantees, on top of the [`InsightExtractor`] contract:
///
/// - at most one item, an [`InsightKind::Unsubscribe`] with [`Provenance::Header`],
///   [`Confidence::Certain`], score 1000, origin `PartRef::Header("List-Unsubscribe")` and
///   `forwarded` false;
/// - an item only when the topmost `List-Unsubscribe` field gives at least one link, read by
///   [`parse_list_unsubscribe`]; later `List-Unsubscribe` fields are never read for links;
/// - `one_click` only when the message has exactly one `List-Unsubscribe` field, exactly one
///   `List-Unsubscribe-Post` field whose value passes [`is_one_click_post`], and an `https` link;
/// - `list_id` from the topmost `List-Id` field through [`parse_list_id`];
/// - only header fields are read; the body, HTML and attachments never change the output.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnsubscribeExtractor;

impl UnsubscribeExtractor {
    /// The extractor.
    pub fn new() -> Self {
        Self
    }
}

impl InsightExtractor for UnsubscribeExtractor {
    fn id(&self) -> &str {
        ID
    }

    fn version(&self) -> String {
        VERSION.to_owned()
    }

    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        run(input, &mut Log(None))
    }

    /// The field counts, each item of the first `List-Unsubscribe` field by position with the
    /// scheme it gave or why it was skipped, the `List-Unsubscribe-Post` and `List-Id` checks and
    /// why one-click is or is not declared.
    fn explain(&self, input: &InsightInput) -> Vec<Explanation> {
        let mut log = Log(Some(Vec::new()));
        run(input, &mut log);
        log.0.unwrap_or_default()
    }
}

struct Log(Option<Vec<Explanation>>);

impl Log {
    fn push(&mut self, step: impl FnOnce() -> Explanation) {
        if let Some(steps) = &mut self.0 {
            steps.push(step());
        }
    }
}

fn header(name: &str) -> PartRef {
    PartRef::Header(name.to_owned())
}

fn run(input: &InsightInput, log: &mut Log) -> Vec<Insight> {
    let mut first = None;
    let mut fields = 0usize;
    let mut post = None;
    let mut posts = 0usize;
    let mut list_id = None;
    for field in &input.headers {
        if field.name.eq_ignore_ascii_case(LIST_UNSUBSCRIBE) {
            fields += 1;
            first.get_or_insert(field.value.as_str());
        } else if field.name.eq_ignore_ascii_case(LIST_UNSUBSCRIBE_POST) {
            posts += 1;
            post.get_or_insert(field.value.as_str());
        } else if field.name.eq_ignore_ascii_case(LIST_ID) {
            list_id.get_or_insert(field.value.as_str());
        }
    }
    log.push(|| {
        Explanation::new("list_unsubscribe:fields")
            .part(header(LIST_UNSUBSCRIBE))
            .count(fields)
    });
    let Some(first) = first else {
        return Vec::new();
    };
    let links = parse::walk(first, |index, step| {
        log.push(|| {
            Explanation::new(match step {
                Step::Kept(UrlScheme::Https) => "item:https",
                Step::Kept(UrlScheme::Http) => "item:http",
                Step::Kept(UrlScheme::Mailto) => "item:mailto",
                Step::Duplicate(_) => "item:later_of_same_scheme",
                Step::Refused => "item:refused",
                Step::Unbracketed => "item:unbracketed_stop",
                Step::Unclosed => "item:unclosed_stop",
            })
            .part(header(LIST_UNSUBSCRIBE))
            .at(index)
        })
    });
    if links.is_empty() {
        log.push(|| Explanation::new("outcome:no_link"));
        return Vec::new();
    }
    let post_ok = post.is_some_and(is_one_click_post);
    log.push(|| {
        Explanation::new(match post {
            None => "post:absent",
            Some(_) if post_ok => "post:one_click",
            Some(_) => "post:other_value",
        })
        .part(header(LIST_UNSUBSCRIBE_POST))
        .count(posts)
    });
    let one_click = fields == 1 && posts == 1 && post_ok && links.https.is_some();
    if post.is_some() {
        for (withheld, rule) in [
            (fields != 1, "one_click:not_one_list_unsubscribe"),
            (posts != 1, "one_click:not_one_post"),
            (!post_ok, "one_click:post_value"),
            (links.https.is_none(), "one_click:no_https"),
        ] {
            if withheld {
                log.push(|| Explanation::new(rule));
            }
        }
    }
    let list_id = list_id.map(|value| (value, parse_list_id(value)));
    if let Some((_, parsed)) = &list_id {
        log.push(|| {
            Explanation::new(if parsed.is_some() {
                "list_id:valid"
            } else {
                "list_id:invalid"
            })
            .part(header(LIST_ID))
        });
    }
    log.push(|| {
        Explanation::new(if one_click {
            "outcome:one_click"
        } else {
            "outcome:links"
        })
    });
    vec![Insight {
        kind: InsightKind::Unsubscribe {
            https: links.https,
            mailto: links.mailto.map(|url| url.as_str().to_owned()),
            http: links.http,
            one_click,
            list_id: list_id.and_then(|(_, parsed)| parsed),
        },
        provenance: Provenance::Header,
        confidence: Confidence::Certain,
        score: 1000,
        origin: PartRef::Header(LIST_UNSUBSCRIBE.to_owned()),
        forwarded: false,
    }]
}
