//! Parsing the Holtmail search language into a [`SearchQuery`].
//!
//! ```text
//! from:ada@example.com has:attachment before:2024-01-01 "quarterly report" -in:trash
//! ```

use holtmail_category::{MessageKind, Topic};
use holtmail_core::Timestamp;
use holtmail_index::{Clause, DocumentKind, SearchQuery};
use holtmail_insight::KindTag;

/// Why a search could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    /// A date operator has a value that is not `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
    #[error("{0:?} is not a date; use YYYY, YYYY-MM or YYYY-MM-DD")]
    InvalidDate(String),
    /// An operator is missing its value, e.g. `from:` at the end.
    #[error("{0}: needs a value")]
    MissingValue(String),
    /// An operator that only takes known values got another one, e.g. `category:weather`.
    #[error(
        "{operator}: {value:?} is not a {operator}; use one of {}",
        allowed(operator)
    )]
    UnknownValue {
        /// The operator, lowercase.
        operator: String,
        /// The value as typed.
        value: String,
    },
}

fn allowed(operator: &str) -> String {
    match operator {
        "category" => Topic::ALL
            .iter()
            .map(|t| t.name())
            .chain(MessageKind::ALL.iter().map(|k| k.name()))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Parses `input`.
///
/// - Words match anywhere; `"quoted words"` match as a phrase.
/// - `from:`, `to:`, `subject:`, `body:`, `filename:`, `attachment:`, `in:`, `label:` take a word
///   or a quoted value; `label:"Muy importante"` matches the whole label name.
/// - `has:attachment`; `is:read`, `is:unread`, `is:flagged` (also `is:starred`).
/// - `is:event`, `is:contact` and `is:mail` find only documents of that kind.
/// - `has:code`, `has:link`, `has:unsubscribe`, `has:reservation`, `has:order`, `has:invoice`,
///   `has:parcel` and `has:pass` find messages with that insight ([`Clause::Insight`]).
/// - `category:<topic or kind>` (`category:travel`, `category:bulk`) and `is:newsletter`
///   (kind `bulk`), `is:notification`, `is:personal`, `is:list` find messages by category
///   ([`Clause::Category`] with `topic:<name>` or `kind:<name>`). An unknown `category:` value
///   is [`QueryError::UnknownValue`].
/// - `with:` takes a name or an address and finds mail from or to them, events they organize
///   or attend, and their contact cards.
/// - `before:` and `after:` take `YYYY`, `YYYY-MM` or `YYYY-MM-DD` in UTC; `before:2024` means
///   before 1 January 2024, `after:2024-03` means from 1 March 2024 on.
/// - `-` before any term negates it.
/// - An unknown `name:value`, and an unknown `has:` or `is:` value, is searched as plain text.
pub fn parse(input: &str) -> Result<SearchQuery, QueryError> {
    let mut clauses = Vec::new();
    for token in tokens(input) {
        let clause = clause(&token.text, token.quoted)?;
        clauses.push(if token.negated {
            Clause::Not(Box::new(clause))
        } else {
            clause
        });
    }
    Ok(SearchQuery { clauses })
}

struct Token {
    text: String,
    quoted: bool,
    negated: bool,
}

fn tokens(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut negated = false;
        if c == '-' {
            chars.next();
            match chars.peek() {
                Some(next) if !next.is_whitespace() => negated = true,
                _ => continue,
            }
        }
        let mut text = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            if c == '"' {
                quoted = true;
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    text.push(c);
                }
            } else {
                text.push(c);
            }
        }
        if !text.is_empty() || quoted {
            out.push(Token {
                text,
                quoted,
                negated,
            });
        }
    }
    out
}

