use std::fmt::Write as _;

use holtmail_insight::{
    Confidence, Explanation, Insight, InsightInput, InsightKind, LocalOrZoned, PartRef, Provenance,
    Reservation,
};

use crate::{Extracted, Runner, scope};

pub(crate) fn report(
    runner: &Runner,
    input: &InsightInput,
    truncated: bool,
    extracted: &Extracted,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "extract_fp {}", runner.extract_fp());
    let html = input.html.as_ref();
    let _ = writeln!(
        out,
        "input text_chars={} html={} segments={} anchors={} ld_json={} attachments={} forward_at={} truncated={truncated}",
        input.text.chars().count(),
        html.is_some(),
        html.map_or(0, |h| h.segments.len()),
        html.map_or(0, |h| h.anchors.len()),
        html.map_or(0, |h| h.ld_json.len()),
        input.attachments.len(),
        input
            .forward_at
            .map_or_else(|| "-".to_owned(), |at| at.to_string()),
    );
    let signals: Vec<&str> = extracted.signals.iter().map(|s| s.name()).collect();
    let _ = writeln!(out, "signals {}", list(&signals));

    for (extractor, version) in &runner.extractors {
        let _ = writeln!(out, "[{} {version}]", extractor.id());
        match scope::scoped(extractor.as_ref(), version, || extractor.explain(input)) {
            Some(steps) => {
                for step in &steps {
                    let _ = writeln!(out, "  {}", step_line(step));
                }
            }
            None => {
                let _ = writeln!(out, "  explain panicked");
            }
        }
    }

    let kinds: Vec<&str> = extracted.kinds.iter().map(|k| k.name()).collect();
    let _ = writeln!(
        out,
        "result outcome={:?} truncated={} kinds={}",
        extracted.outcome,
        extracted.truncated,
        list(&kinds)
    );
    for (index, item) in extracted.items.items.iter().enumerate() {
        let _ = writeln!(out, "  #{index} {}", item_line(item));
    }
    out
}

fn list(names: &[&str]) -> String {
    if names.is_empty() {
        "-".to_owned()
    } else {
        names.join(",")
    }
}

fn part(part: &PartRef) -> String {
    match part {
        PartRef::Header(name) => format!("header:{name}"),
        PartRef::Subject => "subject".to_owned(),
        PartRef::Text => "text".to_owned(),
        PartRef::Html => "html".to_owned(),
        PartRef::Attachment(ordinal) => format!("attachment:{ordinal}"),
    }
}

fn step_line(step: &Explanation) -> String {
    let mut line = step.rule.to_owned();
    if let Some(p) = &step.part {
        let _ = write!(line, " {}", part(p));
    }
    if let Some(at) = step.at {
        let _ = write!(line, " at={at}");
    }
    if let Some(score) = step.score {
        let _ = write!(line, " score={score}");
    }
    if let Some(count) = step.count {
        let _ = write!(line, " count={count}");
    }
    if let Some(skeleton) = &step.skeleton {
        let _ = write!(line, " shape={skeleton}");
    }
    if !step.keywords.is_empty() {
        let _ = write!(line, " keywords={}", step.keywords.join("|"));
    }
    line
}

