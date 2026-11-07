use unicode_normalization::char::{canonical_combining_class, decompose_compatible};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};

const MAX_CHUNK_CHARS: usize = 32;
const MAX_ORIGINAL_FACTOR: usize = 4;

/// A text in NFKC with the way back to the text it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedText {
    /// The normalised text.
    pub text: String,
    /// Some of the original was left out to keep within the byte cap.
    pub truncated: bool,
    starts: Vec<(usize, usize)>,
}

impl NormalizedText {
    /// Normalises `original` to NFKC, keeping at most `max_bytes` bytes of output and cutting only
    /// where the original can be cut without changing how the kept part normalises.
    ///
    /// Deterministic. Never panics. Work is bounded by `max_bytes`, whatever the length of
    /// `original`.
    pub fn new(original: &str, max_bytes: usize) -> Self {
        let window_len = max_bytes.saturating_mul(MAX_ORIGINAL_FACTOR);
        let window = prefix(original, window_len);
        let mut out = Self {
            truncated: window.len() < original.len(),
            ..Self::default()
        };
        let mut chunk_start = 0;
        let mut chunk_chars = 0;
        for (at, c) in window.char_indices() {
            if at > chunk_start && (chunk_chars >= MAX_CHUNK_CHARS || stable(c)) {
                if !out.push(&window[chunk_start..at], chunk_start, max_bytes) {
                    out.truncated = true;
                    return out;
                }
                chunk_start = at;
                chunk_chars = 0;
            }
            chunk_chars += 1;
        }
        if chunk_start < window.len() && !out.push(&window[chunk_start..], chunk_start, max_bytes) {
            out.truncated = true;
        }
        out
    }

    /// The byte offset in the original text where the normalised text at byte `offset` came
    /// from: the start of the run of original text it was normalised from. Offsets past the end
    /// map to the end of the last kept run.
    pub fn original_offset(&self, offset: usize) -> usize {
        match self.starts.partition_point(|&(normal, _)| normal <= offset) {
            0 => 0,
            n => self.starts[n - 1].1,
        }
    }

    fn push(&mut self, chunk: &str, original_at: usize, max_bytes: usize) -> bool {
        let normal = if chunk.is_ascii() || is_nfkc_quick(chunk.chars()) == IsNormalized::Yes {
            std::borrow::Cow::Borrowed(chunk)
        } else {
            std::borrow::Cow::Owned(chunk.nfkc().collect())
        };
        if self.text.len() + normal.len() > max_bytes {
            return false;
        }
        self.starts.push((self.text.len(), original_at));
        self.text.push_str(&normal);
        true
    }
}

fn stable(c: char) -> bool {
    if c.is_ascii() {
        return true;
    }
    let mut first = None;
    decompose_compatible(c, |d| {
        first.get_or_insert(d);
    });
    canonical_combining_class(c) == 0
        && first.is_some_and(|d| {
            canonical_combining_class(d) == 0
                && is_nfkc_quick(std::iter::once(d)) == IsNormalized::Yes
        })
}

pub(crate) fn prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_compatibility_forms() {
        let text = NormalizedText::new("Code: \u{FF14}\u{FF18}\u{FF12}\u{FF19} \u{FB01}le", 1024);
        assert_eq!(text.text, "Code: 4829 file");
        assert!(!text.truncated);
    }

    #[test]
    fn composes_across_marks_like_the_whole_text() {
        for original in [
            "Cafe\u{301} cre\u{300}me",
            "\u{FF76}\u{FF9E}\u{FF77}\u{FF9E}",
            "\u{1100}\u{1161}\u{11A8}",
            "plain ascii text",
        ] {
            let whole: String = original.nfkc().collect();
            assert_eq!(
                NormalizedText::new(original, 1024).text,
                whole,
                "{original}"
            );
        }
    }

    #[test]
    fn offsets_lead_back_to_the_original() {
        let original = "\u{FF21}\u{FF22} 123";
        let text = NormalizedText::new(original, 1024);
        assert_eq!(text.text, "AB 123");
        let at = text.text.find('1').unwrap();
        assert_eq!(&original[text.original_offset(at)..], "123");
        let b = text.text.find('B').unwrap();
        assert_eq!(&original[text.original_offset(b)..], "\u{FF22} 123");
        assert_eq!(text.original_offset(0), 0);
    }

    #[test]
    fn caps_output_on_a_chunk_boundary() {
        let original = "é".repeat(1000);
        let text = NormalizedText::new(&original, 101);
        assert!(text.truncated);
        assert!(text.text.len() <= 101);
        assert_eq!(text.text, "é".repeat(50));
    }

    #[test]
    fn long_runs_of_marks_stay_bounded() {
        let original = format!("a{}", "\u{301}".repeat(100_000));
        let text = NormalizedText::new(&original, 1024);
        assert!(text.truncated);
        assert!(text.text.len() <= 1024);
    }

    #[test]
    fn expanding_chars_do_not_overflow_the_cap() {
        let original = "\u{FDFA}".repeat(10_000);
        let text = NormalizedText::new(&original, 4096);
        assert!(text.truncated);
        assert!(text.text.len() <= 4096);
    }
}
