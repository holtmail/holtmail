use unicode_normalization::UnicodeNormalization;

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

/// NFKC and simple lowercase per char, at most `max` input chars; whitespace runs become one space.
pub(crate) fn fold_chars(text: &str, max: usize) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len().min(max));
    for c in text.chars().take(max) {
        if c.is_ascii() {
            push(&mut out, c.to_ascii_lowercase());
            continue;
        }
        for n in std::iter::once(c).nfkc() {
            let mut lower = n.to_lowercase();
            let folded = match (lower.next(), lower.next()) {
                (Some(single), None) => single,
                _ => n,
            };
            push(&mut out, folded);
        }
    }
    out
}

fn push(out: &mut Vec<char>, c: char) {
    let c = match c {
        '\u{2019}' | '\u{02bc}' => '\'',
        c if c.is_whitespace() => ' ',
        c => c,
    };
    if c == ' ' && out.last().is_none_or(|&last| last == ' ') {
        return;
    }
    out.push(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(text: &str) -> String {
        fold_chars(text, usize::MAX).into_iter().collect()
    }

    #[test]
    fn folds_width_case_and_spaces() {
        assert_eq!(fold("ＶＥＲＩＦＹ  Email"), "verify email");
        assert_eq!(fold("ПОДТВЕРДИТЬ"), "подтвердить");
        assert_eq!(fold("l\u{2019}adresse"), "l'adresse");
        assert_eq!(fold(" \u{a0}Sign\tin"), "sign in");
    }

    #[test]
    fn stops_at_the_cap() {
        assert_eq!(fold_chars("abcdef", 3), ['a', 'b', 'c']);
    }

    #[test]
    fn cjk_ranges() {
        assert!(is_cjk('验'));
        assert!(is_cjk('ロ'));
        assert!(is_cjk('로'));
        assert!(!is_cjk('a'));
        assert!(!is_cjk('д'));
    }
}
