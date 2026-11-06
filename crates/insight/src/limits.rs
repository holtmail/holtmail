//! The caps every party of the contract agrees on.
//!
//! The runner applies the input caps before calling extractors and sets the row's `truncated`
//! flag when one was hit. Extractors must still stay correct, and must not panic, on input above
//! these caps.

/// Items one extractor returns for one message, at most.
pub const MAX_ITEMS_PER_EXTRACTOR: usize = 16;
/// Chars in an identifier: a code, a reference, an order, invoice or tracking number. Longer
/// values are rejected, never truncated.
pub const MAX_IDENTIFIER_CHARS: usize = 64;
/// Chars in display text (labels, merchant, organisation, description), ellipsis included.
pub const MAX_DISPLAY_CHARS: usize = 512;
/// Bytes in a [`Url`](crate::Url), before and after normalisation.
pub const MAX_URL_BYTES: usize = 8 * 1024;
/// Bytes in an action's `destination` after unwrapping.
pub const MAX_DESTINATION_BYTES: usize = 2 * 1024;

/// Bytes of [`InsightInput::text`](crate::InsightInput::text).
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
/// Bytes of HTML scanned into an [`HtmlView`](crate::HtmlView).
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
/// Anchors in an [`HtmlView`](crate::HtmlView).
pub const MAX_ANCHORS: usize = 500;
/// `application/ld+json` blocks in an [`HtmlView`](crate::HtmlView).
pub const MAX_LD_JSON_BLOCKS: usize = 8;
/// Bytes of one `application/ld+json` block.
pub const MAX_LD_JSON_BYTES: usize = 256 * 1024;
/// Nesting depth of microdata items.
pub const MAX_MICRODATA_DEPTH: usize = 16;
/// Microdata properties, counted after `itemref` expansion.
pub const MAX_MICRODATA_PROPERTIES: usize = 4_000;
/// Text segments in an [`HtmlView`](crate::HtmlView).
pub const MAX_SEGMENTS: usize = 4_000;
/// Bytes of one pkpass attachment's content.
pub const MAX_PKPASS_BYTES: usize = 2 * 1024 * 1024;
/// Attachments that carry content.
pub const MAX_PKPASS_ATTACHMENTS: usize = 8;
