//! Insight extractor for action links.
//!
//! [`LinksExtractor`] scores the visible anchors of the HTML body and returns at most two links
//! the reader can act on: verify, confirm, activate, reset a password, sign in, a magic link,
//! view an order, track a parcel, check in or save a pass to a wallet. Labels in twelve
//! languages, URL tokens, social hosts, skip tokens and weights live in `data/link-actions.toml`,
//! whose hash is part of the version. Unwrapping, tracker detection and trust flags belong to the
//! runner and the API, not to this crate.

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod data;
mod extract;
mod fold;
mod href;
mod matcher;

use holtmail_insight::{Explanation, Insight, InsightExtractor, InsightInput, InsightKind};

/// The [`InsightExtractor::id`] of [`LinksExtractor`].
pub const ID: &str = "links";

/// The scoring algorithm's version; the first part of [`LinksExtractor`]'s version.
pub const ALGORITHM: u32 = 1;

/// Finds action links in the HTML body.
///
/// Besides the [`InsightExtractor`] guarantees:
///
/// - **At most two items**, each an [`InsightKind::Action`] with [`Provenance::Heuristic`],
///   [`Confidence::Likely`] and origin [`PartRef::Html`], never forwarded. The second item has a
///   different `url` from the first.
/// - **Scored per anchor** from the embedded table: a label in the anchor text, a label in its
///   `aria-label`, a button look, a URL token, being the first anchor with an action, sitting in
///   the footer and pointing to a social host. Scores are clamped to `0..=1000`; an item needs at
///   least the table's `likely` weight. Highest score first, ties by anchor index.
/// - **Action choice.** A Google Wallet save URL is `wallet_save` whatever its text; otherwise
///   the text label wins, then the `aria-label`, then a URL token. An anchor with no action is
///   never returned.
/// - **Never an action:** `mailto` links, unsubscribe, preference and privacy URLs (also when
///   wrapped), URLs that do not parse as [`Url`](holtmail_insight::Url) or exceed the destination
///   limit, and anchors below an inline-forward marker. Only the first
///   [`MAX_ANCHORS`](holtmail_insight::limits::MAX_ANCHORS) anchors are read.
/// - **Fields.** `url` and `destination` are the href as parsed, `via` is `None` and `tracked` is
///   `false`: the runner unwraps and sets them. `text_host` is the host of the anchor text as an
///   A-label when the whole text reads as a host or an http(s) URL; `label` is the trimmed anchor
///   text, `None` when empty.
/// - **HTML only.** A message without an HTML body gives nothing.
///
/// [`Provenance::Heuristic`]: holtmail_insight::Provenance::Heuristic
/// [`Confidence::Likely`]: holtmail_insight::Confidence::Likely
/// [`PartRef::Html`]: holtmail_insight::PartRef::Html
#[derive(Debug, Clone, Copy, Default)]
pub struct LinksExtractor;

impl LinksExtractor {
    /// The extractor with the embedded table.
    pub fn new() -> Self {
        Self
    }
}

impl InsightExtractor for LinksExtractor {
    fn id(&self) -> &str {
        ID
    }

    fn version(&self) -> String {
        match data::DATA.as_ref() {
            Some(data) => data.version.clone(),
            None => format!("{ALGORITHM}+unreadable"),
        }
    }

    fn extract(&self, input: &InsightInput) -> Vec<Insight> {
        match data::DATA.as_ref() {
            Some(data) => extract::extract(data, input, &mut extract::Log::off()),
            None => Vec::new(),
        }
    }

    /// Every rule that fired per anchor (`label`, `aria_label`, `button_like`, `path_token`,
    /// `first`, `footer`, `social`, `unsubscribe`, `mailto`, `wallet`, `forwarded`), the anchor's
    /// `score`, the number of `candidates` kept and each `chosen` anchor. Positions are anchor
    /// indexes; keywords come only from the embedded table, and one that happens to contain a
    /// returned label, URL or host is left out.
    fn explain(&self, input: &InsightInput) -> Vec<Explanation> {
        let Some(data) = data::DATA.as_ref() else {
            return vec![Explanation::new("data_unreadable")];
        };
        let mut log = extract::Log::on();
        let items = extract::extract(data, input, &mut log);
        let mut secrets: Vec<&str> = Vec::new();
        for item in &items {
            if let InsightKind::Action {
                url,
                destination,
                text_host,
                label,
                ..
            } = &item.kind
            {
                secrets.extend([url.as_str(), destination.as_str()]);
                secrets.extend(url.host());
                secrets.extend(destination.host());
                secrets.extend(text_host.as_deref());
                secrets.extend(label.as_deref());
            }
        }
        secrets.retain(|s| !s.trim().is_empty());
        let mut steps = log.into_steps();
        for step in &mut steps {
            step.keywords
                .retain(|keyword| !secrets.iter().any(|s| keyword.contains(s)));
        }
        steps
    }
}
