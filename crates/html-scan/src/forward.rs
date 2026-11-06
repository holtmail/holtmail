const DASHED: &[&str] = &[
    "forwarded message",
    "weitergeleitete nachricht",
    "message transféré",
    "пересылаемое сообщение",
];

const PLAIN: &[&str] = &["begin forwarded message"];

/// The char offset in `text` of the first inline-forward marker, or `None` when there is none.
///
/// A marker starts a line (leading whitespace allowed) and is one of the forward headers mail
/// clients write: `---------- Forwarded message ---------` and its German, French and Russian
/// forms (at least two dashes before the phrase), or `Begin forwarded message:`. Letter case is
/// ignored. The offset points at the first non-whitespace char of the marker line. Linear in the
/// length of `text`; never panics.
pub fn forward_at(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if is_marker(line) {
            let lead = line.chars().take_while(|c| c.is_whitespace()).count();
            return Some(offset + lead);
        }
        offset += line.chars().count();
    }
    None
}

pub(crate) fn is_marker(line: &str) -> bool {
    let line = line.trim_start();
    let rest = line.trim_start_matches('-');
    let dashes = line.len() - rest.len();
    let head: String = rest
        .trim_start()
        .chars()
        .take(32)
        .flat_map(char::to_lowercase)
        .collect();
    PLAIN.iter().any(|phrase| head.starts_with(phrase))
        || (dashes >= 2 && DASHED.iter().any(|phrase| head.starts_with(phrase)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_at_line_start() {
        assert_eq!(
            forward_at("Hi\n\n---------- Forwarded message ---------\nFrom: a@example.com"),
            Some(4)
        );
        assert_eq!(forward_at("Begin forwarded message:\n"), Some(0));
        assert_eq!(forward_at("  -------- Forwarded Message --------"), Some(2));
        assert_eq!(
            forward_at("ok\n-------- Weitergeleitete Nachricht --------"),
            Some(3)
        );
        assert_eq!(
            forward_at("é\n-------- Message transféré --------"),
            Some(2)
        );
        assert_eq!(
            forward_at("-------- Пересылаемое сообщение --------"),
            Some(0)
        );
    }

    #[test]
    fn prose_is_not_a_marker() {
        assert_eq!(forward_at("I read the forwarded message yesterday."), None);
        assert_eq!(forward_at("Forwarded message attached"), None);
        assert_eq!(forward_at("-Forwarded message"), None);
        assert_eq!(forward_at("-----Original Message-----"), None);
        assert_eq!(forward_at(""), None);
    }

    #[test]
    fn offsets_count_chars() {
        assert_eq!(
            forward_at("Привет\r\n---------- Forwarded message"),
            Some(8)
        );
    }
}
