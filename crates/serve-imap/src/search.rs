//! `SEARCH` keys as mail view filters.

use holtmail_core::Timestamp;
use holtmail_mailview::{Filter, TextField};
use imap_types::core::AString;
use imap_types::datetime::NaiveDate;
use imap_types::search::SearchKey;

use crate::respond;

/// Translates search keys. `uids_of` turns a message sequence set into UID ranges for the
/// selected mailbox; `largest_uid` stands for `*` in a UID set. `\Recent` is never set, so
/// `RECENT` and `NEW` match nothing and `OLD` everything. Also says whether a `MODSEQ` key was
/// used, which makes the answer carry the highest `MODSEQ` found.
pub(crate) fn filter(
    keys: &[SearchKey<'_>],
    uids_of: &dyn Fn(&imap_types::sequence::SequenceSet) -> Vec<(u32, u32)>,
    largest_uid: u32,
) -> (Filter, bool) {
    let mut modseq = false;
    let filters = keys
        .iter()
        .map(|key| one(key, uids_of, largest_uid, &mut modseq))
        .collect();
    (Filter::And(filters), modseq)
}

fn text(field: TextField, value: &AString<'_>) -> Filter {
    Filter::Text(field, String::from_utf8_lossy(value.as_ref()).into_owned())
}

fn flag(name: &str) -> Filter {
    Filter::Flag(name.into())
}

fn not(filter: Filter) -> Filter {
    Filter::Not(Box::new(filter))
}

fn none() -> Filter {
    Filter::Or(Vec::new())
}

/// Midnight UTC at the start of a day.
fn day(date: &NaiveDate) -> Timestamp {
    let date: &chrono::NaiveDate = date.as_ref();
    Timestamp::from_secs(date.and_time(chrono::NaiveTime::MIN).and_utc().timestamp())
}

fn next_day(date: &NaiveDate) -> Timestamp {
    Timestamp::from_secs(day(date).as_secs() + 86_400)
}

fn one(
    key: &SearchKey<'_>,
    uids_of: &dyn Fn(&imap_types::sequence::SequenceSet) -> Vec<(u32, u32)>,
    largest_uid: u32,
    modseq: &mut bool,
) -> Filter {
    use SearchKey as K;
    match key {
        K::And(keys) => Filter::And(
            keys.as_ref()
                .iter()
                .map(|k| one(k, uids_of, largest_uid, modseq))
                .collect(),
        ),
        K::Or(a, b) => Filter::Or(vec![
            one(a, uids_of, largest_uid, modseq),
            one(b, uids_of, largest_uid, modseq),
        ]),
        K::Not(inner) => not(one(inner, uids_of, largest_uid, modseq)),
        K::SequenceSet(set) => Filter::Uids(uids_of(set)),
        K::Uid(set) => Filter::Uids(respond::resolve(set, largest_uid)),
        K::All | K::Old => Filter::All,
        K::New | K::Recent => none(),
        K::Answered => flag("\\Answered"),
        K::Deleted => flag("\\Deleted"),
        K::Draft => flag("\\Draft"),
        K::Flagged => flag("\\Flagged"),
        K::Seen => flag("\\Seen"),
        K::Unanswered => not(flag("\\Answered")),
        K::Undeleted => not(flag("\\Deleted")),
        K::Undraft => not(flag("\\Draft")),
        K::Unflagged => not(flag("\\Flagged")),
        K::Unseen => not(flag("\\Seen")),
        K::Keyword(atom) => flag(atom.as_ref()),
        K::Unkeyword(atom) => not(flag(atom.as_ref())),
        K::Before(date) => Filter::ReceivedBefore(day(date)),
        K::Since(date) => Filter::ReceivedSince(day(date)),
        K::On(date) => Filter::And(vec![
            Filter::ReceivedSince(day(date)),
            Filter::ReceivedBefore(next_day(date)),
        ]),
        K::SentBefore(date) => Filter::SentBefore(day(date)),
        K::SentSince(date) => Filter::SentSince(day(date)),
        K::SentOn(date) => Filter::And(vec![
            Filter::SentSince(day(date)),
            Filter::SentBefore(next_day(date)),
        ]),
        K::Larger(n) => Filter::Larger(u64::from(*n)),
        K::Smaller(n) => Filter::Smaller(u64::from(*n)),
        K::Bcc(v) => text(TextField::Bcc, v),
        K::Body(v) => text(TextField::Body, v),
        K::Cc(v) => text(TextField::Cc, v),
        K::From(v) => text(TextField::From, v),
        K::Subject(v) => text(TextField::Subject, v),
        K::Text(v) => text(TextField::Any, v),
        K::To(v) => text(TextField::To, v),
        K::Header(name, value) => text(
            TextField::Header(String::from_utf8_lossy(name.as_ref()).into_owned()),
            value,
        ),
        K::ModSequence { modseq: value, .. } => {
            *modseq = true;
            Filter::ModseqSince(*value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_become_filters() {
        let keys = vec![
            SearchKey::Unseen,
            SearchKey::From(AString::try_from("ada").unwrap()),
            SearchKey::Uid("5:*".try_into().unwrap()),
        ];
        let (filter, modseq) = filter(&keys, &|_| Vec::new(), 9);
        assert!(!modseq);
        assert_eq!(
            filter,
            Filter::And(vec![
                Filter::Not(Box::new(Filter::Flag("\\Seen".into()))),
                Filter::Text(TextField::From, "ada".into()),
                Filter::Uids(vec![(5, 9)]),
            ])
        );
    }

    #[test]
    fn days_start_at_midnight_utc() {
        let date = NaiveDate::unvalidated(chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        assert_eq!(day(&date).as_secs(), 1_793_577_600);
    }
}
