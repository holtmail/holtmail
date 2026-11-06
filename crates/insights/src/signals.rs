use holtmail_insight::{InsightInput, Signal, Signals};

const ESP_FIELDS: &[&str] = &["x-sg-eid", "x-mc-user", "x-ses-outgoing", "x-pm-message-id"];
const ESP_PREFIXES: &[&str] = &["x-mailgun-"];

pub(crate) fn header_signals(input: &InsightInput) -> Signals {
    let mut signals = Signals::EMPTY;
    let mut set = |signal, on: bool| {
        if on {
            signals.insert(signal);
        }
    };
    let has = |name: &str| input.header_values(name).next().is_some();

    set(Signal::ListId, has("list-id"));
    let list_post: Vec<bool> = input.header_values("list-post").map(is_no).collect();
    set(Signal::ListPost, list_post.iter().any(|no| !no));
    set(Signal::ListPostNo, list_post.iter().any(|no| *no));
    set(Signal::Unsubscribe, has("list-unsubscribe"));
    set(Signal::UnsubscribePost, has("list-unsubscribe-post"));
    set(
        Signal::PrecedenceBulk,
        input.header_values("precedence").any(|value| {
            let value = strip_comments(value);
            ["bulk", "list", "junk"]
                .iter()
                .any(|k| value.eq_ignore_ascii_case(k))
        }),
    );
    for value in input.header_values("auto-submitted") {
        let value = strip_comments(value);
        let keyword = value.split(';').next().unwrap_or("").trim();
        if keyword.eq_ignore_ascii_case("auto-replied") {
            set(Signal::AutoReplied, true);
        } else if !keyword.is_empty() && !keyword.eq_ignore_ascii_case("no") {
            set(Signal::AutoSubmitted, true);
        }
    }
    set(Signal::FeedbackId, has("feedback-id"));
    set(
        Signal::Esp,
        input.headers.iter().any(|field| {
            let name = field.name.to_ascii_lowercase();
            ESP_FIELDS.contains(&name.as_str()) || ESP_PREFIXES.iter().any(|p| name.starts_with(p))
        }),
    );
    set(
        Signal::Noreply,
        input
            .from
            .first()
            .and_then(|address| address.email.rsplit_once('@'))
            .is_some_and(|(local, _)| holtmail_people::is_automated_local(local)),
    );
    set(
        Signal::ForwardMarker,
        input.forward_at.is_some()
            || input.html.as_ref().is_some_and(|html| {
                html.segments.iter().any(|s| s.below_forward)
                    || html.anchors.iter().any(|a| a.below_forward)
            }),
    );
    signals
}

fn is_no(value: &str) -> bool {
    strip_comments(value).eq_ignore_ascii_case("no")
}

fn strip_comments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;
    let mut escaped = false;
    for c in value.chars() {
        if depth > 0 {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        } else if c == '(' {
            depth = 1;
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use holtmail_core::Address;
    use holtmail_insight::{HeaderField, HtmlView, Segment};

    use super::*;

    fn input(headers: &[(&str, &str)]) -> InsightInput {
        InsightInput {
            headers: headers
                .iter()
                .map(|(n, v)| HeaderField::new(*n, *v))
                .collect(),
            ..InsightInput::default()
        }
    }

    fn names(signals: Signals) -> Vec<&'static str> {
        signals.iter().map(Signal::name).collect()
    }

    #[test]
    fn plain_mail_has_no_signals() {
        let mut plain = input(&[("From", "ada@example.com"), ("Subject", "Lunch")]);
        plain.from = vec![Address {
            name: None,
            email: "ada@example.com".into(),
        }];
        assert!(header_signals(&plain).is_empty());
    }

    #[test]
    fn list_and_bulk_headers() {
        let signals = header_signals(&input(&[
            ("List-Id", "News <news.example.com>"),
            ("List-Post", "NO (posting not allowed)"),
            ("List-Unsubscribe", "<https://news.example.com/u>"),
            ("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
            ("Precedence", "Bulk"),
            ("Feedback-ID", "1:2:example"),
            ("X-Mailgun-Sid", "abc"),
        ]));
        assert_eq!(
            names(signals),
            [
                "list_id",
                "list_post_no",
                "unsubscribe",
                "unsubscribe_post",
                "precedence_bulk",
                "feedback_id",
                "esp"
            ]
        );
    }

    #[test]
    fn list_post_with_an_address() {
        let signals = header_signals(&input(&[
            ("List-Id", "<dev.example.org>"),
            ("List-Post", "<mailto:dev@example.org>"),
        ]));
        assert_eq!(names(signals), ["list_id", "list_post"]);
    }

    #[test]
    fn auto_submitted_values() {
        assert_eq!(
            names(header_signals(&input(&[(
                "Auto-Submitted",
                "auto-generated"
            )]))),
            ["auto_submitted"]
        );
        assert_eq!(
            names(header_signals(&input(&[(
                "Auto-Submitted",
                "Auto-Replied (vacation)"
            )]))),
            ["auto_replied"]
        );
        assert!(header_signals(&input(&[("Auto-Submitted", "no")])).is_empty());
        assert!(header_signals(&input(&[("Precedence", "first-class")])).is_empty());
    }

    #[test]
    fn noreply_and_forward_marker() {
        let mut message = input(&[]);
        message.from = vec![Address {
            name: None,
            email: "No-Reply@shop.example.com".into(),
        }];
        assert_eq!(names(header_signals(&message)), ["noreply"]);

        let mut forwarded = input(&[]);
        forwarded.html = Some(HtmlView::new().with_segments(vec![Segment {
            text: "Hello".into(),
            below_forward: true,
            ..Segment::default()
        }]));
        assert_eq!(names(header_signals(&forwarded)), ["forward_marker"]);
        forwarded.html = None;
        forwarded.forward_at = Some(10);
        assert_eq!(names(header_signals(&forwarded)), ["forward_marker"]);
    }
}
