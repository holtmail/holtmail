use holtmail_core::ItemId;
use holtmail_pim::{CalendarObject, Normalized, Participant};

const ANSWERS: [&str; 7] = [
    "accepted",
    "declined",
    "tentative",
    "needs-action",
    "delegated",
    "completed",
    "in-process",
];

const IGNORED: [&str; 2] = ["kind", "uid"];

/// A field whose values differ between copies of an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyDifference {
    /// The field as [`holtmail_pim::diff`] names it, such as `start`, `location` or `attendee`;
    /// fields of a changed instance are prefixed `instance <RECURRENCE-ID> `.
    pub field: String,
    /// Each copy's values, in the order of the copies given; empty when the copy has none.
    pub values: Vec<(ItemId, Vec<String>)>,
}

/// One attendee's answer in each copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendeeAnswers {
    /// The attendee's address, or the calendar address as written.
    pub email: Option<String>,
    /// The name the first copy naming them gives.
    pub name: Option<String>,
    /// Each copy's lowercase `PARTSTAT` for them, in the order of the copies given; `None` when
    /// the copy does not list them or gives no answer.
    pub answers: Vec<(ItemId, Option<String>)>,
}

fn without_answer(value: &str) -> String {
    if let Some(open) = value.rfind(" (")
        && value.ends_with(')')
        && ANSWERS.contains(&&value[open + 2..value.len() - 1])
    {
        return value[..open].to_owned();
    }
    value.to_owned()
}

fn fields(normalized: &Normalized) -> Vec<(String, String)> {
    normalized
        .fields()
        .into_iter()
        .filter(|(name, _)| {
            let base = name.rsplit(' ').next().unwrap_or(name);
            !IGNORED.contains(&base)
        })
        .map(|(name, value)| {
            let base = name.rsplit(' ').next().unwrap_or(&name);
            if base == "attendee" || base == "organizer" {
                let bare = without_answer(&value);
                (name, bare)
            } else {
                (name, value)
            }
        })
        .collect()
}

/// The fields whose values differ between `copies`, in the order they first appear. Values are
/// compared as sets; attendees and the organizer by who they are, without their answers (see
/// [`answers`]); `UID` and the component kind never differ. Copies that cannot be read take no
/// part. Fewer than two readable copies give nothing.
pub fn differences(copies: &[(ItemId, &Normalized)]) -> Vec<CopyDifference> {
    let read: Vec<(ItemId, Vec<(String, String)>)> = copies
        .iter()
        .filter(|(_, n)| matches!(n, Normalized::Calendar(_)))
        .map(|(id, n)| (*id, fields(n)))
        .collect();
    if read.len() < 2 {
        return Vec::new();
    }
    let mut names: Vec<String> = Vec::new();
    for (_, fields) in &read {
        for (name, _) in fields {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    let mut out = Vec::new();
    for name in names {
        let values: Vec<(ItemId, Vec<String>)> = read
            .iter()
            .map(|(id, fields)| {
                let mut values: Vec<String> = fields
                    .iter()
                    .filter(|(n, _)| *n == name)
                    .map(|(_, v)| v.clone())
                    .collect();
                values.sort();
                values.dedup();
                (*id, values)
            })
            .collect();
        if values.windows(2).any(|w| w[0].1 != w[1].1) {
            out.push(CopyDifference {
                field: name,
                values,
            });
        }
    }
    out
}

fn calendar(n: &Normalized) -> Option<&CalendarObject> {
    match n {
        Normalized::Calendar(c) => Some(c),
        _ => None,
    }
}

fn same(a: &Participant, b: &Participant) -> bool {
    match (&a.email, &b.email) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => a.name.is_some() && a.name == b.name,
    }
}

/// Every attendee of the event itself (not of changed instances) named by any copy, in the order
/// they first appear, with each copy's answer.
pub fn answers(copies: &[(ItemId, &Normalized)]) -> Vec<AttendeeAnswers> {
    let mut people: Vec<Participant> = Vec::new();
    for (_, normalized) in copies {
        let Some(main) = calendar(normalized).and_then(|c| c.main()) else {
            continue;
        };
        for attendee in &main.attendees {
            if !people.iter().any(|p| same(p, attendee)) {
                people.push(attendee.clone());
            }
        }
    }
    people
        .into_iter()
        .map(|person| AttendeeAnswers {
            answers: copies
                .iter()
                .map(|(id, normalized)| {
                    let answer = calendar(normalized)
                        .and_then(|c| c.main())
                        .and_then(|e| e.attendees.iter().find(|a| same(a, &person)))
                        .and_then(|a| a.partstat.as_ref())
                        .map(|s| s.to_ascii_lowercase());
                    (*id, answer)
                })
                .collect(),
            email: person.email,
            name: person.name,
        })
        .collect()
}
