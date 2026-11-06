use std::collections::BTreeMap;

use crate::lex::is_ws;

/// One parsed `DKIM-Signature` field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DkimSignature {
    /// `d=`, lowercase.
    pub d: Option<String>,
    /// `s=`, lowercase.
    pub s: Option<String>,
    /// `b=` with all whitespace removed, letter case kept.
    pub b: Option<String>,
    /// `h=`: the signed field names in order, lowercase, each as often as it is listed.
    pub h: Vec<String>,
    /// Whether an `l=` (body length) tag is present.
    pub l: bool,
    /// Every tag in field order with its value trimmed (for `b` and `bh`, without whitespace).
    pub tags: Vec<(String, String)>,
}

impl DkimSignature {
    /// How often each signed field name is listed in `h=`; a count above the number of such
    /// fields in the message means the name is oversigned.
    pub fn h_counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for name in &self.h {
            *counts.entry(name.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// How often `name` is listed in `h=`, compared without regard to letter case.
    pub fn signed_count(&self, name: &str) -> usize {
        self.h
            .iter()
            .filter(|h| h.eq_ignore_ascii_case(name))
            .count()
    }

    /// The value of the tag `name` (tag names are case-sensitive).
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// At most the first [`B_PREFIX_LEN`](crate::B_PREFIX_LEN) characters of `b=`.
    pub fn b_prefix(&self) -> Option<&str> {
        self.b.as_deref().map(crate::b_prefix)
    }
}

/// Parses one `DKIM-Signature` value as an RFC 6376 tag-list. Tag names are case-sensitive,
/// folding whitespace may appear around tags and inside values, and a trailing `;` is allowed.
/// `None` when the value is not a tag-list: an entry without `=`, an invalid tag name, or a
/// tag that occurs twice (RFC 6376 §3.2 makes the whole list invalid then). Required tags are
/// not checked. Never panics.
pub fn parse_dkim_signature(value: &str) -> Option<DkimSignature> {
    let mut sig = DkimSignature::default();
    for spec in value.split(';') {
        if spec.chars().all(is_ws) {
            continue;
        }
        let (name, raw) = spec.split_once('=')?;
        let name = name.trim_matches(is_ws);
        if !valid_name(name) || sig.tags.iter().any(|(n, _)| n == name) {
            return None;
        }
        let value = match name {
            "b" | "bh" => raw.chars().filter(|c| !is_ws(*c)).collect(),
            _ => raw.trim_matches(is_ws).to_owned(),
        };
        match name {
            "d" => sig.d = Some(value.to_ascii_lowercase()),
            "s" => sig.s = Some(value.to_ascii_lowercase()),
            "b" => sig.b = Some(value.clone()),
            "h" => {
                sig.h = value
                    .split(':')
                    .map(|n| n.trim_matches(is_ws).to_ascii_lowercase())
                    .filter(|n| !n.is_empty())
                    .collect()
            }
            "l" => sig.l = true,
            _ => {}
        }
        sig.tags.push((name.to_owned(), value));
    }
    Some(sig)
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tags_with_folding() {
        let sig = parse_dkim_signature(
            "v=1; a=rsa-sha256; c=relaxed/relaxed; d=News.Example.com;\r\n s=S1;\r\n\th=From:To:Subject:\r\n List-Unsubscribe : list-unsubscribe:From;\r\n bh=abc\r\n def=; l=1234;\r\n b=AbC12345\r\n\t xyz+/ ==;",
        )
        .unwrap();
        assert_eq!(sig.d.as_deref(), Some("news.example.com"));
        assert_eq!(sig.s.as_deref(), Some("s1"));
        assert_eq!(sig.b.as_deref(), Some("AbC12345xyz+/=="));
        assert_eq!(sig.b_prefix(), Some("AbC12345"));
        assert_eq!(sig.tag("bh"), Some("abcdef="));
        assert_eq!(
            sig.h,
            [
                "from",
                "to",
                "subject",
                "list-unsubscribe",
                "list-unsubscribe",
                "from"
            ]
        );
        assert_eq!(sig.signed_count("FROM"), 2);
        assert_eq!(sig.h_counts().get("list-unsubscribe"), Some(&2));
        assert!(sig.l);
        assert_eq!(sig.tag("c"), Some("relaxed/relaxed"));
    }

    #[test]
    fn refuses_what_is_not_a_tag_list() {
        assert_eq!(parse_dkim_signature("d=example.com; d=example.org"), None);
        assert_eq!(parse_dkim_signature("d=example.com; garbage"), None);
        assert_eq!(parse_dkim_signature("1x=y"), None);
        assert_eq!(parse_dkim_signature("=y"), None);
        let empty = parse_dkim_signature(" ; ").unwrap();
        assert_eq!(empty, DkimSignature::default());
        let upper = parse_dkim_signature("D=example.com; d=example.org").unwrap();
        assert_eq!(upper.d.as_deref(), Some("example.org"));
        assert!(!upper.l);
    }
}
