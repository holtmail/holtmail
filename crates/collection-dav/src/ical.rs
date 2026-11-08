use holtmail_collection::Partstat;

use crate::date::days_from_civil;

#[derive(Debug, Clone)]
struct Line {
    raw: String,
    logical: String,
}

struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn parse(logical: &str) -> Self {
        let mut name = String::new();
        let mut params: Vec<(String, String)> = Vec::new();
        let mut value_at = logical.len();
        let mut in_params = false;
        let mut in_value = false;
        let mut quoted = false;
        for (i, c) in logical.char_indices() {
            if !quoted && c == ':' {
                value_at = i + 1;
                break;
            }
            if !quoted && c == ';' {
                in_params = true;
                in_value = false;
                params.push((String::new(), String::new()));
                continue;
            }
            match params.last_mut() {
                Some((_, value)) if in_params && in_value => {
                    if c == '"' {
                        quoted = !quoted;
                    }
                    value.push(c);
                }
                Some(_) if in_params && c == '=' => in_value = true,
                Some((pname, _)) if in_params => pname.push(c),
                _ => name.push(c),
            }
        }
        Self {
            name,
            params,
            value: logical.get(value_at..).unwrap_or_default().to_owned(),
        }
    }

    fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim_matches('"'))
    }

    fn set_param(&mut self, name: &str, value: Option<&str>) {
        let at = self
            .params
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name));
        match (at, value) {
            (Some(i), Some(v)) => self.params[i].1 = v.to_owned(),
            (None, Some(v)) => self.params.push((name.to_owned(), v.to_owned())),
            (Some(i), None) => {
                self.params.remove(i);
            }
            (None, None) => {}
        }
    }

    fn render(&self) -> String {
        let mut out = self.name.clone();
        for (name, value) in &self.params {
            out.push(';');
            out.push_str(name);
            out.push('=');
            out.push_str(value);
        }
        out.push(':');
        out.push_str(&self.value);
        out
    }
}

struct Calendar {
    lines: Vec<Line>,
    eol: &'static str,
}

impl Calendar {
    fn parse(text: &str) -> Self {
        let eol = if text.contains("\r\n") || !text.contains('\n') {
            "\r\n"
        } else {
            "\n"
        };
        let mut lines: Vec<Line> = Vec::new();
        for physical in text.split_inclusive('\n') {
            let content = physical.trim_end_matches(['\r', '\n']);
            match lines.last_mut() {
                Some(last) if content.starts_with([' ', '\t']) => {
                    last.raw.push_str(physical);
                    last.logical.push_str(&content[1..]);
                }
                _ => lines.push(Line {
                    raw: physical.to_owned(),
                    logical: content.to_owned(),
                }),
            }
        }
        if let Some(last) = lines.last_mut()
            && !last.raw.ends_with('\n')
        {
            last.raw.push_str(eol);
        }
        Self { lines, eol }
    }

    fn line(&self, logical: String) -> Line {
        Line {
            raw: fold(&logical, self.eol),
            logical,
        }
    }

    fn render(&self) -> String {
        self.lines.iter().map(|l| l.raw.as_str()).collect()
    }

    /// The `VEVENT`s directly inside `VCALENDAR`, as line ranges from `BEGIN` to `END`.
    fn events(&self) -> Vec<(usize, usize)> {
        let mut stack: Vec<String> = Vec::new();
        let mut out = Vec::new();
        let mut start = None;
        for (i, line) in self.lines.iter().enumerate() {
            let prop = Prop::parse(&line.logical);
            if prop.is("BEGIN") {
                let name = prop.value.trim().to_ascii_uppercase();
                if name == "VEVENT" && stack.len() == 1 {
                    start = Some(i);
                }
                stack.push(name);
            } else if prop.is("END") {
                let name = stack.pop().unwrap_or_default();
                if name == "VEVENT"
                    && stack.len() == 1
                    && let Some(s) = start.take()
                {
                    out.push((s, i));
                }
            }
        }
        out
    }

    /// The indexes of an event's own property lines, leaving out nested components.
    fn own(&self, (start, end): (usize, usize)) -> Vec<usize> {
        let mut depth = 0usize;
        let mut out = Vec::new();
        for i in start + 1..end {
            let prop = Prop::parse(&self.lines[i].logical);
            if prop.is("BEGIN") {
                depth += 1;
            } else if prop.is("END") {
                depth = depth.saturating_sub(1);
            } else if depth == 0 {
                out.push(i);
            }
        }
        out
    }

