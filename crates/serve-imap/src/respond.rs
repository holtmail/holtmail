//! Building responses from plain values.

use std::num::{NonZeroU32, NonZeroU64};

use imap_types::IntoStatic;
use imap_types::core::{Atom, Tag, Vec1};
use imap_types::extensions::uidplus::{UidElement, UidSet};
use imap_types::flag::{Flag, FlagFetch, FlagPerm};
use imap_types::response::{Capability, Code, CodeOther, Status};
use imap_types::sequence::{SeqOrUid, Sequence, SequenceSet};

/// A tagged `OK`, `NO` or `BAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Ok,
    No,
    Bad,
}

/// A tagged status; text that is not valid IMAP text falls back to a fixed one.
pub(crate) fn tagged(
    tag: &Tag<'static>,
    kind: Kind,
    code: Option<Code<'static>>,
    text: impl Into<String>,
) -> Status<'static> {
    let text = text.into();
    let make = |code: Option<Code<'static>>, text: String| match kind {
        Kind::Ok => Status::ok(Some(tag.clone()), code, text),
        Kind::No => Status::no(Some(tag.clone()), code, text),
        Kind::Bad => Status::bad(Some(tag.clone()), code, text),
    };
    make(code.clone(), text).unwrap_or_else(|_| make(code, "done".into()).expect("fixed text"))
}

/// An untagged `OK` with a code.
pub(crate) fn untagged_ok(code: Option<Code<'static>>, text: &str) -> Status<'static> {
    Status::ok(None, code, text.to_string()).expect("fixed text")
}

/// A `BYE`.
pub(crate) fn bye(text: &str) -> Status<'static> {
    Status::bye(None, text.to_string()).expect("fixed text")
}

/// A response code this crate's types do not name, such as `CANNOT` (RFC 5530).
pub(crate) fn code(text: &'static str) -> Code<'static> {
    Code::Other(CodeOther::unvalidated(text.as_bytes()))
}

/// A capability by its name.
pub(crate) fn capability(name: &str) -> Capability<'static> {
    Capability::from(Atom::try_from(name.to_string()).expect("capability atom"))
}

/// A flag as the mail view spells it; `None` for one IMAP cannot carry.
pub(crate) fn flag(name: &str) -> Option<Flag<'static>> {
    Flag::try_from(name).ok().map(IntoStatic::into_static)
}

/// Flags for a `FETCH` response.
pub(crate) fn fetch_flags(flags: &[String]) -> Vec<FlagFetch<'static>> {
    flags
        .iter()
        .filter_map(|f| flag(f))
        .map(FlagFetch::Flag)
        .collect()
}

/// Flags for `PERMANENTFLAGS`, with `\*` when `keywords`.
pub(crate) fn permanent_flags(flags: &[&str], keywords: bool) -> Vec<FlagPerm<'static>> {
    let mut out: Vec<FlagPerm<'static>> = flags
        .iter()
        .filter_map(|f| flag(f))
        .map(FlagPerm::Flag)
        .collect();
    if keywords {
        out.push(FlagPerm::Asterisk);
    }
    out
}

/// The flag as the mail view spells it.
pub(crate) fn flag_name(flag: &Flag<'_>) -> String {
    flag.to_string()
}

pub(crate) fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN)
}

pub(crate) fn nz64(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN)
}

/// Ascending values folded into ranges.
pub(crate) fn ranges(values: &[u32]) -> Vec<(u32, u32)> {
    let mut sorted: Vec<u32> = values.iter().copied().filter(|v| *v > 0).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for value in sorted {
        match out.last_mut() {
            Some((_, end)) if *end + 1 == value => *end = value,
            _ => out.push((value, value)),
        }
    }
    out
}

/// A non-empty set of numbers as a sequence set; `None` when empty.
pub(crate) fn sequence_set(values: &[u32]) -> Option<SequenceSet> {
    let sequences: Vec<Sequence> = ranges(values)
        .into_iter()
        .map(|(a, b)| {
            if a == b {
                Sequence::Single(SeqOrUid::Value(nz(a)))
            } else {
                Sequence::Range(SeqOrUid::Value(nz(a)), SeqOrUid::Value(nz(b)))
            }
        })
        .collect();
    Vec1::try_from(sequences).ok().map(SequenceSet)
}

/// A non-empty set of UIDs for `COPYUID`, in the order given (the two lists of `COPYUID` pair
/// up by position, so they are not sorted).
pub(crate) fn uid_set(values: &[u32]) -> Option<UidSet> {
    let elements: Vec<UidElement> = values.iter().map(|v| UidElement::Single(nz(*v))).collect();
    Vec1::try_from(elements).ok().map(UidSet)
}

/// Resolves a sequence set against the largest number in use (`*`); ranges are inclusive in
/// either order. Numbers above `largest` are kept, so a UID set may name ones that do not exist.
pub(crate) fn resolve(set: &SequenceSet, largest: u32) -> Vec<(u32, u32)> {
    let value = |v: &SeqOrUid| match v {
        SeqOrUid::Value(n) => n.get(),
        SeqOrUid::Asterisk => largest,
    };
    set.0
        .as_ref()
        .iter()
        .map(|sequence| match sequence {
            Sequence::Single(v) => (value(v), value(v)),
            Sequence::Range(a, b) => {
                let (a, b) = (value(a), value(b));
                (a.min(b), a.max(b))
            }
        })
        .collect()
}

pub(crate) fn contains(ranges: &[(u32, u32)], value: u32) -> bool {
    ranges.iter().any(|(a, b)| (*a..=*b).contains(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_ranges() {
        assert_eq!(ranges(&[5, 1, 2, 3, 9, 10]), vec![(1, 3), (5, 5), (9, 10)]);
        assert!(sequence_set(&[]).is_none());
    }

    #[test]
    fn resolves_stars() {
        let set: SequenceSet = "1:*,7,9:3".try_into().unwrap();
        assert_eq!(resolve(&set, 5), vec![(1, 5), (7, 7), (3, 9)]);
    }
}
