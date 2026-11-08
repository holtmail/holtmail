const PREFIXES: &[&str] = &[
    "re",
    "aw",
    "sv",
    "antw",
    "ответ",
    "отв",
    "fwd",
    "fw",
    "wg",
    "tr",
    "enc",
];

/// The subject without leading reply and forward prefixes.
///
/// Recognises, in any letter case and repeated in any order: `Re`, `AW`, `SV`, `Antw`, `Ответ`,
/// `Отв`, `Fwd`, `FW`, `WG`, `TR` and `Enc`, each followed by a colon, optionally with a bracketed
/// or parenthesized counter (`Re[2]:`, `Re(3):`) or a caret counter (`Re^2:`), and whitespace in
/// between. A word without its colon is kept: `Reunion` and `Re Ada` are not prefixes.
pub fn strip_prefixes(subject: &str) -> &str {
    let mut rest = subject.trim();
    while let Some(after) = strip_one(rest) {
        rest = after.trim_start();
    }
    rest.trim_end()
}

/// The subject of a reply: exactly one `Re: ` before the subject without prefixes.
pub fn reply_subject(subject: &str) -> String {
    prefixed("Re:", subject)
}

/// The subject of a forward: exactly one `Fwd: ` before the subject without prefixes.
pub fn forward_subject(subject: &str) -> String {
    prefixed("Fwd:", subject)
}

fn prefixed(prefix: &str, subject: &str) -> String {
    let rest = strip_prefixes(subject);
    if rest.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix} {rest}")
    }
}

fn strip_one(text: &str) -> Option<&str> {
    let word_end = text
        .char_indices()
        .find(|(_, c)| !c.is_alphabetic())
        .map_or(text.len(), |(i, _)| i);
    let word = text[..word_end].to_lowercase();
    if !PREFIXES.contains(&word.as_str()) {
        return None;
    }
    let mut rest = text[word_end..].trim_start();
    rest = counter(rest).unwrap_or(rest).trim_start();
    rest.strip_prefix(':').or_else(|| rest.strip_prefix('：'))
}

fn counter(text: &str) -> Option<&str> {
    let (open, close) = match text.chars().next()? {
        '[' => ('[', ']'),
        '(' => ('(', ')'),
        '^' => {
            let digits = text[1..].bytes().take_while(u8::is_ascii_digit).count();
            return (digits > 0).then(|| &text[1 + digits..]);
        }
        _ => return None,
    };
    let inner_end = text.find(close)?;
    let inner = text[open.len_utf8()..inner_end].trim();
    (!inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()))
        .then(|| &text[inner_end + close.len_utf8()..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn strips_known_prefixes() {
        for (subject, expected) in [
            ("Re: Lunch", "Lunch"),
            ("RE: Lunch", "Lunch"),
            ("re:Lunch", "Lunch"),
            ("AW: Lunch", "Lunch"),
            ("Ответ: Обед", "Обед"),
            ("Fwd: Lunch", "Lunch"),
            ("FW: Lunch", "Lunch"),
            ("WG: Lunch", "Lunch"),
            ("Re[2]: Lunch", "Lunch"),
            ("Re [3] : Lunch", "Lunch"),
            ("Re(4): Lunch", "Lunch"),
            ("Re^2: Lunch", "Lunch"),
            ("Re: Fwd: RE: AW: Lunch", "Lunch"),
            ("  Re:   Lunch  ", "Lunch"),
            ("Reunion", "Reunion"),
            ("Re Lunch", "Re Lunch"),
            ("Re[x]: Lunch", "Re[x]: Lunch"),
            ("[list] Re: Lunch", "[list] Re: Lunch"),
            ("Lunch: Re: soup", "Lunch: Re: soup"),
            ("Re:", ""),
            ("", ""),
        ] {
            assert_eq!(strip_prefixes(subject), expected, "{subject}");
        }
    }

    #[test]
    fn adds_exactly_one_prefix() {
        assert_eq!(reply_subject("Lunch"), "Re: Lunch");
        assert_eq!(reply_subject("Re: Re[2]: Lunch"), "Re: Lunch");
        assert_eq!(reply_subject("Fwd: Lunch"), "Re: Lunch");
        assert_eq!(forward_subject("Re: Lunch"), "Fwd: Lunch");
        assert_eq!(reply_subject(""), "Re:");
    }

    fn prefix() -> impl Strategy<Value = String> {
        (
            prop::sample::select(vec![
                "Re",
                "RE",
                "re",
                "AW",
                "Aw",
                "Ответ",
                "ОТВЕТ",
                "Fwd",
                "FWD",
                "FW",
                "WG",
            ]),
            prop::option::of(1u8..100),
            prop::sample::select(vec!["", " "]),
        )
            .prop_map(|(word, count, space)| match count {
                Some(n) => format!("{word}[{n}]:{space}"),
                None => format!("{word}:{space}"),
            })
    }

    proptest! {
        #[test]
        fn prefixes_never_pile_up(
            prefixes in prop::collection::vec(prefix(), 0..6),
            subject in "[A-Za-z0-9][A-Za-z0-9 ,.!?]{0,30}",
        ) {
            prop_assume!(strip_prefixes(&subject) == subject.trim());
            let full = format!("{}{subject}", prefixes.concat());
            let reply = reply_subject(&full);
            prop_assert_eq!(&reply, &reply_subject(&reply));
            prop_assert_eq!(strip_prefixes(&reply), subject.trim());
            prop_assert_eq!(forward_subject(&reply), forward_subject(&full));
        }
    }
}
