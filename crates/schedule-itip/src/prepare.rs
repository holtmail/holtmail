use bytes::Bytes;
use holtmail_collection::Partstat;
use holtmail_pim::ItemKind;
use holtmail_pim_edit::{Class, answered, classify};
use holtmail_schedule::{ClientPut, Prepared, Role, ScheduleError};

use crate::ical::{Comp, Node, param, parse, people, set_param, split};

const CLIENT_PARAMS: [&str; 2] = ["SCHEDULE-STATUS", "SCHEDULE-FORCE-SEND"];

pub(crate) fn normalized(own: &[String]) -> Vec<String> {
    own.iter()
        .filter_map(|a| holtmail_pim::normalize_email(a))
        .collect()
}

pub(crate) fn role(cal: &Comp, own: &[String]) -> Role {
    let mut attendee = false;
    for event in cal.events() {
        if let Some((organizer, _)) = people(event, "ORGANIZER").into_iter().next() {
            if own.contains(&organizer) {
                return Role::Organizer;
            }
            attendee |= people(event, "ATTENDEE")
                .iter()
                .any(|(a, _)| own.contains(a));
        }
    }
    if attendee { Role::Attendee } else { Role::None }
}

pub(crate) fn strip(cal: &mut Comp) -> bool {
    let mut changed = false;
    for name in ["ATTENDEE", "ORGANIZER"] {
        changed |= cal.edit_params(name, &mut |_, params| {
            params.retain(|(k, _)| !CLIENT_PARAMS.contains(&k.as_str()));
        });
    }
    changed
}

pub(crate) fn own_partstat(event: &Comp, me: &str) -> Option<String> {
    people(event, "ATTENDEE")
        .into_iter()
        .find(|(a, _)| a == me)
        .map(|(_, p)| {
            param(&p, "PARTSTAT")
                .unwrap_or("NEEDS-ACTION")
                .to_ascii_uppercase()
        })
}

