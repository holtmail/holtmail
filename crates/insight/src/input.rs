use bytes::Bytes;
use holtmail_core::{Address, Timestamp};

/// What an extractor sees of one message.
///
/// The runner builds it once per message within the caps of [`crate::limits`]; it mirrors the
/// parsed message without exposing the parser's types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InsightInput {
    /// Every header field of the message, topmost first, repeated fields repeated.
    pub headers: Vec<HeaderField>,
    /// `From` mailboxes.
    pub from: Vec<Address>,
    /// `Reply-To` mailboxes.
    pub reply_to: Vec<Address>,
    /// Decoded `Subject`.
    pub subject: Option<String>,
    /// `Date` of the message; the only "now" an extractor may use.
    pub sent_at: Option<Timestamp>,
    /// The plain text body, NFKC-normalised for matching, at most [`MAX_TEXT_BYTES`] bytes; empty
    /// when the message has none.
    ///
    /// [`MAX_TEXT_BYTES`]: crate::limits::MAX_TEXT_BYTES
    pub text: String,
    /// The char offset in [`text`](Self::text) of the first inline-forward marker. Content at or
    /// after it belongs to a forwarded message.
    pub forward_at: Option<usize>,
    /// The scanned HTML body; `None` when the message has no HTML part.
    pub html: Option<HtmlView>,
    /// Every attachment, in document order.
    pub attachments: Vec<AttachmentMeta>,
}

impl InsightInput {
    /// The values of the header fields named `name` (any letter case), topmost first.
    pub fn header_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |field| field.name.eq_ignore_ascii_case(name))
            .map(|field| field.value.as_str())
    }
}

/// One header field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderField {
    /// The name as written, letter case kept.
    pub name: String,
    /// The value unfolded into one line, encoded words decoded, without surrounding whitespace.
    pub value: String,
}

impl HeaderField {
    /// A field from its name and value.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// The visible parts of an HTML body, as the scanner reports them.
///
/// Hidden content (`hidden`, `aria-hidden`, `display:none` and the like, `<template>`,
/// `<noscript>`, and whatever the sanitizer strips) is already gone from anchors and segments.
/// JSON-LD and microdata are metadata: they are kept inside CSS- or `aria`-hidden subtrees, never
/// inside `<template>`, foreign content or what the sanitizer strips. Script, style and other
/// raw-text elements never contribute segments. Lists are in document order and within the caps of
/// [`crate::limits`].
///
/// New fields may be added; build one with [`HtmlView::new`] and the `with_*` methods, or set the
/// public fields of a default value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct HtmlView {
    /// Visible `<a href>` elements.
    pub anchors: Vec<Anchor>,
    /// The text of each `<script type="application/ld+json">` block, unparsed.
    pub ld_json: Vec<String>,
    /// The index in [`ld_json`](Self::ld_json) of the first block found below an inline-forward
    /// marker; `None` when no block is. Blocks are in document order, so every block from this
    /// index on is below the marker and every block before it is above.
    pub ld_json_forward_at: Option<usize>,
    /// Microdata items, flat. Nested items are referenced by index, so the list itself never
    /// recurses.
    pub microdata: Vec<MicrodataItem>,
    /// Visible text runs.
    pub segments: Vec<Segment>,
    /// The index in [`segments`](Self::segments) of the first segment at or after the footer
    /// marker (an unsubscribe anchor or `©`); `None` when there is no footer.
    pub footer_at: Option<usize>,
    /// A cap on the structured data was hit while building the view: a JSON-LD block was
    /// dropped for its size or count, or microdata items or properties were left out by the depth,
    /// item or property caps of [`crate::limits`]. `false` guarantees that `ld_json` and
    /// `microdata` hold everything the scanner reads; the lengths of the lists alone do not tell.
    pub truncated_structured: bool,
}

impl HtmlView {
    /// An empty view.
    pub fn new() -> Self {
        Self::default()
    }

    /// The view with these anchors.
    pub fn with_anchors(mut self, anchors: Vec<Anchor>) -> Self {
        self.anchors = anchors;
        self
    }

    /// The view with these JSON-LD blocks.
    pub fn with_ld_json(mut self, ld_json: Vec<String>) -> Self {
        self.ld_json = ld_json;
        self
    }

    /// The view with this position of the first JSON-LD block below a forward marker.
    pub fn with_ld_json_forward_at(mut self, ld_json_forward_at: Option<usize>) -> Self {
        self.ld_json_forward_at = ld_json_forward_at;
        self
    }

    /// The view with these microdata items.
    pub fn with_microdata(mut self, microdata: Vec<MicrodataItem>) -> Self {
        self.microdata = microdata;
        self
    }

    /// The view with these segments.
    pub fn with_segments(mut self, segments: Vec<Segment>) -> Self {
        self.segments = segments;
        self
    }

