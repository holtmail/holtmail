//! Edits to contacts and events, for held changes from devices and the web app: validating
//! what a client sends, classifying it against its base (`noop`, `personal`, `reply`, `edit`,
//! `create`, `delete`), re-applying it on a newer upstream version, the three-way field diff of
//! the conflict view, the field patch for write-through, and the event and contact forms of the
//! web app; and the iTIP side of invitations: iMIP `REPLY`, `REQUEST` and `CANCEL` objects and
//! the choice of who sends them.
//!
//! A pure library over `holtmail-pim`: no I/O, no store, the same inputs always give the same
//! output. It reads iCalendar and vCard as content lines of its own, so whatever it does not
//! model (alarms, client properties, changed instances) survives a rebase untouched.

mod answer;
mod form;
mod imip;
mod instance;
mod lines;
mod model;
mod patch;
mod schedule;
mod three_way;
mod validate;

pub use answer::{Respond, answered, apply_reply, holds_series, with_instances};
pub use form::{
    Alarm, ContactForm, EventForm, FormError, Labeled, Person, build_contact, build_event,
    contact_form, event_form,
};
pub use imip::{
    Audience, CancelScope, Imip, ImipError, Organized, cancel, organize, organizer, reply,
    reply_agent, request,
};
pub use patch::{Class, Classified, Patch, Rebased, classify, item_patch, rebase, serve};
pub use schedule::{
    CopyUpdate, Host, OrganizerChange, OrganizerRoute, ReplyRoute, ScheduleAgent, organizer_route,
    reply_route,
};
pub use three_way::{FieldDiff, field_diff};
pub use validate::{Invalid, MAX_ICALENDAR, MAX_VCARD, Valid, max_size, validate};
