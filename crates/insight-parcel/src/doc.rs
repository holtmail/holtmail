use holtmail_insight::limits::{MAX_SEGMENTS, MAX_TEXT_BYTES};
use holtmail_insight::{HtmlView, InsightInput, PartRef};
use unicode_normalization::UnicodeNormalization;

pub(crate) const FORWARD: u8 = 1;
pub(crate) const FOOTER: u8 = 2;

fn normalize(c: char, mut out: impl FnMut(char, char)) {
    if c.is_ascii() {
        out(c, c.to_ascii_lowercase());
        return;
    }
    for n in std::iter::once(c).nfkc() {
        let mut lower = n.to_lowercase();
        let folded = match (lower.next(), lower.next()) {
            (Some(single), None) => single,
            _ => n,
        };
        out(n, folded);
    }
}

pub(crate) fn fold(text: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        normalize(c, |_, folded| out.push(folded));
    }
    out
}

/// The body part candidates come from: NFKC chars, their lowercase form, and per char whether it
/// sits below a forward marker or in the footer.
#[derive(Default)]
pub(crate) struct Doc {
    pub norm: Vec<char>,
    pub fold: Vec<char>,
    pub flags: Vec<u8>,
    pub part: Option<PartRef>,
}

impl Doc {
    fn push(&mut self, c: char, flags: u8) {
        let Self {
            norm,
            fold,
            flags: all,
            ..
        } = self;
        normalize(c, |n, f| {
            norm.push(n);
            fold.push(f);
            all.push(flags);
        });
    }

    /// The HTML segments when the message has HTML, else the plain text.
    pub(crate) fn of(input: &InsightInput) -> Self {
        match &input.html {
            Some(view) => Self::segments(view),
            None => Self::text(&input.text, input.forward_at),
        }
    }

    fn text(text: &str, forward_at: Option<usize>) -> Self {
        let mut doc = Self {
            part: Some(PartRef::Text),
            ..Self::default()
        };
        for (n, (at, c)) in text.char_indices().enumerate() {
            if at + c.len_utf8() > MAX_TEXT_BYTES {
                break;
            }
            let flags = if forward_at.is_some_and(|f| n >= f) {
                FORWARD
            } else {
                0
            };
            doc.push(c, flags);
        }
        doc
    }

    fn segments(view: &HtmlView) -> Self {
        let mut doc = Self {
            part: Some(PartRef::Html),
            ..Self::default()
        };
        let mut bytes = 0usize;
        let mut footer = false;
        'segments: for (index, segment) in view.segments.iter().take(MAX_SEGMENTS).enumerate() {
            footer |= view.footer_at == Some(index);
            let mut flags = if footer { FOOTER } else { 0 };
            if segment.below_forward {
                flags |= FORWARD;
            }
            if index > 0 {
                doc.push('\n', flags);
            }
            for c in segment.text.chars() {
                bytes += c.len_utf8();
                if bytes > MAX_TEXT_BYTES {
                    break 'segments;
                }
                doc.push(c, flags);
            }
        }
        doc
    }

    pub(crate) fn len(&self) -> usize {
        self.norm.len()
    }

    pub(crate) fn at(&self, i: usize) -> char {
        self.norm.get(i).copied().unwrap_or('\0')
    }

    pub(crate) fn flagged(&self, start: usize, end: usize, flag: u8) -> bool {
        self.flags
            .get(start..end)
            .is_some_and(|f| f.iter().any(|f| f & flag != 0))
    }
}

pub(crate) fn is_cjk(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0E00..=0x0E7F
            | 0x1100..=0x11FF
            | 0x3040..=0x30FF
            | 0x3130..=0x318F
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFF66..=0xFF9F
            | 0x20000..=0x2FA1F
    )
}
