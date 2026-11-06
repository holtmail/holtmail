//! Bounded scan of message HTML into visible anchors, text segments and structured-data blocks.
//!
//! [`scan`] turns a message's HTML into the [`HtmlView`](holtmail_insight::HtmlView) that insight
//! extractors read; [`forward_at`] finds the inline-forward marker in a plain-text body. Both are
//! deterministic, never panic and use bounded memory and stack on any input.

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod forward;
mod names;
mod scan;
mod style;

pub use forward::forward_at;
pub use scan::scan;

/// The version of the scan rules. It changes whenever the same HTML or text could give a
/// different [`HtmlView`](holtmail_insight::HtmlView) or [`forward_at`], so derived data built
/// from the scan can be recomputed.
pub const VERSION: &str = "1";