fn item_line(item: &Insight) -> String {
    let confidence = match item.confidence {
        Confidence::Possible => "possible",
        Confidence::Likely => "likely",
        Confidence::Certain => "certain",
    };
    let provenance = match item.provenance {
        Provenance::Header => "header",
        Provenance::JsonLd => "json-ld",
        Provenance::Microdata => "microdata",
        Provenance::PkPass => "pkpass",
        Provenance::OriginBound => "origin-bound",
        Provenance::Heuristic => "heuristic",
    };
    let mut line = format!(
        "{} {confidence} {provenance} score={} origin={}",
        item.kind.tag().name(),
        item.score,
        part(&item.origin)
    );
    if item.forwarded {
        line.push_str(" forwarded");
    }
    match &item.kind {
        InsightKind::Code {
            value,
            origin_bound,
        } => {
            let _ = write!(
                line,
                " shape={} origin_bound={}",
                holtmail_insight::Skeleton::of(value),
                origin_bound.is_some()
            );
        }
        InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            list_id,
        } => {
            let _ = write!(
                line,
                " https={} http={} mailto={} one_click={one_click} list_id={}",
                https.is_some(),
                http.is_some(),
                mailto.is_some(),
                list_id.is_some()
            );
        }
        InsightKind::Action {
            action,
            via,
            tracked,
            text_host,
            label,
            ..
        } => {
            let action = serde_json::to_value(action)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let _ = write!(
                line,
                " action={action} via={} tracked={tracked} text_host={} label={}",
                via.is_some(),
                text_host.is_some(),
                label.is_some()
            );
        }
        InsightKind::Parcel {
            carrier,
            number,
            check_digit,
            track_url,
        } => {
            let _ = write!(
                line,
                " carrier={} shape={} check_digit={check_digit} track_url={}",
                carrier.as_str(),
                holtmail_insight::Skeleton::of(number),
                track_url.is_some()
            );
        }
        InsightKind::Reservation(reservation) => {
            let _ = write!(line, " {}", reservation_shape(reservation));
        }
        InsightKind::Order {
            number,
            merchant,
            total,
            status,
            url,
        } => {
            let _ = write!(
                line,
                " number={} merchant={} total={} status={} url={}",
                shape(number.as_deref()),
                merchant.is_some(),
                total.is_some(),
                status.is_some(),
                url.is_some()
            );
        }
        InsightKind::Invoice {
            number,
            payee,
            total,
            due,
            url,
        } => {
            let _ = write!(
                line,
                " number={} payee={} total={} due={} url={}",
                shape(number.as_deref()),
                payee.is_some(),
                total.is_some(),
                due.is_some(),
                url.is_some()
            );
        }
        InsightKind::Pass {
            style,
            relevant,
            boarding,
            attachment,
            signature_checked,
            ..
        } => {
            let style = serde_json::to_value(style)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let _ = write!(
                line,
                " style={style} attachment={attachment} relevant={} boarding_legs={} signature_checked={signature_checked}",
                relevant.as_ref().map_or("-", time_shape),
                boarding.as_ref().map_or(0, |b| b.legs.len())
            );
        }
    }
    line
}

fn shape(value: Option<&str>) -> String {
    value.map_or_else(
        || "-".to_owned(),
        |value| holtmail_insight::Skeleton::of(value).to_string(),
    )
}

fn time_shape(at: &LocalOrZoned) -> &'static str {
    match at {
        LocalOrZoned::Local(_) => "local",
        LocalOrZoned::Zoned(..) => "zoned",
    }
}

fn reservation_shape(reservation: &Reservation) -> String {
    let (kind, number, start, people, seats, url) = match reservation {
        Reservation::Flight(r) => (
            "flight",
            r.reservation_number.as_deref(),
            &r.departure,
            r.passengers.len(),
            r.seats.len(),
            r.url.is_some(),
        ),
        Reservation::Lodging(r) => (
            "lodging",
            Some(r.reservation_number.as_str()),
            &r.checkin,
            r.guests.len(),
            0,
            r.url.is_some(),
        ),
        Reservation::Train(r) => (
            "train",
            r.reservation_number.as_deref(),
            &r.departure,
            r.passengers.len(),
            r.seats.len(),
            r.url.is_some(),
        ),
        Reservation::Bus(r) => (
            "bus",
            r.reservation_number.as_deref(),
            &r.departure,
            r.passengers.len(),
            r.seats.len(),
            r.url.is_some(),
        ),
        Reservation::Event(r) => (
            "event",
            r.reservation_number.as_deref(),
            &r.start,
            0,
            r.tickets.len(),
            r.url.is_some(),
        ),
        Reservation::Food(r) => (
            "food",
            r.reservation_number.as_deref(),
            &r.start,
            0,
            0,
            r.url.is_some(),
        ),
        Reservation::RentalCar(r) => (
            "rental_car",
            Some(r.reservation_number.as_str()),
            &r.pickup,
            0,
            0,
            r.url.is_some(),
        ),
    };
    format!(
        "reservation={kind} number={} start={} people={people} seats={seats} url={url}",
        shape(number),
        time_shape(start)
    )
}
