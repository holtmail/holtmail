use holtmail_insight::limits::{MAX_ANCHORS, MAX_DISPLAY_CHARS, MAX_URL_BYTES};
use holtmail_insight::{
    Anchor, Confidence, Explanation, Insight, InsightInput, InsightKind, LinkAction, PartRef,
    Provenance, Url, UrlScheme, truncate_display,
};

use crate::data::Data;
use crate::fold::fold_chars;
use crate::href::{Href, host_in};

const MAX_OUTPUT: usize = 2;

pub(crate) struct Log {
    steps: Option<Vec<Explanation>>,
}

impl Log {
    pub(crate) fn off() -> Self {
        Self { steps: None }
    }

    pub(crate) fn on() -> Self {
        Self {
            steps: Some(Vec::new()),
        }
    }

    fn push(&mut self, step: impl FnOnce() -> Explanation) {
        if let Some(steps) = &mut self.steps {
            steps.push(step().part(PartRef::Html));
        }
    }

    pub(crate) fn into_steps(self) -> Vec<Explanation> {
        self.steps.unwrap_or_default()
    }
}

struct Candidate<'a> {
    anchor: &'a Anchor,
    position: usize,
    url: Url,
    action: LinkAction,
    score: u16,
}

fn at(anchor: &Anchor) -> usize {
    usize::try_from(anchor.index).unwrap_or(usize::MAX)
}

/// The action items of `input`, best first.
pub(crate) fn extract(data: &Data, input: &InsightInput, log: &mut Log) -> Vec<Insight> {
    let Some(html) = &input.html else {
        log.push(|| Explanation::new("no_html"));
        return Vec::new();
    };
    let w = &data.weights;
    let mut first_seen = false;
    let mut candidates = Vec::new();
    for (position, anchor) in html.anchors.iter().take(MAX_ANCHORS).enumerate() {
        let at = at(anchor);
        if anchor.below_forward {
            log.push(|| Explanation::new("forwarded").at(at));
            continue;
        }
        if anchor.href.len() > MAX_URL_BYTES {
            continue;
        }
        let Ok(url) = Url::parse(&anchor.href) else {
            continue;
        };
        if url.scheme() == UrlScheme::Mailto {
            log.push(|| Explanation::new("mailto").at(at).score(0));
            continue;
        }
        let href = Href::new(&anchor.href);
        if let Some(token) = href.skip(&data.skip) {
            log.push(|| {
                Explanation::new("unsubscribe")
                    .at(at)
                    .score(0)
                    .keyword(token)
            });
            continue;
        }
        if !url.fits_destination() {
            continue;
        }

        let label = data
            .labels
            .find(&fold_chars(&anchor.text, MAX_DISPLAY_CHARS));
        let aria = anchor
            .aria_label
            .as_deref()
            .and_then(|aria| data.labels.find(&fold_chars(aria, MAX_DISPLAY_CHARS)));
        let path = href.path_token(&data.paths);
        let wallet = data
            .wallet
            .iter()
            .any(|prefix| url.as_str().starts_with(prefix.as_str()));
        let action = if wallet {
            Some(LinkAction::WalletSave)
        } else {
            label
                .map(|l| l.action)
                .or(aria.map(|a| a.action))
                .or(path.and_then(|p| p.action))
        };
        let Some(action) = action else {
            continue;
        };

        let mut positive = 0;
        if let Some(label) = label {
            positive += w.label;
            log.push(|| {
                Explanation::new("label")
                    .at(at)
                    .score(w.label)
                    .keyword(label.keyword)
            });
        }
        if let Some(aria) = aria {
            positive += w.aria_label;
            log.push(|| {
                Explanation::new("aria_label")
                    .at(at)
                    .score(w.aria_label)
                    .keyword(aria.keyword)
            });
        }
        if anchor.button_like {
            positive += w.button_like;
            log.push(|| Explanation::new("button_like").at(at).score(w.button_like));
        }
        if let Some(path) = path {
            positive += w.path_token;
            log.push(|| {
                Explanation::new("path_token")
                    .at(at)
                    .score(w.path_token)
                    .keyword(path.keyword)
            });
        }
        if !first_seen {
            first_seen = true;
            positive += w.first;
            log.push(|| Explanation::new("first").at(at).score(w.first));
        }
        if wallet {
            positive = positive.max(w.wallet);
            log.push(|| Explanation::new("wallet").at(at).score(w.wallet));
        }
        let mut total = positive;
        if anchor.in_footer {
            total += w.in_footer;
            log.push(|| Explanation::new("footer").at(at).score(w.in_footer));
        }
        if let Some(domain) = data
            .social
            .iter()
            .find(|domain| href.hosts().any(|host| host_in(host, domain)))
        {
            total += w.social;
            log.push(|| {
                Explanation::new("social")
                    .at(at)
                    .score(w.social)
                    .keyword(domain.as_str())
            });
        }
        let score = u16::try_from(total.clamp(0, 1000)).unwrap_or(0);
        log.push(|| Explanation::new("score").at(at).score(i32::from(score)));
        candidates.push(Candidate {
            anchor,
            position,
            url,
            action,
            score,
        });
    }

    candidates.retain(|c| i32::from(c.score) >= w.likely);
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.anchor.index.cmp(&b.anchor.index))
            .then(a.position.cmp(&b.position))
    });
    let mut chosen: Vec<Candidate<'_>> = Vec::new();
    for candidate in candidates {
        if chosen.len() >= MAX_OUTPUT {
            break;
        }
        if chosen.iter().any(|c| c.url == candidate.url) {
            continue;
        }
        chosen.push(candidate);
    }
    log.push(|| Explanation::new("candidates").count(chosen.len()));
    chosen
        .into_iter()
        .map(|c| {
            log.push(|| {
                Explanation::new("chosen")
                    .at(at(c.anchor))
                    .score(i32::from(c.score))
            });
            item(c)
        })
        .collect()
}

fn item(c: Candidate<'_>) -> Insight {
    let text = c.anchor.text.trim();
    Insight {
        kind: InsightKind::Action {
            action: c.action,
            destination: c.url.clone(),
            url: c.url,
            via: None,
            tracked: false,
            text_host: text_host(text),
            label: (!text.is_empty()).then(|| truncate_display(text)),
        },
        provenance: Provenance::Heuristic,
        confidence: Confidence::Likely,
        score: c.score,
        origin: PartRef::Html,
        forwarded: false,
    }
}

/// The anchor text as an A-label host, when the whole text reads as a host or an http(s) URL.
pub(crate) fn text_host(text: &str) -> Option<String> {
    let text = text.trim();
    let lower = text.get(..8).map(str::to_ascii_lowercase);
    let bare = match lower.as_deref() {
        Some(l) if l.starts_with("https://") => text.get(8..)?,
        Some(l) if l.starts_with("http://") => text.get(7..)?,
        _ => text,
    };
    if bare.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (authority, _) = bare.split_at(bare.find(['/', '?', '#']).unwrap_or(bare.len()));
    let authority = match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            host
        }
        _ => authority,
    };
    let authority = authority.strip_suffix('.').unwrap_or(authority);
    if authority.is_empty()
        || authority.len() > 253 * 4
        || authority.chars().any(|c| "/\\?#@:%[]<>\"'`".contains(c))
        || !authority.contains('.')
    {
        return None;
    }
    let last = authority.rsplit('.').next()?;
    if last.is_empty() || last.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let url = Url::parse(&format!("https://{authority}/")).ok()?;
    let host = url.host()?;
    host.contains('.').then(|| host.to_owned())
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests;