fn clause(text: &str, quoted: bool) -> Result<Clause, QueryError> {
    let Some((name, value)) = text.split_once(':').filter(|(name, _)| !name.contains(' ')) else {
        return Ok(free(text, quoted));
    };
    let name = name.to_ascii_lowercase();
    let known = matches!(
        name.as_str(),
        "from"
            | "to"
            | "subject"
            | "body"
            | "filename"
            | "attachment"
            | "in"
            | "label"
            | "with"
            | "has"
            | "is"
            | "category"
            | "before"
            | "after"
    );
    if !known {
        return Ok(free(text, quoted));
    }
    if value.is_empty() {
        return Err(QueryError::MissingValue(name));
    }
    let value = value.to_owned();
    Ok(match name.as_str() {
        "from" => Clause::From(value),
        "to" => Clause::To(value),
        "subject" => Clause::Subject(value),
        "body" => Clause::Body(value),
        "filename" => Clause::Filename(value),
        "attachment" => Clause::Attachment(value),
        "in" => Clause::In(value),
        "label" => Clause::Label(value),
        "with" => Clause::With(value),
        "has" if value.eq_ignore_ascii_case("attachment") => Clause::HasAttachment,
        "has" => match KindTag::from_name(&value.to_ascii_lowercase()) {
            Some(tag) => Clause::Insight(tag.name().into()),
            None => free(text, quoted),
        },
        "category" => category(&value)?,
        "is" => match value.to_ascii_lowercase().as_str() {
            "newsletter" | "newsletters" => kind(MessageKind::Bulk),
            "notification" | "notifications" => kind(MessageKind::Notification),
            "personal" => kind(MessageKind::Personal),
            "list" => kind(MessageKind::List),
            "read" | "seen" => Clause::Flag("\\Seen".into()),
            "unread" | "unseen" => Clause::Not(Box::new(Clause::Flag("\\Seen".into()))),
            "flagged" | "starred" => Clause::Flag("\\Flagged".into()),
            "event" | "events" | "meeting" => Clause::Kind(DocumentKind::Event),
            "contact" | "contacts" => Clause::Kind(DocumentKind::Contact),
            "mail" | "message" | "messages" => Clause::Kind(DocumentKind::Mail),
            _ => free(text, quoted),
        },
        "before" => Clause::Before(date(&value)?),
        "after" => Clause::After(date(&value)?),
        _ => free(text, quoted),
    })
}

fn kind(kind: MessageKind) -> Clause {
    Clause::Category(format!("kind:{}", kind.name()))
}

fn category(value: &str) -> Result<Clause, QueryError> {
    let name = value.to_lowercase();
    if let Some(topic) = Topic::from_name(&name) {
        return Ok(Clause::Category(format!("topic:{}", topic.name())));
    }
    match MessageKind::from_name(&name) {
        Some(found) => Ok(kind(found)),
        None => Err(QueryError::UnknownValue {
            operator: "category".into(),
            value: value.to_owned(),
        }),
    }
}

fn free(text: &str, quoted: bool) -> Clause {
    if quoted {
        Clause::Phrase(text.to_owned())
    } else {
        Clause::Text(text.to_owned())
    }
}

