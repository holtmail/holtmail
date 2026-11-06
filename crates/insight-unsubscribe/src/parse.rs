use holtmail_insight::{Url, UrlScheme};

const ONE_CLICK: &str = "List-Unsubscribe=One-Click";
const MAX_LIST_ID_BYTES: usize = 255;

/// The links of one `List-Unsubscribe` field, at most one per scheme.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnsubscribeLinks {
    /// The first `https` link that is a valid [`Url`].
    pub https: Option<Url>,
    /// The first `http` link that is a valid [`Url`].
    pub http: Option<Url>,
    /// The first `mailto` link that is a valid [`Url`].
    pub mailto: Option<Url>,
}

impl UnsubscribeLinks {
    /// True when the field gave no link.
    pub fn is_empty(&self) -> bool {
        self.https.is_none() && self.http.is_none() && self.mailto.is_none()
    }

    fn is_full(&self) -> bool {
        self.https.is_some() && self.http.is_some() && self.mailto.is_some()
    }
}

/// Reads a `List-Unsubscribe` value strictly by RFC 2369.
///
/// - Items are comma-separated and read left to right; only `<…>` items count, with whitespace
///   inside the brackets ignored. Anything after a `>` up to the next comma is ignored.
/// - Reading stops at the first item that does not start with `<` or has no closing `>`, so a
///   link after an unbracketed item is never taken.
/// - Only `https`, `http` and `mailto` links that are a valid [`Url`] are kept, the first of each
///   scheme; every other item (`file:`, `javascript:`, relative or malformed) is skipped.
///
/// Never panics, and runs in time linear in the length of `value`.
pub fn parse_list_unsubscribe(value: &str) -> UnsubscribeLinks {
    walk(value, |_, _| {})
}

/// What happened to one item of a `List-Unsubscribe` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Kept(UrlScheme),
    Duplicate(UrlScheme),
    Refused,
    Unbracketed,
    Unclosed,
}

/// [`parse_list_unsubscribe`], reporting each item's position and fate to `seen`.
pub(crate) fn walk(value: &str, mut seen: impl FnMut(usize, Step)) -> UnsubscribeLinks {
    let mut links = UnsubscribeLinks::default();
    let mut rest = value;
    let mut index = 0;
    loop {
        let Some(after) = rest.trim_start().strip_prefix('<') else {
            seen(index, Step::Unbracketed);
            break;
        };
        let Some((inner, tail)) = after.split_once('>') else {
            seen(index, Step::Unclosed);
            break;
        };
        let compact: String = inner.chars().filter(|c| !c.is_whitespace()).collect();
        match Url::parse(&compact) {
            Ok(url) => {
                let scheme = url.scheme();
                let slot = match scheme {
                    UrlScheme::Https => &mut links.https,
                    UrlScheme::Http => &mut links.http,
                    UrlScheme::Mailto => &mut links.mailto,
                };
                if slot.is_none() {
                    *slot = Some(url);
                    seen(index, Step::Kept(scheme));
                } else {
                    seen(index, Step::Duplicate(scheme));
                }
            }
            Err(_) => seen(index, Step::Refused),
        }
        if links.is_full() {
            break;
        }
        match tail.split_once(',') {
            Some((_, next)) => rest = next,
            None => break,
        }
        index += 1;
    }
    links
}

/// True when a `List-Unsubscribe-Post` value is exactly `List-Unsubscribe=One-Click` after
/// trimming surrounding whitespace, letter case included (RFC 8058).
pub fn is_one_click_post(value: &str) -> bool {
    value.trim() == ONE_CLICK
}

/// The list identifier of a `List-Id` value (RFC 2919), lowercased.
///
/// The identifier is the content of the last `<…>` in the value. It is `None` unless that content
/// is a dot-atom of at most 255 bytes with at least one dot, as RFC 2919 requires.
pub fn parse_list_id(value: &str) -> Option<String> {
    let (_, after) = value.rsplit_once('<')?;
    let (inner, _) = after.split_once('>')?;
    let id = inner.trim();
    let valid = !id.is_empty()
        && id.len() <= MAX_LIST_ID_BYTES
        && id.contains('.')
        && id
            .split('.')
            .all(|label| !label.is_empty() && label.bytes().all(is_atext));
    valid.then(|| id.to_ascii_lowercase())
}