fn exdates(event: &Comp) -> Vec<String> {
    event
        .lines()
        .map(|l| split(l))
        .filter(|(n, _, _)| n == "EXDATE")
        .flat_map(|(_, _, v)| {
            v.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn personal(item: &Node) -> bool {
    match item {
        Node::Comp(c) => c.name == "VALARM",
        Node::Line(l) => {
            let (name, _, _) = split(l);
            matches!(name.as_str(), "TRANSP" | "PERCENT-COMPLETE" | "COMPLETED")
                || ["X-MOZ-", "X-APPLE-", "X-EVOLUTION-"]
                    .iter()
                    .any(|p| name.starts_with(p))
        }
    }
}

fn answer(acc: Vec<u8>, me: &str, partstat: &str, rid: Option<&str>) -> Vec<u8> {
    let Some(partstat) = Partstat::parse(partstat) else {
        return acc;
    };
    answered(&acc, me, partstat, rid).unwrap_or(acc)
}

/// The attendee's write over `previous`, keeping only what RFC 6638 section 3.2.2.1 allows.
fn reduce(previous: &[u8], body: &Comp, own: &[String]) -> Option<Vec<u8>> {
    let before = parse(previous)?;
    let me = before
        .events()
        .flat_map(|e| people(e, "ATTENDEE"))
        .map(|(a, _)| a)
        .find(|a| own.contains(a))?;
    let series_of = |cal: &Comp| cal.events().find(|e| e.rid().is_empty()).cloned();
    let old_series = series_of(&before);
    let mut acc = previous.to_vec();
    if let Some(series) = series_of(body) {
        if let Some(p) = own_partstat(&series, &me)
            && old_series.as_ref().and_then(|s| own_partstat(s, &me)) != Some(p.clone())
        {
            acc = answer(acc, &me, &p, None);
        }
        let old = old_series.as_ref().map(exdates).unwrap_or_default();
        for value in exdates(&series).into_iter().filter(|v| !old.contains(v)) {
            acc = answer(acc, &me, "DECLINED", Some(&value));
        }
    }
    for instance in body.events().filter(|e| !e.rid().is_empty()) {
        let rid = instance.rid();
        let Some(p) = own_partstat(instance, &me) else {
            continue;
        };
        let held = before
            .events()
            .find(|e| e.rid() == rid)
            .and_then(|e| own_partstat(e, &me));
        if held.as_deref() != Some(p.as_str()) {
            acc = answer(acc, &me, &p, Some(&rid));
        }
    }
    let mut out = parse(&acc)?;
    for event in out.events_mut() {
        let rid = event.rid();
        let Some(theirs) = body.events().find(|e| e.rid() == rid) else {
            continue;
        };
        event.items.retain(|i| !personal(i));
        event
            .items
            .extend(theirs.items.iter().filter(|i| personal(i)).cloned());
    }
    Some(out.to_bytes())
}

/// Every attendee's `PARTSTAT` but the user's, from the matching event of `previous`: the
/// answers merged since the client last read the copy. True when something changed.
fn keep_answers(cal: &mut Comp, previous: &[u8], own: &[String]) -> bool {
    let Some(before) = parse(previous) else {
        return false;
    };
    let mut changed = false;
    for event in cal.events_mut() {
        let rid = event.rid();
        let Some(old) = before
            .events()
            .find(|e| e.rid() == rid)
            .or_else(|| before.events().find(|e| e.rid().is_empty()))
        else {
            continue;
        };
        let answers: Vec<(String, String)> = people(old, "ATTENDEE")
            .into_iter()
            .filter(|(a, _)| !own.contains(a))
            .filter_map(|(a, p)| param(&p, "PARTSTAT").map(|v| (a, v.to_owned())))
            .collect();
        let edit = |value: &str, params: &mut crate::ical::Params| {
            let Some(address) = holtmail_pim::normalize_email(value) else {
                return;
            };
            if let Some((_, answer)) = answers.iter().find(|(a, _)| *a == address) {
                set_param(params, "PARTSTAT", Some(answer));
            }
        };
        for item in &mut event.items {
            if let Node::Line(line) = item {
                let (name, mut params, value) = split(line);
                if name != "ATTENDEE" {
                    continue;
                }
                let before = params.clone();
                edit(&value, &mut params);
                if params != before {
                    *line = crate::ical::join(&name, &params, &value);
                    changed = true;
                }
            }
        }
    }
    changed
}

pub(crate) fn prepare(put: &ClientPut) -> Result<Prepared, ScheduleError> {
    let mut cal = parse(&put.body)
        .ok_or_else(|| ScheduleError::Invalid("not a readable iCalendar event".into()))?;
    let stripped = strip(&mut cal);
    let own = normalized(&put.own);
    let role = role(&cal, &own);
    let kept = match (role, &put.previous, put.keep_answers) {
        (Role::Organizer, Some(previous), true) => keep_answers(&mut cal, previous, &own),
        _ => false,
    };
    let clean = if stripped || kept {
        Bytes::from(cal.to_bytes())
    } else {
        put.body.clone()
    };
    let previous = match (role, &put.previous) {
        (Role::Attendee, Some(previous)) => previous,
        _ => {
            return Ok(Prepared {
                body: clean,
                role,
                stripped,
                reduced: false,
            });
        }
    };
    let allowed = classify(ItemKind::Event, Some(previous), &clean, &own)
        .is_ok_and(|c| matches!(c.class, Class::Noop | Class::Personal | Class::Reply));
    if allowed {
        return Ok(Prepared {
            body: clean,
            role,
            stripped,
            reduced: false,
        });
    }
    let reduced = reduce(previous, &cal, &own).unwrap_or_else(|| previous.to_vec());
    Ok(Prepared {
        body: Bytes::from(reduced),
        role,
        stripped,
        reduced: true,
    })
}