    fn own_prop(&self, event: (usize, usize), name: &str) -> Option<Prop> {
        self.own(event)
            .into_iter()
            .map(|i| Prop::parse(&self.lines[i].logical))
            .find(|p| p.is(name))
    }
}

fn fold(logical: &str, eol: &str) -> String {
    let mut out = String::with_capacity(logical.len() + 8);
    let mut width = 0;
    for c in logical.chars() {
        if width + c.len_utf8() > 75 {
            out.push_str(eol);
            out.push(' ');
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out.push_str(eol);
    out
}

fn bare(address: &str) -> &str {
    let address = address.trim();
    match address.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("mailto:") => &address[7..],
        _ => address,
    }
}

fn is_own(value: &str, addresses: &[String]) -> bool {
    let value = bare(value);
    addresses
        .iter()
        .any(|a| bare(a).eq_ignore_ascii_case(value))
}

fn stamp(value: &str) -> Option<(i64, bool, bool)> {
    let value = value.trim();
    let num = |r: std::ops::Range<usize>| value.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(num(0..4)?, num(4..6)?, num(6..8)?);
    if value.len() == 8 {
        return Some((days * 86_400, true, false));
    }
    if value.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let secs = days * 86_400 + num(9..11)? * 3600 + num(11..13)? * 60 + num(13..15)?;
    match &value[15..] {
        "" => Some((secs, false, false)),
        "Z" | "z" => Some((secs, false, true)),
        _ => None,
    }
}

fn duration(start: &str, end: &str) -> Option<String> {
    let (s, s_date, s_utc) = stamp(start)?;
    let (e, e_date, e_utc) = stamp(end)?;
    if s_date != e_date || s_utc != e_utc || e < s {
        return None;
    }
    Some(if s_date {
        format!("P{}D", (e - s) / 86_400)
    } else {
        format!("PT{}S", e - s)
    })
}

/// Why an answer cannot be written into an event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NoAnswer {
    NotAnEvent,
    NotAnAttendee,
    ClientScheduled,
}

impl NoAnswer {
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            Self::NotAnEvent => "the item is not an event",
            Self::NotAnAttendee => "the account is not an attendee of this event",
            Self::ClientScheduled => {
                "the event asks calendar apps, not the server, to tell the organizer"
            }
        }
    }
}

fn override_of(cal: &Calendar, master: (usize, usize), recurrence_id: &str) -> Vec<Line> {
    const DROPPED: [&str; 8] = [
        "RRULE",
        "RDATE",
        "EXDATE",
        "EXRULE",
        "DTSTART",
        "DTEND",
        "DURATION",
        "RECURRENCE-ID",
    ];
    let start = cal.own_prop(master, "DTSTART");
    let mut when = Prop {
        name: "DTSTART".into(),
        params: start.as_ref().map(|p| p.params.clone()).unwrap_or_default(),
        value: recurrence_id.to_owned(),
    };
    if recurrence_id.ends_with(['Z', 'z']) {
        when.set_param("TZID", None);
    }
    when.set_param("VALUE", (recurrence_id.len() == 8).then_some("DATE"));
    let mut rid = Prop {
        name: "RECURRENCE-ID".into(),
        params: when.params.clone(),
        value: when.value.clone(),
    };
    rid.set_param("VALUE", (recurrence_id.len() == 8).then_some("DATE"));
    let length = cal
        .own_prop(master, "DURATION")
        .map(|p| p.value)
        .or_else(|| {
            let end = cal.own_prop(master, "DTEND")?;
            duration(&start.as_ref()?.value, &end.value)
        });
    let own = cal.own(master);
    let mut out = vec![cal.lines[master.0].clone()];
    out.push(cal.line(rid.render()));
    out.push(cal.line(when.render()));
    if let Some(length) = length {
        out.push(cal.line(format!("DURATION:{length}")));
    }
    for i in master.0 + 1..master.1 {
        if own.contains(&i) {
            let prop = Prop::parse(&cal.lines[i].logical);
            if DROPPED.iter().any(|d| prop.is(d)) {
                continue;
            }
        }
        out.push(cal.lines[i].clone());
    }
    out.push(cal.lines[master.1].clone());
    out
}

