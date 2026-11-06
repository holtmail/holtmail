use serde::{Deserialize, Serialize};

use crate::diff::person;
use crate::event::sort_components;
use crate::{CalendarObject, Change, Event, Normalized, Participant, diff};

/// The iTIP method of a calendar object sent by mail (RFC 5546, RFC 6047).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// `PUBLISH`, and an object without `METHOD`: the event as its sender has it.
    Publish,
    /// `REQUEST`: an invitation or an update of one.
    Request,
    /// `REPLY`: an attendee's answer.
    Reply,
    /// `ADD`: new instances of a recurring event.
    Add,
    /// `CANCEL`: the event, or some instances, called off.
    Cancel,
    /// `REFRESH`: an attendee asking for the latest version.
    Refresh,
    /// `COUNTER`: an attendee proposing a change.
    Counter,
    /// `DECLINECOUNTER`: the organizer turning a proposal down.
    DeclineCounter,
    /// Any other method; recorded, never applied.
    Unknown,
}

impl Method {
    /// The method of an object whose `METHOD` is `written` (any case); `None` is [`Method::Publish`].
    pub fn of(written: Option<&str>) -> Self {
        let Some(written) = written else {
            return Self::Publish;
        };
        match written.trim().to_ascii_uppercase().as_str() {
            "PUBLISH" => Self::Publish,
            "REQUEST" => Self::Request,
            "REPLY" => Self::Reply,
            "ADD" => Self::Add,
            "CANCEL" => Self::Cancel,
            "REFRESH" => Self::Refresh,
            "COUNTER" => Self::Counter,
            "DECLINECOUNTER" => Self::DeclineCounter,
            _ => Self::Unknown,
        }
    }

    /// Whether the method speaks for an attendee or answers one (`REPLY`, `REFRESH`, `COUNTER`,
    /// `DECLINECOUNTER`, or unknown) rather than changing the event itself.
    pub fn is_answer(&self) -> bool {
        matches!(
            self,
            Self::Reply | Self::Refresh | Self::Counter | Self::DeclineCounter | Self::Unknown
        )
    }
}

fn instance_key(event: &Event) -> Option<String> {
    event.key().and_then(|k| k.recurrence_id)
}

fn full(method: Method, calendar: &CalendarObject) -> bool {
    matches!(method, Method::Publish | Method::Request)
        && calendar
            .components
            .iter()
            .any(|e| e.recurrence_id.is_none())
}

fn upsert(result: &mut CalendarObject, event: &Event) {
    let key = instance_key(event);
    match result
        .components
        .iter_mut()
        .find(|e| instance_key(e) == key)
    {
        Some(slot) => *slot = event.clone(),
        None => result.components.push(event.clone()),
    }
}