    /// The view with this footer position.
    pub fn with_footer_at(mut self, footer_at: Option<usize>) -> Self {
        self.footer_at = footer_at;
        self
    }

    /// The view with this structured-data truncation flag.
    pub fn with_truncated_structured(mut self, truncated_structured: bool) -> Self {
        self.truncated_structured = truncated_structured;
        self
    }
}

/// One visible link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Anchor {
    /// The `href` attribute as written, entities decoded, not validated.
    pub href: String,
    /// The visible text inside the anchor, whitespace collapsed.
    pub text: String,
    /// The `aria-label` attribute, when present.
    pub aria_label: Option<String>,
    /// The anchor looks like a button: a background cell, padding or `role=button`.
    pub button_like: bool,
    /// The anchor sits at or after the footer marker.
    pub in_footer: bool,
    /// The anchor sits below an inline-forward marker.
    pub below_forward: bool,
    /// The anchor's position among all anchors of the body, counting from 0.
    pub index: u32,
}

/// One run of visible text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segment {
    /// The text, whitespace collapsed.
    pub text: String,
    /// Shown with emphasis: `strong`, `b`, `h1`–`h3`, letter spacing or a large font.
    pub emphasis: bool,
    /// The only text on its visual line.
    pub alone_on_line: bool,
    /// Below an inline-forward marker.
    pub below_forward: bool,
}

/// One microdata item (`itemscope`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MicrodataItem {
    /// The `itemtype` values as written.
    pub types: Vec<String>,
    /// The `itemid`, when present.
    pub id: Option<String>,
    /// Properties in document order, `itemref` targets included.
    pub properties: Vec<MicrodataProperty>,
    /// The item is not itself the value of a property.
    pub top_level: bool,
    /// The `itemscope` element sits below an inline-forward marker.
    pub below_forward: bool,
}

impl MicrodataItem {
    /// An item with these types and nothing else.
    pub fn new(types: Vec<String>) -> Self {
        Self {
            types,
            ..Self::default()
        }
    }
}

/// One microdata property (`itemprop`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrodataProperty {
    /// The property name.
    pub name: String,
    /// Its value.
    pub value: MicrodataValue,
}

/// The value of a microdata property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicrodataValue {
    /// Text content with whitespace collapsed, or a `content`, `value` or `datetime` attribute.
    Text(String),
    /// An `href` or `src` attribute, as written.
    Url(String),
    /// A nested item: its index in [`HtmlView::microdata`]. The index may point anywhere,
    /// including back to an ancestor; readers must bound their walk.
    Item(usize),
}

/// One attachment of the message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentMeta {
    /// The attachment's position among all attachments, counting from 0; what
    /// [`PartRef::Attachment`](crate::PartRef::Attachment) refers to.
    pub ordinal: u16,
    /// The file name, decoded.
    pub filename: Option<String>,
    /// Lowercase media type, e.g. `application/vnd.apple.pkpass`.
    pub content_type: String,
    /// Decoded size in bytes.
    pub size: u64,
    /// The decoded content, present only for pkpass attachments within the caps of
    /// [`crate::limits`].
    pub content: Option<Bytes>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_values_ignore_case_and_keep_order() {
        let input = InsightInput {
            headers: vec![
                HeaderField::new("List-Unsubscribe", "<https://example.com/a>"),
                HeaderField::new("Subject", "Hello"),
                HeaderField::new("list-unsubscribe", "<https://example.com/b>"),
            ],
            ..InsightInput::default()
        };
        let values: Vec<_> = input.header_values("LIST-UNSUBSCRIBE").collect();
        assert_eq!(
            values,
            ["<https://example.com/a>", "<https://example.com/b>"]
        );
    }

    #[test]
    fn html_view_builds_from_outside() {
        let view = HtmlView::new()
            .with_segments(vec![Segment {
                text: "Hello".into(),
                ..Segment::default()
            }])
            .with_footer_at(Some(0));
        assert_eq!(view.segments.len(), 1);
        assert_eq!(view.footer_at, Some(0));
    }

    #[test]
    fn html_view_builds_structured_fields() {
        let mut item = MicrodataItem::new(vec!["https://schema.org/Order".into()]);
        item.below_forward = true;
        let view = HtmlView::new()
            .with_ld_json(vec!["{}".into(), "{}".into()])
            .with_ld_json_forward_at(Some(1))
            .with_microdata(vec![item])
            .with_truncated_structured(true);
        assert_eq!(view.ld_json_forward_at, Some(1));
        assert!(view.microdata[0].below_forward);
        assert!(view.truncated_structured);
        let empty = HtmlView::new();
        assert_eq!(empty.ld_json_forward_at, None);
        assert!(!empty.truncated_structured);
    }
}
