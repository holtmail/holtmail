use std::collections::HashMap;

use crate::doc::{fold_chars, is_cjk};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum List {
    Code = 0,
    Promo = 1,
    Reference = 2,
    Month = 3,
    Money = 4,
    Address = 5,
    Footer = 6,
    Backup = 7,
}

const LISTS: usize = 8;

struct Phrase {
    chars: Vec<char>,
    list: List,
    whole_start: bool,
    whole_end: bool,
}

pub(crate) struct Matcher {
    phrases: Vec<Phrase>,
    first: HashMap<char, Vec<usize>>,
    max_len: usize,
}

impl Matcher {
    pub(crate) fn new(lists: &[(List, Vec<String>)]) -> Self {
        let mut phrases = Vec::new();
        let mut first: HashMap<char, Vec<usize>> = HashMap::new();
        let mut max_len = 1;
        for (list, words) in lists {
            for word in words {
                let word = word.trim();
                let (suffix, word) = match word.strip_prefix('-') {
                    Some(rest) if !rest.is_empty() => (true, rest),
                    _ => (false, word),
                };
                let chars = fold_chars(word);
                let (Some(&head), Some(&tail)) = (chars.first(), chars.last()) else {
                    continue;
                };
                let spaced = !chars.iter().any(|&c| is_cjk(c));
                first.entry(head).or_default().push(phrases.len());
                max_len = max_len.max(chars.len());
                phrases.push(Phrase {
                    whole_start: spaced && !suffix && head.is_alphanumeric(),
                    whole_end: spaced && tail.is_alphanumeric(),
                    chars,
                    list: *list,
                });
            }
        }
        Self {
            phrases,
            first,
            max_len,
        }
    }

    pub(crate) fn hits(&self, chars: &[char]) -> Hits {
        let mut found = Vec::new();
        let mut prev: Option<char> = None;
        for (i, &c) in chars.iter().enumerate() {
            if let Some(ids) = self.first.get(&c) {
                for phrase in ids.iter().filter_map(|&id| self.phrases.get(id)) {
                    if phrase.whole_start && prev.is_some_and(char::is_alphanumeric) {
                        continue;
                    }
                    let end = i + phrase.chars.len();
                    if chars.get(i..end) != Some(phrase.chars.as_slice()) {
                        continue;
                    }
                    if phrase.whole_end && chars.get(end).is_some_and(|c| c.is_alphanumeric()) {
                        continue;
                    }
                    found.push(Hit {
                        start: i,
                        end,
                        list: phrase.list,
                    });
                }
            }
            prev = Some(c);
        }
        Hits::from_found(found, self.max_len)
    }
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    start: usize,
    end: usize,
    list: List,
}

/// Phrase hits per list, sorted by start; a hit inside a strictly longer hit of another list is
/// dropped.
pub(crate) struct Hits {
    lists: [Vec<(usize, usize)>; LISTS],
    max_len: usize,
}

impl Hits {
    fn from_found(mut found: Vec<Hit>, max_len: usize) -> Self {
        found.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let mut reach: [Option<(usize, usize)>; LISTS] = [None; LISTS];
        let mut lists: [Vec<(usize, usize)>; LISTS] = Default::default();
        for hit in found {
            let inside = reach.iter().enumerate().any(|(list, r)| {
                list != hit.list as usize
                    && r.is_some_and(|(end, start)| {
                        end > hit.end || (end == hit.end && start < hit.start)
                    })
            });
            if !inside && let Some(slot) = lists.get_mut(hit.list as usize) {
                slot.push((hit.start, hit.end));
            }
            if let Some(r) = reach.get_mut(hit.list as usize) {
                let better = match *r {
                    None => true,
                    Some((end, _)) => hit.end > end,
                };
                if better {
                    *r = Some((hit.end, hit.start));
                }
            }
        }
        Self { lists, max_len }
    }

    fn list(&self, list: List) -> &[(usize, usize)] {
        self.lists.get(list as usize).map_or(&[], Vec::as_slice)
    }

    fn from(&self, list: List, at: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let hits = self.list(list);
        let skip = hits.partition_point(|&(start, _)| start < at);
        hits.iter().skip(skip).copied()
    }

    pub(crate) fn first(&self, list: List) -> Option<usize> {
        self.list(list).first().map(|&(start, _)| start)
    }

    /// Chars between `[a, b)` and the nearest hit, 0 when one overlaps; `None` beyond `max`.
    pub(crate) fn distance(&self, list: List, a: usize, b: usize, max: usize) -> Option<usize> {
        self.nearest(list, a, b, max).map(|(distance, _)| distance)
    }

    /// The nearest hit to `[a, b)` within `max` chars, with its distance; the first one on ties.
    pub(crate) fn nearest(
        &self,
        list: List,
        a: usize,
        b: usize,
        max: usize,
    ) -> Option<(usize, (usize, usize))> {
        let low = a.saturating_sub(max + self.max_len);
        self.from(list, low)
            .take_while(|&(start, _)| start < b + max)
            .map(|(start, end)| {
                let distance = if end <= a {
                    a - end
                } else {
                    start.saturating_sub(b)
                };
                (distance, (start, end))
            })
            .filter(|&(d, _)| d <= max)
            .min_by_key(|&(d, _)| d)
    }

    /// The first hit of `list`, as `(start, end)`.
    pub(crate) fn first_hit(&self, list: List) -> Option<(usize, usize)> {
        self.list(list).first().copied()
    }

    /// Hits that overlap `[a, b)`.
    pub(crate) fn overlapping(
        &self,
        list: List,
        a: usize,
        b: usize,
    ) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.from(list, a.saturating_sub(self.max_len))
            .take_while(move |&(start, _)| start < b)
            .filter(move |&(_, end)| end > a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher() -> Matcher {
        Matcher::new(&[
            (List::Code, vec!["code".into(), "验证码".into()]),
            (List::Promo, vec!["promo code".into()]),
        ])
    }

    #[test]
    fn suffix_phrases_end_compounds() {
        let m = Matcher::new(&[(List::Address, vec!["-straße".into()])]);
        let hits = m.hits(&fold_chars("Musterstraße 12, Straße 3, straßenbahn"));
        assert_eq!(hits.list(List::Address), &[(6, 12), (17, 23)]);
    }

    #[test]
    fn whole_words_and_substrings() {
        let m = matcher();
        let hits = m.hits(&fold_chars("Codes and code; 您的验证码是"));
        assert_eq!(hits.list(List::Code), &[(10, 14), (18, 21)]);
    }

    #[test]
    fn longer_hit_of_another_list_wins() {
        let m = matcher();
        let hits = m.hits(&fold_chars("Promo code SAVE20, your code 1234"));
        assert_eq!(hits.list(List::Promo), &[(0, 10)]);
        assert_eq!(hits.list(List::Code), &[(24, 28)]);
        assert_eq!(hits.distance(List::Code, 29, 33, 80), Some(1));
        assert_eq!(hits.distance(List::Promo, 29, 33, 10), None);
    }
}