/// The event as the mail about it describes it, from its versions in order (see
/// [`crate::Method`]), oldest first.
///
/// - The base is the latest `PUBLISH` or `REQUEST` holding the event itself (a component without
///   `RECURRENCE-ID`); without one, the latest readable version of any method.
/// - Later `REQUEST`s or `PUBLISH`es of single instances replace or add those instances; later
///   `ADD`s add instances the base lacks; later `CANCEL`s mark the event, or the instances they
///   name, `CANCELLED`. Versions before the base change nothing of its fields.
/// - Each attendee's `PARTSTAT` is the last one said in any `REQUEST`, `PUBLISH` or `REPLY`, so a
///   reply after an update shows on it, and an update after a reply that resets the answer wins.
///   Attendees known only from a reply are added.
/// - `COUNTER`, `DECLINECOUNTER`, `REFRESH` and unknown methods change nothing.
///
/// Unreadable versions are skipped; with none readable the result is [`Normalized::Unreadable`].
/// The same versions always give the same result.
pub fn merge(versions: &[(Method, &Normalized)]) -> Normalized {
    let calendars: Vec<(Method, &CalendarObject)> = versions
        .iter()
        .filter_map(|(method, n)| match n {
            Normalized::Calendar(c) => Some((*method, c)),
            _ => None,
        })
        .collect();
    let Some(base_at) = calendars
        .iter()
        .rposition(|(m, c)| full(*m, c))
        .or_else(|| calendars.len().checked_sub(1))
    else {
        return Normalized::Unreadable;
    };
    let mut result = calendars[base_at].1.clone();
    let mut answers: Vec<(Option<String>, Participant)> = Vec::new();
    let mut answer = |key: Option<String>, attendee: &Participant| {
        if attendee.email.is_none() || attendee.partstat.is_none() {
            return;
        }
        answers.retain(|(k, a)| !(k == &key && a.email == attendee.email));
        answers.push((key, attendee.clone()));
    };
    for (at, (method, calendar)) in calendars.iter().enumerate() {
        let later = at > base_at;
        for event in &calendar.components {
            match method {
                Method::Publish | Method::Request | Method::Reply => {
                    if later && *method != Method::Reply {
                        upsert(&mut result, event);
                    }
                    for attendee in &event.attendees {
                        answer(instance_key(event), attendee);
                    }
                }
                Method::Add if later => {
                    let key = instance_key(event);
                    if !result.components.iter().any(|e| instance_key(e) == key) {
                        result.components.push(event.clone());
                    }
                }
                Method::Cancel if later => {
                    let key = instance_key(event);
                    match result
                        .components
                        .iter_mut()
                        .find(|e| instance_key(e) == key)
                    {
                        Some(slot) => slot.status = Some("CANCELLED".into()),
                        None => {
                            let mut cancelled = event.clone();
                            cancelled.status = Some("CANCELLED".into());
                            result.components.push(cancelled);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for (key, said) in answers {
        let Some(event) = result
            .components
            .iter_mut()
            .find(|e| instance_key(e) == key)
        else {
            continue;
        };
        match event.attendees.iter_mut().find(|a| a.email == said.email) {
            Some(attendee) => attendee.partstat = said.partstat,
            None => event.attendees.push(said),
        }
    }
    for (_, calendar) in &calendars {
        for zone in &calendar.zones {
            if !result.zones.iter().any(|z| z.tzid == zone.tzid) {
                result.zones.push(zone.clone());
            }
        }
    }
    sort_components(&mut result.components);
    Normalized::Calendar(result)
}

/// What each version changed, for a timeline of versions in order:
///
/// - an answer-like version ([`Method::is_answer`]) lists the attendees it speaks for with their
///   `PARTSTAT` as `answer` changes;
/// - a `CANCEL` gives one `status` change to `CANCELLED` for the event or each instance it names,
///   since a cancellation carries only the fields that identify what it cancels;
/// - any other version is compared by [`diff`] with the latest version before it that was neither
///   an answer nor a cancellation; the first gives no changes.
///
/// Changes of an instance are prefixed `instance <RECURRENCE-ID> `.
pub fn timeline(versions: &[(Method, &Normalized)]) -> Vec<Vec<Change>> {
    let mut previous: Option<&Normalized> = None;
    versions
        .iter()
        .map(|(method, normalized)| {
            if method.is_answer() {
                return answers(normalized);
            }
            if *method == Method::Cancel {
                return cancellations(previous, normalized);
            }
            let changes = previous.map(|p| diff(p, normalized)).unwrap_or_default();
            previous = Some(normalized);
            changes
        })
        .collect()
}

fn prefix(event: &Event) -> String {
    instance_key(event)
        .map(|k| format!("instance {k} "))
        .unwrap_or_default()
}

fn cancellations(previous: Option<&Normalized>, cancel: &Normalized) -> Vec<Change> {
    let Normalized::Calendar(calendar) = cancel else {
        return Vec::new();
    };
    let before = |event: &Event| match previous {
        Some(Normalized::Calendar(p)) => p
            .components
            .iter()
            .find(|e| instance_key(e) == instance_key(event))
            .and_then(|e| e.status.clone()),
        _ => None,
    };
    calendar
        .components
        .iter()
        .map(|event| Change {
            field: format!("{}status", prefix(event)),
            before: before(event),
            after: Some("CANCELLED".into()),
        })
        .collect()
}

fn answers(normalized: &Normalized) -> Vec<Change> {
    let Normalized::Calendar(calendar) = normalized else {
        return Vec::new();
    };
    calendar
        .components
        .iter()
        .flat_map(|event| {
            let tag = prefix(event);
            event.attendees.iter().map(move |a| Change {
                field: format!("{tag}answer"),
                before: None,
                after: Some(person(a)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ItemKind, normalize};

    fn cal(method: &str, body: &str) -> Normalized {
        let text = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\nUID:plan@example.com\r\n{body}END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        normalize(ItemKind::Event, text.as_bytes())
    }

    fn main(n: &Normalized) -> &Event {
        match n {
            Normalized::Calendar(c) => c.main().unwrap(),
            other => panic!("{other:?}"),
        }
    }

    const ORGANIZER: &str = "ORGANIZER;CN=Ada Quill:mailto:ada@example.com\r\n";

    #[test]
    fn merges_updates_answers_and_a_cancel() {
        let first = cal(
            "REQUEST",
            &format!(
                "SEQUENCE:0\r\nSUMMARY:Plan\r\nDTSTART:20250310T090000Z\r\n{ORGANIZER}ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\n"
            ),
        );
        let moved = cal(
            "REQUEST",
            &format!(
                "SEQUENCE:1\r\nSUMMARY:Plan\r\nDTSTART:20250310T100000Z\r\n{ORGANIZER}ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\n"
            ),
        );
        let reply = cal(
            "REPLY",
            &format!(
                "SEQUENCE:1\r\n{ORGANIZER}ATTENDEE;PARTSTAT=ACCEPTED;CN=Bob:mailto:bob@example.org\r\n"
            ),
        );
        let cancel = cal("CANCEL", &format!("SEQUENCE:2\r\n{ORGANIZER}"));
        let versions = [
            (Method::Request, &first),
            (Method::Request, &moved),
            (Method::Reply, &reply),
        ];
        let merged = merge(&versions);
        let event = main(&merged);
        assert_eq!(event.start.as_ref().unwrap().value, "2025-03-10T10:00:00Z");
        assert_eq!(event.summary.as_deref(), Some("Plan"));
        assert_eq!(event.attendees.len(), 1);
        assert_eq!(event.attendees[0].partstat.as_deref(), Some("ACCEPTED"));
        assert_eq!(event.status, None);

        let all = [
            (Method::Request, &first),
            (Method::Request, &moved),
            (Method::Reply, &reply),
            (Method::Cancel, &cancel),
        ];
        let merged = merge(&all);
        let event = main(&merged);
        assert_eq!(event.status.as_deref(), Some("CANCELLED"));
        assert_eq!(event.summary.as_deref(), Some("Plan"));

        let steps = timeline(&all);
        assert!(steps[0].is_empty());
        assert!(
            steps[1]
                .iter()
                .any(|c| c.field == "start" && c.after.as_deref() == Some("2025-03-10T10:00:00Z"))
        );
        assert_eq!(steps[2][0].field, "answer");
        assert_eq!(
            steps[2][0].after.as_deref(),
            Some("Bob <bob@example.org> (accepted)")
        );
        assert_eq!(
            steps[3],
            [Change {
                field: "status".into(),
                before: None,
                after: Some("CANCELLED".into()),
            }]
        );
    }

    #[test]
    fn a_later_update_resets_an_earlier_answer_and_counters_change_nothing() {
        let first = cal(
            "REQUEST",
            "SEQUENCE:0\r\nSUMMARY:One\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\n",
        );
        let reply = cal(
            "REPLY",
            "SEQUENCE:0\r\nATTENDEE;PARTSTAT=DECLINED:mailto:bob@example.org\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:cleo@example.org\r\n",
        );
        let counter = cal("COUNTER", "SEQUENCE:0\r\nSUMMARY:Other\r\n");
        let update = cal(
            "REQUEST",
            "SEQUENCE:1\r\nSUMMARY:Two\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\n",
        );
        let merged = merge(&[
            (Method::Request, &first),
            (Method::Reply, &reply),
            (Method::Counter, &counter),
            (Method::Request, &update),
        ]);
        let event = main(&merged);
        assert_eq!(event.summary.as_deref(), Some("Two"));
        let states: Vec<_> = event
            .attendees
            .iter()
            .map(|a| (a.email.clone().unwrap(), a.partstat.clone().unwrap()))
            .collect();
        assert_eq!(
            states,
            [
                ("bob@example.org".to_owned(), "NEEDS-ACTION".to_owned()),
                ("cleo@example.org".to_owned(), "ACCEPTED".to_owned())
            ]
        );
    }

    #[test]
    fn a_reply_alone_is_the_event_and_instances_are_cancelled_one_by_one() {
        let reply = cal(
            "REPLY",
            "SUMMARY:Only reply\r\nATTENDEE;PARTSTAT=TENTATIVE:mailto:bob@example.org\r\n",
        );
        assert_eq!(
            main(&merge(&[(Method::Reply, &reply)])).summary.as_deref(),
            Some("Only reply")
        );
        let series = cal(
            "REQUEST",
            "SUMMARY:Weekly\r\nDTSTART:20250310T090000Z\r\nRRULE:FREQ=WEEKLY\r\n",
        );
        let one_off = normalize(
            ItemKind::Event,
            b"BEGIN:VCALENDAR\r\nMETHOD:CANCEL\r\nBEGIN:VEVENT\r\nUID:plan@example.com\r\nRECURRENCE-ID:20250317T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        );
        let Normalized::Calendar(merged) =
            merge(&[(Method::Request, &series), (Method::Cancel, &one_off)])
        else {
            panic!("not a calendar");
        };
        assert_eq!(merged.components.len(), 2);
        assert_eq!(merged.components[0].status, None);
        assert_eq!(merged.components[1].status.as_deref(), Some("CANCELLED"));
        assert_eq!(
            merge(&[(Method::Request, &Normalized::Unreadable)]),
            Normalized::Unreadable
        );
    }

    #[test]
    fn reads_methods() {
        assert_eq!(Method::of(None), Method::Publish);
        assert_eq!(Method::of(Some(" declinecounter")), Method::DeclineCounter);
        assert_eq!(Method::of(Some("X-OTHER")), Method::Unknown);
        assert!(Method::Reply.is_answer());
        assert!(!Method::Cancel.is_answer());
    }
}
