//! Gmail `X-GM-LABELS` values as readable label names.

use crate::utf7;

/// The readable name of one `X-GM-LABELS` value as the IMAP parser returns it: still escaped
/// when it was a quoted string, and in modified UTF-7. System labels lose their backslash and
/// read as Gmail shows them.
pub(crate) fn label_name(raw: &str) -> String {
    let unescaped = unescape(raw);
    if let Some(system) = unescaped.strip_prefix('\\') {
        return match system.to_ascii_lowercase().as_str() {
            "inbox" => "Inbox".into(),
            "sent" => "Sent".into(),
            "important" => "Important".into(),
            "starred" => "Starred".into(),
            "draft" | "drafts" => "Drafts".into(),
            "trash" => "Trash".into(),
            "spam" => "Spam".into(),
            _ => system.to_owned(),
        };
    }
    utf7::decode(&unescaped)
}

/// Readable names of a message's labels, sorted and without duplicates or empty names.
pub(crate) fn label_names<'a>(raw: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut names: Vec<String> = raw
        .into_iter()
        .map(label_name)
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Whether a readable label name stands for a mailbox (Inbox, Sent, Drafts, Trash, Spam, in
/// any case, or a raw `\` system name): changing it would move or delete the message.
pub(crate) fn acts_as_folder(name: &str) -> bool {
    name.starts_with('\\')
        || ["Inbox", "Sent", "Drafts", "Trash", "Spam"]
            .iter()
            .any(|system| name.eq_ignore_ascii_case(system))
}

/// The `X-GM-LABELS` value for a readable label name, the mirror of [`label_name`]: the names
/// Gmail gives its system labels become `\Inbox` and the like, any other name is encoded in
/// modified UTF-7 and quoted.
pub(crate) fn label_value(name: &str) -> String {
    let system = match name {
        "Inbox" => "\\Inbox",
        "Sent" => "\\Sent",
        "Important" => "\\Important",
        "Starred" => "\\Starred",
        "Drafts" => "\\Draft",
        "Trash" => "\\Trash",
        "Spam" => "\\Spam",
        _ => return crate::writer::quote(&utf7::encode(name)),
    };
    system.to_owned()
}

fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.clone().next() {
                Some(next @ ('\\' | '"')) => {
                    chars.next();
                    out.push(next);
                }
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_system_labels() {
        assert_eq!(label_name("\\Inbox"), "Inbox");
        assert_eq!(label_name("\\\\Important"), "Important");
        assert_eq!(label_name("\\\\Starred"), "Starred");
        assert_eq!(label_name("\\Sent"), "Sent");
        assert_eq!(label_name("\\Draft"), "Drafts");
        assert_eq!(label_name("\\Muted"), "Muted");
    }

    #[test]
    fn decodes_quoted_and_utf7_labels() {
        assert_eq!(label_name("Projects/2025"), "Projects/2025");
        assert_eq!(label_name(r#"Say \"hi\""#), r#"Say "hi""#);
        assert_eq!(label_name(r"back\\slash"), r"back\slash");
        assert_eq!(label_name("&BBIEMAQ2BD0EPgQ1-"), "Важное");
        assert_eq!(label_name("Fish &- Chips"), "Fish & Chips");
    }

    #[test]
    fn values_mirror_names() {
        assert_eq!(label_value("Inbox"), "\\Inbox");
        assert_eq!(label_value("Drafts"), "\\Draft");
        assert_eq!(
            label_value("Projects/Важное"),
            "\"Projects/&BBIEMAQ2BD0EPgQ1-\""
        );
        assert_eq!(label_value(r#"Say "hi""#), r#""Say \"hi\"""#);
        for name in ["Inbox", "Drafts", "Starred"] {
            assert_eq!(label_name(&label_value(name)), name);
        }
    }

    #[test]
    fn sorts_and_dedups() {
        assert_eq!(
            label_names(["Work", "\\Inbox", "Work", ""]),
            ["Inbox", "Work"]
        );
    }
}
