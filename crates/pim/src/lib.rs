//! Contacts and calendar events for Holtmail: cutting `.vcf` and `.ics` streams into items,
//! reading each item into a normalized model, comparing two versions field by field, and the keys
//! that match the same event or person across collections.
//!
//! A library, not a contract: there is one right reading of each format. Parsing goes through
//! `calcard`, whose types never leave this crate. Nothing here does I/O.

mod contact;
mod diff;
mod event;
mod format;
mod graph;
mod imip;
mod js;
mod keys;
mod split;
mod write;

use serde::{Deserialize, Serialize};

pub use contact::{Contact, Email, EmbeddedPhoto, NameParts, Phone, Photo, PostalAddress};
pub use diff::{Change, diff};
pub use event::{
    CalendarObject, Event, EventTime, Observance, Participant, ZoneDefinition, iana_zone,
};
pub use format::Format;
pub use imip::{Method, merge, timeline};
pub use keys::{EventKey, normalize_email, normalize_phone};
pub use split::{RawItem, calendar_method, calendar_name, split};
pub use write::{IcsWriter, as_text};

/// What an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A vCard.
    Contact,
    /// An iCalendar `VEVENT` and its changed instances.
    Event,
    /// An iCalendar `VTODO`.
    Task,
    /// Any other component, such as `VJOURNAL`; stored, not modeled.
    Other,
}

impl ItemKind {
    /// The lowercase name used in storage and the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contact => "contact",
            Self::Event => "event",
            Self::Task => "task",
            Self::Other => "other",
        }
    }

    /// Parses [`ItemKind::as_str`] output.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contact" => Self::Contact,
            "event" => Self::Event,
            "task" => Self::Task,
            "other" => Self::Other,
            _ => return None,
        })
    }

    /// The media type of the raw bytes: `text/vcard` for contacts, `text/calendar` otherwise.
    pub fn content_type(&self) -> &'static str {
        match self {
            Self::Contact => "text/vcard",
            _ => "text/calendar",
        }
    }
}

/// The normalized view of one raw version of an item. Derived data: the same bytes always give the
/// same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Normalized {
    /// A vCard.
    Contact(Box<Contact>),
    /// An iCalendar object.
    Calendar(CalendarObject),
    /// Bytes that could not be read as the item's kind. The raw bytes are still archived.
    Unreadable,
}

/// A raw version read as its kind: a vCard for contacts, an iCalendar object for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    /// The normalized view.
    pub normalized: Normalized,
    /// The picture embedded in a vCard, to be stored as a blob of its own; its id is in
    /// [`Contact::photo`].
    pub photo: Option<EmbeddedPhoto>,
}

/// Reads one raw item version of `kind` written as vCard (contacts) or iCalendar (the rest):
/// [`read_as`] with [`Format::of`]. Never fails: bytes that do not parse give
/// [`Normalized::Unreadable`]. Text is read as UTF-8, invalid sequences replaced; vCard 2.1
/// quoted-printable and base64 values are decoded with their `CHARSET`.
pub fn read(kind: ItemKind, raw: &[u8]) -> Read {
    read_as(Format::of(kind), kind, raw)
}

/// Reads one raw item version written in `format`. Never fails: bytes that do not parse as
/// `format`, or a contact format for an event `kind` and the other way round, give
/// [`Normalized::Unreadable`].
///
/// JSContact and JSCalendar are converted to vCard and iCalendar by `calcard` and read as those,
/// so the same card gives the same fields whichever way it arrived. Graph contacts and events are
/// mapped field by field: see the crate's docs for the mapping.
pub fn read_as(format: Format, kind: ItemKind, raw: &[u8]) -> Read {
    let unreadable = Read {
        normalized: Normalized::Unreadable,
        photo: None,
    };
    if format.is_contact() != (kind == ItemKind::Contact) {
        return unreadable;
    }
    let contact = |found: Option<(Contact, Option<EmbeddedPhoto>)>| match found {
        Some((contact, photo)) => Read {
            normalized: Normalized::Contact(Box::new(contact)),
            photo,
        },
        None => Read {
            normalized: Normalized::Unreadable,
            photo: None,
        },
    };
    let calendar = |found: Option<CalendarObject>| Read {
        normalized: found.map_or(Normalized::Unreadable, Normalized::Calendar),
        photo: None,
    };
    match format {
        Format::VCard => contact(contact::parse(raw)),
        Format::JsContact => contact(js::contact(raw)),
        Format::GraphContact => contact(graph::contact(raw).map(|c| (c, None))),
        Format::ICalendar => calendar(event::parse(raw)),
        Format::JsCalendar => calendar(js::calendar(raw)),
        Format::GraphEvent => calendar(graph::event(raw)),
    }
}

/// [`read`] without the picture's bytes.
pub fn normalize(kind: ItemKind, raw: &[u8]) -> Normalized {
    read(kind, raw).normalized
}

/// [`read_as`] without the picture's bytes.
pub fn normalize_as(format: Format, kind: ItemKind, raw: &[u8]) -> Normalized {
    read_as(format, kind, raw).normalized
}

impl Normalized {
    /// The title to list the item by: a contact's [`Contact::display_name`], or the summary of
    /// an event or task.
    pub fn title(&self) -> Option<String> {
        match self {
            Normalized::Contact(contact) => contact.display_name(),
            Normalized::Calendar(calendar) => calendar.main().and_then(|e| e.summary.clone()),
            Normalized::Unreadable => None,
        }
    }

    /// The normalized email addresses of a contact, or of an event's organizer and attendees,
    /// each once, in order of appearance.
    pub fn emails(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut add = |email: Option<&String>| {
            if let Some(email) = email
                && email.contains('@')
                && !out.contains(email)
            {
                out.push(email.clone());
            }
        };
        match self {
            Normalized::Contact(contact) => {
                for email in &contact.emails {
                    add(email.normalized.as_ref());
                }
            }
            Normalized::Calendar(calendar) => {
                for event in &calendar.components {
                    add(event.organizer.as_ref().and_then(|o| o.email.as_ref()));
                    for attendee in &event.attendees {
                        add(attendee.email.as_ref());
                    }
                }
            }
            Normalized::Unreadable => {}
        }
        out
    }

    /// A contact's phone numbers in E.164 form, each once; empty for anything else.
    pub fn phones(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Normalized::Contact(contact) = self {
            for phone in &contact.phones {
                if let Some(e164) = &phone.e164
                    && !out.contains(e164)
                {
                    out.push(e164.clone());
                }
            }
        }
        out
    }

    /// Keys that link a contact to others for the same person: `email:<address>` and
    /// `tel:<E.164>`. A name is never a key. Empty for anything but contacts.
    pub fn contact_keys(&self) -> Vec<String> {
        if !matches!(self, Normalized::Contact(_)) {
            return Vec::new();
        }
        self.emails()
            .into_iter()
            .map(|e| format!("email:{e}"))
            .chain(self.phones().into_iter().map(|p| format!("tel:{p}")))
            .collect()
    }

    /// The keys of an object's events and changed instances, in order.
    pub fn event_keys(&self) -> Vec<EventKey> {
        match self {
            Normalized::Calendar(calendar) => {
                calendar.components.iter().filter_map(Event::key).collect()
            }
            _ => Vec::new(),
        }
    }
}
