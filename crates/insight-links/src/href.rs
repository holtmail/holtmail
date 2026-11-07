use holtmail_insight::LinkAction;

use crate::data::PathToken;

const DECODE_ROUNDS: usize = 2;

/// An href in the forms the rules read: ASCII-lowercased as written, and lowercased after
/// percent-decoding, so the target of a wrapped link is visible.
pub(crate) struct Href {
    lowered: String,
    decoded: String,
}

/// A URL token found in an href.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token<'a> {
    pub action: Option<LinkAction>,
    pub keyword: &'a str,
}

impl Href {
    pub(crate) fn new(raw: &str) -> Self {
        let mut decoded = raw.to_owned();
        for _ in 0..DECODE_ROUNDS {
            let next = percent_decode(&decoded);
            if next == decoded {
                break;
            }
            decoded = next;
        }
        Self {
            lowered: raw.to_ascii_lowercase(),
            decoded: decoded.to_lowercase(),
        }
    }

    fn forms(&self) -> [&str; 2] {
        [&self.lowered, &self.decoded]
    }

    /// The first skip token that occurs in either form.
    pub(crate) fn skip<'a>(&self, tokens: &'a [String]) -> Option<&'a str> {
        tokens
            .iter()
            .find(|token| {
                self.forms()
                    .iter()
                    .any(|form| form.contains(token.as_str()))
            })
            .map(String::as_str)
    }

    /// The path token that occurs first, preferring tokens that name an action.
    pub(crate) fn path_token<'a>(&self, tokens: &'a [PathToken]) -> Option<Token<'a>> {
        let mut best: Option<(bool, usize, &PathToken)> = None;
        for token in tokens {
            let Some(at) = self
                .forms()
                .iter()
                .filter_map(|form| find_token(form, &token.token))
                .min()
            else {
                continue;
            };
            let key = (token.action.is_none(), at);
            if best.is_none_or(|(none, best_at, _)| key < (none, best_at)) {
                best = Some((key.0, key.1, token));
            }
        }
        best.map(|(_, _, token)| Token {
            action: token.action,
            keyword: &token.token,
        })
    }

    /// Every host named after a `://` in the decoded form, lowercase, without port or userinfo.
    pub(crate) fn hosts(&self) -> impl Iterator<Item = &str> {
        self.decoded.match_indices("://").filter_map(|(at, sep)| {
            let rest = self.decoded.get(at + sep.len()..)?;
            let end = rest
                .find(|c: char| "/?#&\\\"' ".contains(c) || c.is_whitespace())
                .unwrap_or(rest.len());
            let authority = rest.get(..end)?;
            let host = authority.rsplit('@').next()?;
            let host = host.split(':').next()?.trim_end_matches('.');
            (!host.is_empty()).then_some(host)
        })
    }
}

/// Whether `host` is `domain` or one of its subdomains.
pub(crate) fn host_in(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain)
        .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
}

fn find_token(form: &str, token: &str) -> Option<usize> {
    let bytes = form.as_bytes();
    form.match_indices(token).map(|(at, _)| at).find(|&at| {
        let before = at.checked_sub(1).and_then(|i| bytes.get(i)).copied();
        let after = bytes.get(at + token.len()).copied();
        if token.starts_with('/') {
            let continues = after.is_some_and(|b| b.is_ascii_alphanumeric() || b >= 0x80);
            !matches!(before, Some(b'/' | b':')) && (token.ends_with('/') || !continues)
        } else if token.ends_with('=') {
            matches!(before, Some(b'?' | b'&' | b';' | b'#'))
        } else {
            true
        }
    })
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).and_then(|h| hex(*h)),
                bytes.get(i + 2).and_then(|l| hex(*l)),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Vec<PathToken> {
        vec![
            PathToken {
                token: "token=".into(),
                action: None,
            },
            PathToken {
                token: "/verify".into(),
                action: Some(LinkAction::Verify),
            },
            PathToken {
                token: "/reset".into(),
                action: Some(LinkAction::ResetPassword),
            },
        ]
    }

    fn path(href: &str) -> Option<(Option<LinkAction>, String)> {
        let tokens = tokens();
        Href::new(href)
            .path_token(&tokens)
            .map(|t| (t.action, t.keyword.to_owned()))
    }

    #[test]
    fn path_tokens_need_segment_boundaries() {
        assert_eq!(
            path("https://example.com/account/Verify?x=1"),
            Some((Some(LinkAction::Verify), "/verify".into()))
        );
        assert_eq!(
            path("https://example.com/verify-email"),
            Some((Some(LinkAction::Verify), "/verify".into()))
        );
        assert_eq!(path("https://example.com/verifyx"), None);
        assert_eq!(path("https://verify.example.com/"), None);
        assert_eq!(
            path("https://example.com/a?token=1"),
            Some((None, "token=".into()))
        );
        assert_eq!(path("https://example.com/a?mytoken=1"), None);
    }

    #[test]
    fn action_tokens_win_over_plain_ones() {
        assert_eq!(
            path("https://example.com/a?token=1&next=/reset"),
            Some((Some(LinkAction::ResetPassword), "/reset".into()))
        );
    }

    #[test]
    fn wrapped_paths_are_read_decoded() {
        let wrapped = "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2Freset%3Ft%3D1&data=x";
        assert_eq!(
            path(wrapped),
            Some((Some(LinkAction::ResetPassword), "/reset".into()))
        );
        let double = "https://www.google.com/url?q=https%253A%252F%252Fexample.com%252Fverify";
        assert_eq!(
            path(double),
            Some((Some(LinkAction::Verify), "/verify".into()))
        );
    }

    #[test]
    fn hosts_include_wrapped_targets() {
        let href = Href::new(
            "https://www.google.com/url?q=https%3A%2F%2Fuser@WWW.Facebook.com:443%2Fpage",
        );
        let hosts: Vec<&str> = href.hosts().collect();
        assert_eq!(hosts, ["www.google.com", "www.facebook.com"]);
        assert!(host_in("www.facebook.com", "facebook.com"));
        assert!(host_in("facebook.com", "facebook.com"));
        assert!(!host_in("notfacebook.com", "facebook.com"));
    }

    #[test]
    fn skip_tokens_read_both_forms() {
        let skip = vec!["unsubscribe".to_owned()];
        assert!(
            Href::new("https://example.com/r?u=%2Funsubscribe")
                .skip(&skip)
                .is_some()
        );
        assert!(
            Href::new("https://example.com/verify")
                .skip(&skip)
                .is_none()
        );
    }

    #[test]
    fn decoding_never_panics_on_broken_escapes() {
        for raw in ["%", "%Z1", "%e2%82", "abc%", "%%41", "\u{0}%ff"] {
            let _ = Href::new(raw);
        }
        assert_eq!(percent_decode("%41%2f%zz"), "A/%zz");
    }
}