fn date(value: &str) -> Result<Timestamp, QueryError> {
    let invalid = || QueryError::InvalidDate(value.to_owned());
    let mut parts = value.split('-');
    let year: i64 = parts
        .next()
        .filter(|p| p.len() == 4)
        .and_then(|p| p.parse().ok())
        .ok_or_else(invalid)?;
    let mut number = |max: i64| -> Result<i64, QueryError> {
        match parts.next() {
            None => Ok(1),
            Some(p) => p
                .parse()
                .ok()
                .filter(|n| (1..=max).contains(n) && p.len() == 2)
                .ok_or_else(invalid),
        }
    };
    let month = number(12)?;
    let day = number(31)?;
    if parts.next().is_some() {
        return Err(invalid());
    }
    Ok(Timestamp::from_secs(
        days_from_civil(year, month, day) * 86_400,
    ))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clauses(input: &str) -> Vec<Clause> {
        parse(input).unwrap().clauses
    }

    #[test]
    fn parses_words_phrases_and_operators() {
        assert_eq!(
            clauses(
                r#"lunch "quarterly report" from:ada@example.com subject:"menu, v2" has:attachment"#
            ),
            vec![
                Clause::Text("lunch".into()),
                Clause::Phrase("quarterly report".into()),
                Clause::From("ada@example.com".into()),
                Clause::Subject("menu, v2".into()),
                Clause::HasAttachment,
            ]
        );
    }

    #[test]
    fn searches_attachment_text() {
        assert_eq!(
            clauses(r#"attachment:invoice -attachment:"draft copy""#),
            vec![
                Clause::Attachment("invoice".into()),
                Clause::Not(Box::new(Clause::Attachment("draft copy".into()))),
            ]
        );
    }

    #[test]
    fn negates_with_a_dash() {
        assert_eq!(
            clauses("-in:trash - word"),
            vec![
                Clause::Not(Box::new(Clause::In("trash".into()))),
                Clause::Text("word".into()),
            ]
        );
    }

    #[test]
    fn parses_labels() {
        assert_eq!(
            clauses(r#"label:Projects/2025 -label:"Muy importante""#),
            vec![
                Clause::Label("Projects/2025".into()),
                Clause::Not(Box::new(Clause::Label("Muy importante".into()))),
            ]
        );
    }

    #[test]
    fn maps_flags() {
        assert_eq!(
            clauses("is:unread IS:Starred"),
            vec![
                Clause::Not(Box::new(Clause::Flag("\\Seen".into()))),
                Clause::Flag("\\Flagged".into()),
            ]
        );
    }

    #[test]
    fn parses_kinds_and_people() {
        assert_eq!(
            clauses(r#"is:event IS:Contact -is:mail with:ada with:"Bob Stone""#),
            vec![
                Clause::Kind(DocumentKind::Event),
                Clause::Kind(DocumentKind::Contact),
                Clause::Not(Box::new(Clause::Kind(DocumentKind::Mail))),
                Clause::With("ada".into()),
                Clause::With("Bob Stone".into()),
            ]
        );
        assert_eq!(parse("with:"), Err(QueryError::MissingValue("with".into())));
    }

    #[test]
    fn parses_dates_in_utc() {
        assert_eq!(
            clauses("after:2025-03-14 before:2026"),
            vec![
                Clause::After(Timestamp::from_secs(1_741_910_400)),
                Clause::Before(Timestamp::from_secs(1_767_225_600)),
            ]
        );
        assert_eq!(
            clauses("before:1969-12")[0],
            Clause::Before(Timestamp::from_secs(-2_678_400))
        );
        assert_eq!(
            parse("before:yesterday"),
            Err(QueryError::InvalidDate("yesterday".into()))
        );
        assert_eq!(
            parse("after:2025-13"),
            Err(QueryError::InvalidDate("2025-13".into()))
        );
    }

    #[test]
    fn keeps_unknown_operators_as_text() {
        assert_eq!(
            clauses("re:hello https://example.com"),
            vec![
                Clause::Text("re:hello".into()),
                Clause::Text("https://example.com".into()),
            ]
        );
    }

    #[test]
    fn parses_insight_kinds() {
        assert_eq!(
            clauses(
                "has:code HAS:Link has:unsubscribe has:reservation has:order has:invoice has:parcel has:pass"
            ),
            [
                "code",
                "link",
                "unsubscribe",
                "reservation",
                "order",
                "invoice",
                "parcel",
                "pass"
            ]
            .map(|name| Clause::Insight(name.into()))
            .to_vec()
        );
        assert_eq!(
            clauses("-has:code has:attachment"),
            vec![
                Clause::Not(Box::new(Clause::Insight("code".into()))),
                Clause::HasAttachment,
            ]
        );
    }

    #[test]
    fn parses_categories() {
        let category = |value: &str| Clause::Category(value.into());
        assert_eq!(
            clauses("category:travel CATEGORY:Finance category:bulk category:Notification"),
            vec![
                category("topic:travel"),
                category("topic:finance"),
                category("kind:bulk"),
                category("kind:notification"),
            ]
        );
        for topic in Topic::ALL {
            assert_eq!(
                clauses(&format!("category:{}", topic.name())),
                [category(&format!("topic:{}", topic.name()))]
            );
        }
        assert_eq!(
            clauses(r#"-category:"travel" category:personal category:list"#),
            vec![
                Clause::Not(Box::new(category("topic:travel"))),
                category("kind:personal"),
                category("kind:list"),
            ]
        );
        assert_eq!(
            parse("category:"),
            Err(QueryError::MissingValue("category".into()))
        );
    }

    #[test]
    fn refuses_unknown_categories() {
        let error = parse("lunch category:weather").unwrap_err();
        assert_eq!(
            error,
            QueryError::UnknownValue {
                operator: "category".into(),
                value: "weather".into(),
            }
        );
        let message = error.to_string();
        assert!(
            message.contains(r#""weather" is not a category"#),
            "{message}"
        );
        assert!(
            message.contains("travel") && message.contains("bulk"),
            "{message}"
        );
        assert!(parse("-category:kind:bulk").is_err());
    }

    #[test]
    fn maps_kinds_to_categories() {
        let category = |value: &str| Clause::Category(value.into());
        assert_eq!(
            clauses("is:newsletter IS:Notification is:personal -is:list"),
            vec![
                category("kind:bulk"),
                category("kind:notification"),
                category("kind:personal"),
                Clause::Not(Box::new(category("kind:list"))),
            ]
        );
    }

    #[test]
    fn keeps_unknown_has_and_is_values_as_text() {
        assert_eq!(
            clauses("has:dog is:spam -has:codes"),
            vec![
                Clause::Text("has:dog".into()),
                Clause::Text("is:spam".into()),
                Clause::Not(Box::new(Clause::Text("has:codes".into()))),
            ]
        );
    }

    #[test]
    fn reports_missing_values() {
        assert_eq!(parse("from:"), Err(QueryError::MissingValue("from".into())));
    }

    #[test]
    fn tolerates_unclosed_quotes_and_empty_input() {
        assert_eq!(
            clauses(r#""open phrase"#),
            vec![Clause::Phrase("open phrase".into())]
        );
        assert!(clauses("   ").is_empty());
    }
}
