use crate::ItemKind;

/// How an item's raw bytes are written. Each source stores what its server sent, so the format
/// travels with the bytes as their media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// A vCard (`text/vcard`).
    VCard,
    /// An iCalendar object (`text/calendar`).
    ICalendar,
    /// A Microsoft Graph `contact` as JSON.
    GraphContact,
    /// A Microsoft Graph `event` as JSON, a series master with its exceptions expanded.
    GraphEvent,
    /// A JSContact card (RFC 9553), such as a JMAP `ContactCard`.
    JsContact,
    /// A JSCalendar event or task (RFC 8984), such as a JMAP `CalendarEvent`.
    JsCalendar,
}

impl Format {
    /// Every format.
    pub const ALL: [Format; 6] = [
        Self::VCard,
        Self::ICalendar,
        Self::GraphContact,
        Self::GraphEvent,
        Self::JsContact,
        Self::JsCalendar,
    ];

    /// The media type the bytes are stored and served with.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::VCard => "text/vcard",
            Self::ICalendar => "text/calendar",
            Self::GraphContact => "application/vnd.microsoft.graph.contact+json",
            Self::GraphEvent => "application/vnd.microsoft.graph.event+json",
            Self::JsContact => "application/jscontact+json",
            Self::JsCalendar => "application/jscalendar+json",
        }
    }

    /// The format of a media type, parameters and case ignored; `None` for any other type.
    pub fn from_content_type(content_type: &str) -> Option<Self> {
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        Self::ALL.into_iter().find(|f| f.content_type() == essence)
    }

    /// The text format of `kind`: vCard for contacts, iCalendar for the rest.
    pub fn of(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Contact => Self::VCard,
            _ => Self::ICalendar,
        }
    }

    /// The format of bytes stored with `content_type` for an item of `kind`: the type's format,
    /// else the text format of `kind`, so versions stored before formats were recorded keep
    /// reading as they did.
    pub fn stored(content_type: &str, kind: ItemKind) -> Self {
        Self::from_content_type(content_type).unwrap_or(Self::of(kind))
    }

    /// Whether the format holds contacts rather than calendar objects.
    pub fn is_contact(self) -> bool {
        matches!(self, Self::VCard | Self::GraphContact | Self::JsContact)
    }

    /// What one item in this format is: a contact, or for calendar formats the kind of its first
    /// component (`VTODO` and JSCalendar `Task` are tasks). Bytes that cannot be told count as an
    /// event.
    pub fn kind(self, raw: &[u8]) -> ItemKind {
        match self {
            Self::VCard | Self::GraphContact | Self::JsContact => ItemKind::Contact,
            Self::GraphEvent => ItemKind::Event,
            Self::ICalendar => crate::split(raw)
                .first()
                .map_or(ItemKind::Event, |item| item.kind),
            Self::JsCalendar => {
                let kind = serde_json::from_slice::<serde_json::Value>(raw)
                    .ok()
                    .and_then(|v| v.get("@type").and_then(|t| t.as_str()).map(str::to_owned));
                match kind.as_deref() {
                    Some(t) if t.eq_ignore_ascii_case("task") => ItemKind::Task,
                    _ => ItemKind::Event,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_round_trip() {
        for format in Format::ALL {
            assert_eq!(
                Format::from_content_type(format.content_type()),
                Some(format)
            );
        }
        assert_eq!(
            Format::from_content_type("Text/Calendar; charset=utf-8"),
            Some(Format::ICalendar)
        );
        assert_eq!(Format::from_content_type("application/json"), None);
    }

    #[test]
    fn kinds_of_items() {
        assert_eq!(
            Format::JsCalendar.kind(br#"{"@type":"Task","title":"x"}"#),
            ItemKind::Task
        );
        assert_eq!(
            Format::JsCalendar.kind(br#"{"@type":"Event"}"#),
            ItemKind::Event
        );
        assert_eq!(Format::GraphContact.kind(b"{}"), ItemKind::Contact);
        assert_eq!(
            Format::ICalendar
                .kind(b"BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:t\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"),
            ItemKind::Task
        );
    }
}
