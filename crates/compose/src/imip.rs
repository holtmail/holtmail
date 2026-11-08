use crate::original::thread;
use crate::{CalendarPart, Draft, Mailbox, Original};

/// An attendee's answer, as an iMIP reply says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Answer {
    /// `ACCEPTED`.
    Accepted,
    /// `TENTATIVE`.
    Tentative,
    /// `DECLINED`.
    Declined,
}

/// What an iMIP message is, for its subject, its readable part and its method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImipKind {
    /// A first `REQUEST`.
    Invitation,
    /// A later `REQUEST` of the same event.
    Update,
    /// A `REPLY`.
    Reply(Answer),
    /// A `CANCEL`.
    Cancel,
}

impl ImipKind {
    /// The iTIP method: `REQUEST`, `REPLY` or `CANCEL`.
    pub fn method(self) -> &'static str {
        match self {
            Self::Invitation | Self::Update => "REQUEST",
            Self::Reply(_) => "REPLY",
            Self::Cancel => "CANCEL",
        }
    }
}

/// An iTIP object and the plain facts the readable part shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// What the message is.
    pub kind: ImipKind,
    /// The iCalendar object, whose `METHOD` is [`ImipKind::method`].
    pub calendar: Vec<u8>,
    /// The event's title.
    pub summary: Option<String>,
    /// When it starts, as text.
    pub when: Option<String>,
    /// Where it takes place.
    pub location: Option<String>,
    /// A note from the sender, such as the reply's `COMMENT`.
    pub comment: Option<String>,
}

fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_control())
        .filter(|part| !part.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

fn title(invitation: &Invitation) -> String {
    invitation
        .summary
        .as_deref()
        .map(one_line)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no title)".to_owned())
}

/// The subject of an iMIP message, in English: `Invitation: <title>`, `Updated invitation:`,
/// `Accepted:`, `Tentatively accepted:`, `Declined:` or `Cancelled:`, with `(no title)` for an
/// event without one. Never contains a line break.
pub fn imip_subject(invitation: &Invitation) -> String {
    let prefix = match invitation.kind {
        ImipKind::Invitation => "Invitation",
        ImipKind::Update => "Updated invitation",
        ImipKind::Reply(Answer::Accepted) => "Accepted",
        ImipKind::Reply(Answer::Tentative) => "Tentatively accepted",
        ImipKind::Reply(Answer::Declined) => "Declined",
        ImipKind::Cancel => "Cancelled",
    };
    format!("{prefix}: {}", title(invitation))
}

/// The readable part of an iMIP message from `from`, in English: one sentence saying what
/// happened, then `When:` and `Where:` when known, then the comment.
pub fn imip_text(from: &Mailbox, invitation: &Invitation) -> String {
    let who = from
        .name
        .as_deref()
        .map(one_line)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| from.address.trim().to_owned());
    let event = title(invitation);
    let mut text = match invitation.kind {
        ImipKind::Invitation => format!("{who} invites you to \"{event}\"."),
        ImipKind::Update => format!("{who} updated \"{event}\"."),
        ImipKind::Reply(Answer::Accepted) => format!("{who} accepted \"{event}\"."),
        ImipKind::Reply(Answer::Tentative) => format!("{who} tentatively accepted \"{event}\"."),
        ImipKind::Reply(Answer::Declined) => format!("{who} declined \"{event}\"."),
        ImipKind::Cancel => format!("{who} cancelled \"{event}\"."),
    };
    text.push('\n');
    let facts: Vec<String> = [("When", &invitation.when), ("Where", &invitation.location)]
        .into_iter()
        .filter_map(|(label, value)| {
            let value = one_line(value.as_deref()?);
            (!value.is_empty()).then(|| format!("{label}: {value}"))
        })
        .collect();
    if !facts.is_empty() {
        text.push('\n');
        text.push_str(&facts.join("\n"));
        text.push('\n');
    }
    if let Some(comment) = invitation
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        text.push('\n');
        text.push_str(comment);
        text.push('\n');
    }
    text
}

/// The draft of an iMIP message (RFC 6047) from `from` to `to` carrying `invitation`.
///
/// Guarantees: the body is the readable text ([`imip_text`]) with the object as the last
/// alternative, `text/calendar; method=<ImipKind::method>`; the subject is [`imip_subject`];
/// with `answering` (the invitation mail a reply answers), `In-Reply-To` is its `Message-ID`
/// and `References` follow the same rules as [`crate::reply`]. Nothing else is set: no HTML, no
/// attachments, no extra headers. [`crate::build`] turns it into bytes.
pub fn imip(
    from: Mailbox,
    to: Vec<Mailbox>,
    invitation: &Invitation,
    answering: Option<&Original>,
) -> Draft {
    Draft {
        subject: imip_subject(invitation),
        text: imip_text(&from, invitation),
        from,
        to,
        in_reply_to: answering.and_then(|o| o.message_id.clone()),
        references: answering.map(thread).unwrap_or_default(),
        calendar: Some(CalendarPart {
            method: invitation.kind.method().to_owned(),
            bytes: invitation.calendar.clone(),
        }),
        ..Draft::default()
    }
}
