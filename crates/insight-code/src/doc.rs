use holtmail_insight::HtmlView;
use holtmail_insight::limits::{MAX_SEGMENTS, MAX_TEXT_BYTES};
use unicode_normalization::UnicodeNormalization;

pub(crate) const FORWARD: u8 = 1;
pub(crate) const URL: u8 = 2;

const MAX_SUBJECT_BYTES: usize = 4 * 1024;
const MAX_ALONE_UNIT: usize = 256;
const MAX_URL_CHARS: usize = 8 * 1024;

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

fn normalize(c: char, mut out: impl FnMut(char, char)) {
    if c.is_ascii() || matches!(c, '№' | 'º') {
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

pub(crate) fn fold_chars(text: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        normalize(c, |_, folded| out.push(folded));
    }
    out
}

pub(crate) fn fold_str(text: &str) -> String {
    fold_chars(text).into_iter().collect()
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Unit {
    pub start: usize,
    pub end: usize,
    pub emphasis: bool,
    pub alone: bool,
}

/// One body part as matched: NFKC-normalised chars, their lowercase form and where each came from.
#[derive(Default)]
pub(crate) struct Doc {
    pub norm: Vec<char>,
    pub fold: Vec<char>,
    pub flags: Vec<u8>,
    unit_of: Vec<u32>,
    units: Vec<Unit>,
    /// The input was cut at a cap; the last char does not end a token.
    pub cut: bool,
    pub footer_start: Option<usize>,
}

impl Doc {
    fn push(&mut self, c: char, flags: u8) {
        let unit = u32::try_from(self.units.len().saturating_sub(1)).unwrap_or(u32::MAX);
        let Self {
            norm,
            fold,
            flags: all,
            unit_of,
            ..
        } = self;
        normalize(c, |n, f| {
            norm.push(n);
            fold.push(f);
            all.push(flags);
            unit_of.push(unit);
        });
    }

    fn open(&mut self, emphasis: bool, alone: bool) {
        let at = self.fold.len();
        self.units.push(Unit {
            start: at,
            end: at,
            emphasis,
            alone,
        });
    }

    fn close(&mut self) {
        let at = self.fold.len();
        if let Some(unit) = self.units.last_mut() {
            unit.end = at;
        }
    }

    /// The plain text body: each line is one unit, alone on its line.
    pub(crate) fn text(text: &str, forward_at: Option<usize>) -> Self {
        let mut doc = Self::default();
        doc.open(false, true);
        for (n, (at, c)) in text.char_indices().enumerate() {
            if at + c.len_utf8() > MAX_TEXT_BYTES {
                doc.cut = true;
                break;
            }
            let flags = if forward_at.is_some_and(|f| n >= f) {
                FORWARD
            } else {
                0
            };
            if c == '\n' {
                doc.close();
                doc.push(c, flags);
                doc.open(false, true);
            } else {
                doc.push(c, flags);
            }
        }
        doc.close();
        doc.mark_urls();
        doc
    }

    /// The HTML segments, joined by line breaks, up to [`MAX_TEXT_BYTES`] of visible text like the
    /// plain text body.
    pub(crate) fn segments(view: &HtmlView) -> Self {
        let mut doc = Self::default();
        let mut bytes = 0usize;
        'segments: for (index, segment) in view.segments.iter().take(MAX_SEGMENTS).enumerate() {
            if index > 0 {
                doc.push('\n', 0);
            }
            if view.footer_at == Some(index) {
                doc.footer_start = Some(doc.fold.len());
            }
            doc.open(segment.emphasis, segment.alone_on_line);
            let flags = if segment.below_forward { FORWARD } else { 0 };
            for c in segment.text.chars() {
                bytes += c.len_utf8();
                if bytes > MAX_TEXT_BYTES {
                    doc.cut = true;
                    doc.close();
                    break 'segments;
                }
                doc.push(c, flags);
            }
            doc.close();
        }
        doc.mark_urls();
        doc
    }

    /// A single line, such as the subject.
    pub(crate) fn line(text: &str) -> Self {
        let mut doc = Self::default();
        doc.open(false, false);
        for (at, c) in text.char_indices() {
            if at + c.len_utf8() > MAX_SUBJECT_BYTES {
                doc.cut = true;
                break;
            }
            doc.push(c, 0);
        }
        doc.close();
        doc.mark_urls();
        doc
    }

    pub(crate) fn len(&self) -> usize {
        self.fold.len()
    }

    /// The folded char at `i`, or NUL outside the doc.
    pub(crate) fn at(&self, i: usize) -> char {
        self.fold.get(i).copied().unwrap_or('\0')
    }

    /// The folded char before `i`, or NUL at the start.
    pub(crate) fn before(&self, i: usize) -> char {
        i.checked_sub(1).map_or('\0', |j| self.at(j))
    }

    pub(crate) fn flag(&self, i: usize, flag: u8) -> bool {
        self.flags.get(i).is_some_and(|f| f & flag != 0)
    }

    pub(crate) fn unit(&self, i: usize) -> Option<Unit> {
        let index = usize::try_from(*self.unit_of.get(i)?).ok()?;
        self.units.get(index).copied()
    }

    /// The unit holding `[start, end)` shows nothing else alphanumeric.
    pub(crate) fn alone(&self, start: usize, end: usize) -> bool {
        let Some(unit) = self.unit(start) else {
            return false;
        };
        if !unit.alone || unit.end - unit.start > MAX_ALONE_UNIT {
            return false;
        }
        (unit.start..unit.end)
            .filter(|&i| i < start || i >= end)
            .all(|i| !self.at(i).is_alphanumeric())
    }

    pub(crate) fn emphasis(&self, start: usize) -> bool {
        self.unit(start).is_some_and(|u| u.emphasis)
    }

    fn mark_urls(&mut self) {
        const OPENERS: [&str; 7] = [
            "https://", "http://", "www.", "mailto:", "href=", "src=", "style=",
        ];
        let openers: Vec<Vec<char>> = OPENERS.iter().map(|o| o.chars().collect()).collect();
        let mut i = 0;
        while i < self.fold.len() {
            let c = self.at(i);
            if !matches!(c, 'h' | 'w' | 'm' | 's') || self.before(i).is_alphanumeric() {
                i += 1;
                continue;
            }
            let Some(opener) = openers
                .iter()
                .find(|o| self.fold.get(i..i + o.len()) == Some(o.as_slice()))
            else {
                i += 1;
                continue;
            };
            let mut j = i + opener.len();
            let quote = self.at(j);
            let quoted = opener.last() == Some(&'=') && matches!(quote, '"' | '\'');
            if quoted {
                j += 1;
            }
            let limit = (j + MAX_URL_CHARS).min(self.fold.len());
            while j < limit {
                let c = self.at(j);
                let stop = if quoted {
                    c == quote
                } else {
                    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'')
                };
                if stop {
                    break;
                }
                j += 1;
            }
            for flag in self.flags.get_mut(i..j).into_iter().flatten() {
                *flag |= URL;
            }
            i = j.max(i + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_insight::Segment;

    #[test]
    fn fullwidth_digits_fold_to_ascii() {
        let doc = Doc::text("認証コード：８７６５４３", None);
        let text: String = doc.norm.iter().collect();
        assert_eq!(text, "認証コード:876543");
    }

    #[test]
    fn lines_are_units() {
        let doc = Doc::text("Your code:\n482913\nThanks", None);
        assert!(doc.alone(11, 17));
        assert!(!doc.alone(0, 4));
    }

    #[test]
    fn urls_are_marked() {
        let doc = Doc::text("go https://example.com/v?c=482913 now", None);
        assert!(doc.flag(30, URL));
        assert!(!doc.flag(35, URL));
        let doc = Doc::text("<td style=\"color: #a1b2c3\">", None);
        assert!(doc.flag(20, URL));
    }

    #[test]
    fn segments_keep_their_flags() {
        let view = HtmlView::new()
            .with_segments(vec![
                Segment {
                    text: "Code".into(),
                    ..Segment::default()
                },
                Segment {
                    text: "482913".into(),
                    emphasis: true,
                    alone_on_line: true,
                    below_forward: true,
                },
            ])
            .with_footer_at(Some(1));
        let doc = Doc::segments(&view);
        assert_eq!(doc.footer_start, Some(5));
        assert!(doc.emphasis(5));
        assert!(doc.alone(5, 11));
        assert!(doc.flag(6, FORWARD));
        assert!(!doc.flag(1, FORWARD));
    }
}
