use std::fmt;

use crate::PartRef;

const MAX_SKELETON_CHARS: usize = 64;

/// One step of an extractor's reasoning, safe to print on an operator's terminal.
///
/// It carries no message content: a rule name fixed in code, a part and char position, a score, a
/// count, the [`Skeleton`] of a candidate and keywords from the extractor's own embedded tables.
/// Values, hosts, URLs and free text from the message never appear in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    /// The rule or step, such as `money` or `outcome:tie`; a name fixed in the extractor's code.
    pub rule: &'static str,
    /// The part the step looked at.
    pub part: Option<PartRef>,
    /// A char offset in that part, or an item's position in a list.
    pub at: Option<usize>,
    /// The score the step gave or compared.
    pub score: Option<i32>,
    /// How many of something the step counted.
    pub count: Option<usize>,
    /// The shape of the candidate the step is about.
    pub skeleton: Option<Skeleton>,
    /// Keywords the step matched, exactly as they are written in the extractor's own tables.
    pub keywords: Vec<String>,
}

impl Explanation {
    /// A step named `rule` with nothing else.
    pub fn new(rule: &'static str) -> Self {
        Self {
            rule,
            part: None,
            at: None,
            score: None,
            count: None,
            skeleton: None,
            keywords: Vec::new(),
        }
    }

    /// The step in this part.
    pub fn part(mut self, part: PartRef) -> Self {
        self.part = Some(part);
        self
    }

    /// The step at this position.
    pub fn at(mut self, at: usize) -> Self {
        self.at = Some(at);
        self
    }

    /// The step with this score.
    pub fn score(mut self, score: i32) -> Self {
        self.score = Some(score);
        self
    }

    /// The step with this count.
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// The step about a candidate of this shape.
    pub fn skeleton(mut self, skeleton: Skeleton) -> Self {
        self.skeleton = Some(skeleton);
        self
    }

    /// The step with this keyword from the extractor's own table.
    pub fn keyword(mut self, keyword: impl Into<String>) -> Self {
        self.keywords.push(keyword.into());
        self
    }
}

/// The shape of a text with its content removed: every digit becomes `D`, every letter `a`,
/// whitespace a space, ASCII punctuation stays and anything else becomes `*`. At most 64 chars;
/// a longer text ends in `…`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Skeleton(String);

impl Skeleton {
    /// The skeleton of `text`.
    pub fn of(text: &str) -> Self {
        let mut out = String::new();
        for (index, c) in text.chars().enumerate() {
            if index == MAX_SKELETON_CHARS {
                out.push('…');
                break;
            }
            out.push(if c.is_numeric() {
                'D'
            } else if c.is_alphabetic() {
                'a'
            } else if c.is_whitespace() {
                ' '
            } else if c.is_ascii_punctuation() {
                c
            } else {
                '*'
            });
        }
        Self(out)
    }

    /// The skeleton as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Skeleton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_keeps_only_the_shape() {
        assert_eq!(Skeleton::of("482-913").as_str(), "DDD-DDD");
        assert_eq!(Skeleton::of("K7p2 QX").as_str(), "aDaD aa");
        assert_eq!(Skeleton::of("８７6").as_str(), "DDD");
        assert_eq!(Skeleton::of("验证码：1").as_str(), "aaa*D");
        let long = Skeleton::of(&"9".repeat(100));
        assert_eq!(long.as_str().chars().count(), 65);
        assert!(long.as_str().ends_with('…'));
    }

    #[test]
    fn explanation_builds() {
        let step = Explanation::new("candidate")
            .part(PartRef::Text)
            .at(12)
            .score(640)
            .count(1)
            .skeleton(Skeleton::of("123456"))
            .keyword("code");
        assert_eq!(step.at, Some(12));
        assert_eq!(step.keywords, ["code"]);
        assert_eq!(step.skeleton.unwrap().as_str(), "DDDDDD");
    }
}