fn is_atext(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(url: &Option<Url>) -> Option<&str> {
        url.as_ref().map(Url::as_str)
    }

    #[test]
    fn reads_items_left_to_right_keeping_the_first_of_each_scheme() {
        let links = parse_list_unsubscribe(
            "<mailto:leave@example.com?subject=unsubscribe>, <https://example.com/u/1>, \
             <https://example.com/u/2>, <http://example.com/u/3>, <mailto:other@example.com>",
        );
        assert_eq!(s(&links.https), Some("https://example.com/u/1"));
        assert_eq!(s(&links.http), Some("http://example.com/u/3"));
        assert_eq!(
            s(&links.mailto),
            Some("mailto:leave@example.com?subject=unsubscribe")
        );
    }

    #[test]
    fn ignores_whitespace_inside_brackets_and_text_after_them() {
        let links =
            parse_list_unsubscribe("<https://example.com/ u/1> (comment), <mailto:a@example.com>");
        assert_eq!(s(&links.https), Some("https://example.com/u/1"));
        assert_eq!(s(&links.mailto), Some("mailto:a@example.com"));
    }

    #[test]
    fn stops_at_the_first_unbracketed_item() {
        let links = parse_list_unsubscribe(
            "<mailto:a@example.com>, https://example.org/u, <https://example.com/u>",
        );
        assert_eq!(s(&links.mailto), Some("mailto:a@example.com"));
        assert!(links.https.is_none());
        assert!(
            parse_list_unsubscribe("https://example.org/u, <https://example.com/u>").is_empty()
        );
        assert!(parse_list_unsubscribe("<https://example.com/u").is_empty());
        assert!(parse_list_unsubscribe(", <https://example.com/u>").is_empty());
        assert!(
            parse_list_unsubscribe("<https://example.com/u> <https://example.org/u>")
                .https
                .is_some_and(|u| u.as_str() == "https://example.com/u")
        );
    }

    #[test]
    fn refuses_other_schemes_and_continues() {
        let links = parse_list_unsubscribe(
            "<file:///etc/passwd>, <javascript:alert(1)>, <ftp://example.com/u>, <u/1>, \
             <https://user:pw@example.com/u>, <mailto:>, <https://example.com/u>",
        );
        assert_eq!(s(&links.https), Some("https://example.com/u"));
        assert!(links.http.is_none() && links.mailto.is_none());
    }

    #[test]
    fn scheme_letter_case_does_not_matter() {
        let links = parse_list_unsubscribe("<HTTPS://Example.COM/U>");
        assert_eq!(s(&links.https), Some("https://example.com/U"));
    }

    #[test]
    fn one_click_post_is_exact() {
        assert!(is_one_click_post("List-Unsubscribe=One-Click"));
        assert!(is_one_click_post("  List-Unsubscribe=One-Click "));
        assert!(!is_one_click_post("list-unsubscribe=one-click"));
        assert!(!is_one_click_post("List-Unsubscribe=One-Click; x=1"));
        assert!(!is_one_click_post(""));
    }

    #[test]
    fn list_id_is_the_bracketed_dot_atom() {
        assert_eq!(
            parse_list_id("Example News <News.Example.COM>").as_deref(),
            Some("news.example.com")
        );
        assert_eq!(
            parse_list_id("<weekly.list.example.org>").as_deref(),
            Some("weekly.list.example.org")
        );
        assert_eq!(parse_list_id("news.example.com"), None);
        assert_eq!(parse_list_id("<news>"), None);
        assert_eq!(parse_list_id("<news..example.com>"), None);
        assert_eq!(parse_list_id("<news example.com>"), None);
        assert_eq!(parse_list_id("<новости.example.com>"), None);
        assert_eq!(
            parse_list_id(&format!("<{}.example.com>", "a".repeat(250))),
            None
        );
    }
}
