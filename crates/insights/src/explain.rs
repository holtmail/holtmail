use std::fmt::Write as _;

use holtmail_insight::{
    Confidence, Explanation, Insight, InsightInput, InsightKind, PartRef, Provenance,
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
        _ => {}
    }
    line
}
