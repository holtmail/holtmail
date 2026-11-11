//! Mailbox names as IMAP shows them: the view's path joined by `/`, in modified UTF-7.

use holtmail_mailview::{FolderRole, Mailbox};

use crate::utf7;

/// The hierarchy delimiter.
pub(crate) const DELIMITER: char = '/';

/// The name of a mailbox of the tree, in UTF-8.
pub(crate) fn name(mailbox: &Mailbox) -> String {
    mailbox.path.join("/")
}

/// The name a client sent, decoded from modified UTF-7. `INBOX` in any letter case is the inbox.
pub(crate) fn decode(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let decoded = utf7::decode(&text);
    if decoded.eq_ignore_ascii_case("INBOX") {
        "INBOX".into()
    } else {
        decoded
    }
}

/// The name as sent to clients.
pub(crate) fn encode(name: &str) -> String {
    utf7::encode(name)
}

/// The mailbox of `tree` named `wanted`: the exact name first, then ignoring ASCII case.
pub(crate) fn find<'a>(tree: &'a [Mailbox], wanted: &str) -> Option<&'a Mailbox> {
    tree.iter()
        .find(|m| name(m) == wanted)
        .or_else(|| tree.iter().find(|m| name(m).eq_ignore_ascii_case(wanted)))
}

/// Whether `name` matches a `LIST` pattern: `*` matches anything, `%` anything but the
/// delimiter. The `INBOX` prefix of a pattern is matched ignoring case.
pub(crate) fn matches(pattern: &str, name: &str) -> bool {
    let pattern = if pattern.len() >= 5
        && pattern.is_char_boundary(5)
        && pattern[..5].eq_ignore_ascii_case("INBOX")
        && name.starts_with("INBOX")
    {
        format!("INBOX{}", &pattern[5..])
    } else {
        pattern.to_string()
    };
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    glob(&pattern, &name)
}

fn glob(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|skip| glob(rest, &name[skip..])),
        Some(('%', rest)) => {
            let limit = name
                .iter()
                .position(|c| *c == DELIMITER)
                .unwrap_or(name.len());
            (0..=limit).any(|skip| glob(rest, &name[skip..]))
        }
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

/// The `SPECIAL-USE` attribute of a role (RFC 6154), without its backslash.
pub(crate) fn special_use(role: FolderRole) -> Option<&'static str> {
    match role {
        FolderRole::Inbox => None,
        FolderRole::Sent => Some("Sent"),
        FolderRole::Drafts => Some("Drafts"),
        FolderRole::Trash => Some("Trash"),
        FolderRole::Junk => Some("Junk"),
        FolderRole::Archive => Some("Archive"),
        FolderRole::All => Some("All"),
        FolderRole::Flagged => Some("Flagged"),
        FolderRole::Important => Some("Important"),
    }
}

/// Whether some mailbox of `tree` sits below `mailbox`.
pub(crate) fn has_children(tree: &[Mailbox], mailbox: &Mailbox) -> bool {
    tree.iter().any(|other| {
        other.path.len() > mailbox.path.len()
            && other.path[..mailbox.path.len()] == mailbox.path[..]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(matches("*", "Accounts/Work/INBOX"));
        assert!(matches("%", "INBOX"));
        assert!(!matches("%", "Accounts/Work"));
        assert!(matches("Accounts/%", "Accounts/Work"));
        assert!(!matches("Accounts/%", "Accounts/Work/INBOX"));
        assert!(matches("inbox", "INBOX"));
        assert!(matches("Inbox*", "INBOX"));
        assert!(!matches("sent", "Sent"));
        assert!(matches("", ""));
        assert!(matches("A*Z", "AbcZ"));
        assert!(!matches("A*Z", "AbcY"));
    }

    #[test]
    fn decodes_names() {
        assert_eq!(decode(b"inbox"), "INBOX");
        assert_eq!(decode(b"Projects/&AMk-t&AOk-"), "Projects/Été");
    }
}