/// `text` with the account's own `PARTSTAT` set to `answer` in the series (the master event and
/// every overridden occurrence) or in the occurrence `recurrence_id`, which gets an override
/// copied from the master when it has none. Every other line keeps its bytes. With
/// `notify_organizer`, an event that leaves scheduling to calendar apps
/// (`SCHEDULE-AGENT` other than `SERVER`) is refused, since the server would not tell anyone.
pub(crate) fn answer(
    text: &str,
    addresses: &[String],
    recurrence_id: Option<&str>,
    answer: Partstat,
    notify_organizer: bool,
) -> Result<String, NoAnswer> {
    let mut cal = Calendar::parse(text);
    let events = cal.events();
    let master = events
        .iter()
        .copied()
        .find(|e| cal.own_prop(*e, "RECURRENCE-ID").is_none());
    let mut targets: Vec<(usize, usize)> = match recurrence_id {
        None => events.clone(),
        Some(rid) => events
            .iter()
            .copied()
            .filter(|e| {
                cal.own_prop(*e, "RECURRENCE-ID")
                    .is_some_and(|p| p.value.trim().eq_ignore_ascii_case(rid))
            })
            .collect(),
    };
    if let (Some(rid), true) = (recurrence_id, targets.is_empty()) {
        let master = master.ok_or(NoAnswer::NotAnEvent)?;
        let copy = override_of(&cal, master, rid);
        let at = events.last().map_or(master.1, |e| e.1) + 1;
        let len = copy.len();
        cal.lines.splice(at..at, copy);
        targets = vec![(at, at + len - 1)];
    }
    if targets.is_empty() {
        return Err(NoAnswer::NotAnEvent);
    }
    let mut found = false;
    for event in targets {
        for i in cal.own(event) {
            let mut prop = Prop::parse(&cal.lines[i].logical);
            let client = prop
                .param("SCHEDULE-AGENT")
                .is_some_and(|a| !a.eq_ignore_ascii_case("SERVER"));
            if prop.is("ORGANIZER") && client && notify_organizer {
                return Err(NoAnswer::ClientScheduled);
            }
            if !prop.is("ATTENDEE") || !is_own(&prop.value, addresses) {
                continue;
            }
            if client && notify_organizer {
                return Err(NoAnswer::ClientScheduled);
            }
            found = true;
            prop.set_param("PARTSTAT", Some(answer.as_str()));
            prop.set_param("SCHEDULE-STATUS", None);
            cal.lines[i] = cal.line(prop.render());
        }
    }
    if !found {
        return Err(NoAnswer::NotAnAttendee);
    }
    Ok(cal.render())
}

/// The account's own answer in the series or in the occurrence `recurrence_id`.
#[cfg(test)]
pub(crate) fn partstat(
    text: &str,
    addresses: &[String],
    recurrence_id: Option<&str>,
) -> Option<Partstat> {
    let cal = Calendar::parse(text);
    let event = cal.events().into_iter().find(|e| {
        match (cal.own_prop(*e, "RECURRENCE-ID"), recurrence_id) {
            (None, None) => true,
            (Some(p), Some(rid)) => p.value.trim().eq_ignore_ascii_case(rid),
            _ => false,
        }
    })?;
    cal.own(event)
        .into_iter()
        .map(|i| Prop::parse(&cal.lines[i].logical))
        .find(|p| p.is("ATTENDEE") && is_own(&p.value, addresses))
        .and_then(|p| p.param("PARTSTAT").and_then(Partstat::parse))
}

