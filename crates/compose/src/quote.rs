use holtmail_core::Timestamp;

use crate::Mailbox;

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn date(date: Timestamp) -> Option<String> {
    chrono::DateTime::from_timestamp(date.as_secs(), 0)
        .map(|date| date.format("%a, %-d %b %Y at %H:%M UTC").to_string())
}

pub(crate) fn mailbox(mailbox: &Mailbox) -> String {
    match mailbox.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => format!("{name} <{}>", mailbox.address),
        _ => mailbox.address.clone(),
    }
}

pub(crate) fn mailboxes(list: &[Mailbox]) -> String {
    list.iter().map(mailbox).collect::<Vec<_>>().join(", ")
}

pub(crate) fn attribution(from: &[Mailbox], sent: Option<Timestamp>) -> String {
    let who = if from.is_empty() {
        "someone".to_owned()
    } else {
        mailboxes(from)
    };
    match sent.and_then(date) {
        Some(when) => format!("On {when}, {who} wrote:"),
        None => format!("{who} wrote:"),
    }
}

pub(crate) fn quote_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let text = text.trim_end_matches('\n');
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for line in text.split('\n') {
        if line.is_empty() {
            out.push('>');
        } else if line.starts_with('>') {
            out.push('>');
            out.push_str(line);
        } else {
            out.push_str("> ");
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_every_line() {
        assert_eq!(
            quote_text("Hi\r\n\r\n> earlier\nbye\n"),
            "> Hi\n>\n>> earlier\n> bye\n"
        );
    }

    #[test]
    fn formats_attribution() {
        let from = [Mailbox::named("Ada Lovelace", "ada@example.com")];
        assert_eq!(
            attribution(&from, Some(Timestamp::from_secs(1_793_000_000))),
            "On Mon, 26 Oct 2026 at 07:33 UTC, Ada Lovelace <ada@example.com> wrote:"
        );
        assert_eq!(attribution(&[], None), "someone wrote:");
    }

    #[test]
    fn escapes_html() {
        assert_eq!(
            escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
