use std::collections::HashMap;

use holtmail_insight::LinkAction;

use crate::fold::{fold_chars, is_cjk};

struct Phrase {
    chars: Vec<char>,
    action: LinkAction,
    keyword: String,
    whole: bool,
}

/// A matched label: its action and the phrase as written in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Label<'a> {
    pub action: LinkAction,
    pub keyword: &'a str,
}

pub(crate) struct Matcher {
    phrases: Vec<Phrase>,
    first: HashMap<char, Vec<usize>>,
}

impl Matcher {
    pub(crate) fn new(labels: &[(LinkAction, String)]) -> Result<Self, String> {
        let mut phrases: Vec<Phrase> = Vec::new();
        let mut first: HashMap<char, Vec<usize>> = HashMap::new();
        for (action, keyword) in labels {
            let chars = fold_chars(keyword.trim(), usize::MAX);
            let (Some(&head), Some(&tail)) = (chars.first(), chars.last()) else {
                return Err(format!("empty label for {action:?}"));
            };
            if let Some(other) = phrases.iter().find(|p| p.chars == chars) {
                if other.action != *action {
                    return Err(format!("label {keyword:?} names two actions"));
                }
                continue;
            }
            let spaced = !chars.iter().any(|&c| is_cjk(c));
            first.entry(head).or_default().push(phrases.len());
            phrases.push(Phrase {
                whole: spaced && head.is_alphanumeric() && tail.is_alphanumeric(),
                chars,
                action: *action,
                keyword: keyword.trim().to_owned(),
            });
        }
        Ok(Self { phrases, first })
    }

    /// The label that starts first in `chars`, the longest one among those that start there.
    pub(crate) fn find(&self, chars: &[char]) -> Option<Label<'_>> {
        let mut prev: Option<char> = None;
        for (i, &c) in chars.iter().enumerate() {
            let mut best: Option<&Phrase> = None;
            for phrase in self
                .first
                .get(&c)
                .into_iter()
                .flatten()
                .filter_map(|&id| self.phrases.get(id))
            {
                if phrase.whole && prev.is_some_and(char::is_alphanumeric) {
                    continue;
                }
                let end = i + phrase.chars.len();
                if chars.get(i..end) != Some(phrase.chars.as_slice()) {
                    continue;
                }
                if phrase.whole && chars.get(end).is_some_and(|c| c.is_alphanumeric()) {
                    continue;
                }
                if best.is_none_or(|b| phrase.chars.len() > b.chars.len()) {
                    best = Some(phrase);
                }
            }
            if let Some(phrase) = best {
                return Some(Label {
                    action: phrase.action,
                    keyword: &phrase.keyword,
                });
            }
            prev = Some(c);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher() -> Matcher {
        Matcher::new(&[
            (LinkAction::Confirm, "confirm".into()),
            (LinkAction::Verify, "confirm your email".into()),
            (LinkAction::SignIn, "登录".into()),
            (LinkAction::MagicLink, "登录链接".into()),
        ])
        .unwrap()
    }

    fn find(text: &str) -> Option<LinkAction> {
        let m = matcher();
        m.find(&fold_chars(text, usize::MAX)).map(|l| l.action)
    }

    #[test]
    fn longest_at_the_first_start_wins() {
        assert_eq!(find("Confirm your email"), Some(LinkAction::Verify));
        assert_eq!(find("Please confirm"), Some(LinkAction::Confirm));
        assert_eq!(find("点击登录链接"), Some(LinkAction::MagicLink));
        assert_eq!(find("立即登录"), Some(LinkAction::SignIn));
    }

    #[test]
    fn spaced_labels_match_whole_words() {
        assert_eq!(find("Confirmation"), None);
        assert_eq!(find("unconfirm"), None);
        assert_eq!(find("confirm!"), Some(LinkAction::Confirm));
    }

    #[test]
    fn one_label_one_action() {
        assert!(
            Matcher::new(&[
                (LinkAction::Confirm, "ok".into()),
                (LinkAction::Verify, "OK".into()),
            ])
            .is_err()
        );
    }
}