/// The value of the first `UID` line.
pub(crate) fn uid(text: &str) -> Option<String> {
    Calendar::parse(text)
        .lines
        .iter()
        .map(|l| Prop::parse(&l.logical))
        .find(|p| p.is("UID"))
        .map(|p| p.value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVITE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n\
        BEGIN:VEVENT\r\nUID:weekly@example.org\r\nDTSTAMP:20261101T090000Z\r\n\
        DTSTART;TZID=Europe/Berlin:20261201T100000\r\nDTEND;TZID=Europe/Berlin:20261201T113000\r\n\
        RRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Planning\r\n\
        ORGANIZER;CN=\"Org, Anna\":mailto:organizer@example.org\r\n\
        ATTENDEE;CN=\"Lovelace; Ada\";PARTSTAT=NEEDS-ACTION;RSVP=TRUE;SCHEDULE-STATUS=1.2:\r\n mailto:Ada@Example.com\r\n\
        ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\n\
        BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";

    fn ada() -> Vec<String> {
        vec!["mailto:ada@example.com".into()]
    }

    #[test]
    fn parses_quoted_parameters() {
        let p =
            Prop::parse("ATTENDEE;CN=\"Lovelace; Ada: Countess\";PARTSTAT=X:mailto:a@example.com");
        assert_eq!(p.name, "ATTENDEE");
        assert_eq!(p.param("CN"), Some("Lovelace; Ada: Countess"));
        assert_eq!(p.param("partstat"), Some("X"));
        assert_eq!(p.value, "mailto:a@example.com");
        assert_eq!(Prop::parse("SUMMARY:a:b").value, "a:b");
    }

    #[test]
    fn answers_the_series_and_keeps_every_other_line() {
        let out = answer(INVITE, &ada(), None, Partstat::Accepted, true).unwrap();
        assert_eq!(partstat(&out, &ada(), None), Some(Partstat::Accepted));
        assert!(out.contains("ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.org\r\n"));
        assert!(out.contains("ORGANIZER;CN=\"Org, Anna\":mailto:organizer@example.org\r\n"));
        assert!(!out.contains("SCHEDULE-STATUS"));
        assert!(out.contains("CN=\"Lovelace; Ada\""));
        assert_eq!(
            out.lines().filter(|l| l.contains("BEGIN:VEVENT")).count(),
            1
        );
        assert!(out.lines().all(|l| l.len() <= 76));
    }

    #[test]
    fn answers_one_occurrence_with_an_override() {
        let accepted = answer(INVITE, &ada(), None, Partstat::Accepted, false).unwrap();
        let out = answer(
            &accepted,
            &ada(),
            Some("20261208T100000"),
            Partstat::Declined,
            false,
        )
        .unwrap();
        assert_eq!(partstat(&out, &ada(), None), Some(Partstat::Accepted));
        assert_eq!(
            partstat(&out, &ada(), Some("20261208T100000")),
            Some(Partstat::Declined)
        );
        let tail = &out[out.rfind("BEGIN:VEVENT").unwrap()..];
        assert!(tail.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261208T100000\r\n"));
        assert!(tail.contains("DTSTART;TZID=Europe/Berlin:20261208T100000\r\n"));
        assert!(tail.contains("DURATION:PT5400S\r\n"));
        assert!(!tail.contains("RRULE") && !tail.contains("DTEND"));
        assert!(tail.contains("BEGIN:VALARM"));
        let again = answer(
            &out,
            &ada(),
            Some("20261208T100000"),
            Partstat::Tentative,
            false,
        )
        .unwrap();
        assert_eq!(
            again.matches("BEGIN:VEVENT").count(),
            2,
            "the override is reused"
        );
        assert_eq!(
            partstat(&again, &ada(), Some("20261208T100000")),
            Some(Partstat::Tentative)
        );
    }

    #[test]
    fn refuses_what_it_cannot_answer() {
        let bob = vec!["carol@example.org".to_owned()];
        assert_eq!(
            answer(INVITE, &bob, None, Partstat::Accepted, false),
            Err(NoAnswer::NotAnAttendee)
        );
        let card = "BEGIN:VCARD\r\nFN:Ada\r\nEND:VCARD\r\n";
        assert_eq!(
            answer(card, &ada(), None, Partstat::Accepted, false),
            Err(NoAnswer::NotAnEvent)
        );
        let client = INVITE.replace("ORGANIZER;", "ORGANIZER;SCHEDULE-AGENT=CLIENT;");
        assert_eq!(
            answer(&client, &ada(), None, Partstat::Accepted, true),
            Err(NoAnswer::ClientScheduled)
        );
        assert!(answer(&client, &ada(), None, Partstat::Accepted, false).is_ok());
    }

    #[test]
    fn durations_and_uids() {
        assert_eq!(duration("20261201", "20261203").as_deref(), Some("P2D"));
        assert_eq!(
            duration("20261201T100000Z", "20261201T110000Z").as_deref(),
            Some("PT3600S")
        );
        assert_eq!(duration("20261201", "20261201T110000Z"), None);
        assert_eq!(uid(INVITE).as_deref(), Some("weekly@example.org"));
        let lf = INVITE.replace("\r\n", "\n");
        let out = answer(&lf, &ada(), None, Partstat::Declined, false).unwrap();
        assert!(!out.contains('\r'));
    }
}
