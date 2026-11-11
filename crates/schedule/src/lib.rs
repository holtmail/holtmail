//! Contract for calendar scheduling (RFC 6638 `calendar-auto-schedule` and iMIP, RFC 6047).
//!
//! A [`Scheduler`] prepares what a calendar client writes (client scheduling parameters
//! stripped, an attendee's write reduced to what RFC 6638 section 3.2.2.1 allows), delivers the
//! iTIP messages a change calls for (organizer `REQUEST` and `CANCEL`, attendee `REPLY`), held
//! until the caller confirms the change was recorded, decides what an incoming iTIP message in mail does, and answers whether a message
//! was just sent, so web, CalDAV and mail never send the same thing twice.
//!
//! The scheduler never writes to the store: it returns the bytes to record and the decisions,
//! and the caller records them in its own transaction. It sends only through
//! `holtmail_submission::Submission`, so every message is held to the sending locks and limits.

#[cfg(feature = "testing")]
pub mod testing;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_core::{ArchiveId, OutboxId, Timestamp};
use holtmail_route::Route;
use holtmail_submission::Actor;

/// How long a delivered iTIP message counts as just sent: for [`Scheduler::just_sent`] and for
/// dropping a repeated outgoing message. Ten minutes.
pub const RECENT: Duration = Duration::from_secs(600);

/// The time source a scheduler reads, so tests can move time.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// The clock of the system.
pub fn system_clock() -> Clock {
    Arc::new(Timestamp::now)
}

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// A calendar client on a device, over CalDAV. `SCHEDULE-AGENT=CLIENT` on an attendee
    /// means that client tells the attendee itself.
    Device,
    /// The web app, which is Holtmail's own client: `SCHEDULE-AGENT=CLIENT` there means
    /// Holtmail mails the attendee, because a scheduling upstream server will not.
    Web,
    /// The scheduler itself, such as an incoming `REPLY` merged into the organizer's copy.
    /// Never sends anything.
    Itip,
}

/// The calendar holding the event, as far as scheduling goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Host {
    /// A CalDAV calendar upstream; `auto_schedule` when that server advertises
    /// `calendar-auto-schedule` for it.
    CalDav {
        /// The upstream server sends iTIP messages when an event is written.
        auto_schedule: bool,
    },
    /// A Microsoft Graph calendar, which always schedules.
    Graph,
    /// A JMAP calendar; `scheduling` when the server is known to send scheduling messages.
    Jmap {
        /// Writing makes the server send its scheduling messages.
        scheduling: bool,
    },
    /// The "From mail" calendar, or any calendar without an upstream server.
    Mail,
}

/// What the account may write to the upstream calendar now: every lock open and the
/// collection writing through. All false when the change stays held in Holtmail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct HostCaps {
    /// Creating events.
    pub create: bool,
    /// Changing events.
    pub update: bool,
    /// Deleting events.
    pub delete: bool,
    /// Answering invitations through the server.
    pub respond: bool,
}

/// The user's part in an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The `ORGANIZER` is one of the user's addresses.
    Organizer,
    /// One of the user's addresses is an `ATTENDEE` and the organizer is someone else.
    Attendee,
    /// Not a scheduling object for the user: no organizer, or none of the user's addresses.
    None,
}

/// What a calendar client wants to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPut {
    /// The copy as it is served now; `None` for a new event.
    pub previous: Option<Bytes>,
    /// The client's iCalendar object.
    pub body: Bytes,
    /// The user's calendar addresses (`calendar-user-address-set`), in any spelling.
    pub own: Vec<String>,
    /// The client wrote with a matching `If-Schedule-Tag-Match`: its copy may predate answers
    /// merged since, so an organizer's write takes every other attendee's `PARTSTAT` from
    /// `previous` (RFC 6638 section 3.2.10).
    pub keep_answers: bool,
}

/// A client's object made fit to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The bytes to record.
    pub body: Bytes,
    /// The user's part in the event.
    pub role: Role,
    /// Client-sent `SCHEDULE-STATUS` or `SCHEDULE-FORCE-SEND` parameters were removed.
    pub stripped: bool,
    /// An attendee's write held changes RFC 6638 section 3.2.2.1 does not allow; `body` keeps
    /// only the allowed ones.
    pub reduced: bool,
}

/// The address and route an attendee's answer leaves with, when a front end already chose
/// them: an invitation that came through an alias is answered as the alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyAs {
    /// The attendee address the answer speaks for, as it is on the invitation.
    pub attendee: String,
    /// The route to send on; its `from` is the mail's `From`.
    pub route: Route,
    /// The address the answer goes to instead of the `ORGANIZER`: the reverse alias or the
    /// alias service's `Reply-To` for an invitation that came through an alias service.
    pub to: Option<String>,
}

/// A change of an event, as it is about to be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Who made it; messages are submitted as this actor.
    pub actor: Actor,
    /// The archive of the calendar.
    pub archive: ArchiveId,
    /// Where it came from.
    pub origin: Origin,
    /// The calendar holding the event.
    pub host: Host,
    /// What the account may write there now.
    pub caps: HostCaps,
    /// The user's calendar addresses, in any spelling.
    pub own: Vec<String>,
    /// The copy before; `None` for a create.
    pub previous: Option<Bytes>,
    /// The copy after, prepared; `None` for a delete.
    pub next: Option<Bytes>,
    /// For an attendee's answer: the address and route chosen by the front end.
    pub reply_as: Option<ReplyAs>,
    /// `Message-ID`s (without angle brackets) of the invitation mail an answer refers to,
    /// oldest first; the last one becomes `In-Reply-To`.
    pub references: Vec<String>,
    /// A note for the answer's `COMMENT`.
    pub comment: Option<String>,
}

/// An RFC 6638 `SCHEDULE-STATUS` code (section 3.2.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScheduleStatus {
    /// `1.0`: pending; the change is held, or the upstream server sends once it is written.
    Pending,
    /// `1.1`: sent; queued in the outbox, or written through to a scheduling server.
    Sent,
    /// `1.2`: delivered; only when an upstream scheduler reports it.
    Delivered,
    /// `3.7`: no usable identity or route for the sender.
    NoRoute,
    /// `5.1`: the outbox refused the message when it was submitted. A message that fails or
    /// bounces later is reported by the outbox, not written back here.
    Failed,
}

impl ScheduleStatus {
    /// The code as iCalendar writes it, such as `1.1`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "1.0",
            Self::Sent => "1.1",
            Self::Delivered => "1.2",
            Self::NoRoute => "3.7",
            Self::Failed => "5.1",
        }
    }

    /// Reads a code; only the five above are known.
    pub fn parse(code: &str) -> Option<Self> {
        match code.trim() {
            "1.0" => Some(Self::Pending),
            "1.1" => Some(Self::Sent),
            "1.2" => Some(Self::Delivered),
            "3.7" => Some(Self::NoRoute),
            "5.1" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// How a change's messages leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Way {
    /// No message is needed.
    Nothing,
    /// The upstream server sends when the change is written. For an attendee's answer the
    /// caller writes it with `respond` and `notify` on.
    Server,
    /// The upstream server will send once the held change is written.
    Deferred,
    /// Holtmail mails them through the outbox.
    Mail,
    /// Nobody is told (`SCHEDULE-AGENT=NONE` on the organizer).
    Quiet,
}

/// The status of one recipient of a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientStatus {
    /// The address, normalized (lowercase, no `mailto:`).
    pub address: String,
    /// What became of the message for it.
    pub status: ScheduleStatus,
}

/// What [`Scheduler::deliver`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The bytes to record: the change's copy with `SEQUENCE` raised and answers reset where
    /// RFC 5546 asks, and `SCHEDULE-STATUS` written. `None` for a delete.
    pub body: Option<Bytes>,
    /// `body` differs from the change's `next`: a client must refetch, so no ETag is returned.
    pub rewritten: bool,
    /// The user's part in the event.
    pub role: Role,
    /// How the messages leave.
    pub way: Way,
    /// Each recipient the change concerns, in the order of the event.
    pub statuses: Vec<RecipientStatus>,
    /// The outbox messages held for this change: to confirm once it is recorded, to withdraw
    /// if recording fails, and to record with it.
    pub queued: Vec<OutboxId>,
    /// Messages not queued because the same one was delivered within [`RECENT`].
    pub suppressed: usize,
}

/// An iTIP method the scheduler handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItipMethod {
    /// `REQUEST`.
    Request,
    /// `REPLY`.
    Reply,
    /// `CANCEL`.
    Cancel,
}

impl ItipMethod {
    /// The method as iCalendar writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "REQUEST",
            Self::Reply => "REPLY",
            Self::Cancel => "CANCEL",
        }
    }

    /// Reads a method name, ignoring case; other methods are `None`.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "REQUEST" => Some(Self::Request),
            "REPLY" => Some(Self::Reply),
            "CANCEL" => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// What makes two iMIP messages the same for [`Scheduler::just_sent`]: the same answer or the
/// same version of the same event or instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItipKey {
    /// The event's `UID`.
    pub uid: String,
    /// The message's `SEQUENCE`.
    pub sequence: u32,
    /// The method.
    pub method: ItipMethod,
    /// The `RECURRENCE-ID` value of the first event, trimmed; `None` for the series.
    pub recurrence_id: Option<String>,
    /// For a `REPLY`: the answering attendee's `PARTSTAT`, uppercase; `None` otherwise.
    pub partstat: Option<String>,
}

/// Where an incoming message was found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Arrival {
    /// A live arrival: a folder that finished a sync before, or an SMTP delivery. Never a
    /// backfill or an import.
    pub live: bool,
    /// The message is in a Sent folder (or has Gmail's `Sent` label).
    pub sent_folder: bool,
    /// The message is the archived copy of something Holtmail sent.
    pub outbox_copy: bool,
}

/// The authentication facts of an incoming message, from the topmost
/// `Authentication-Results` whose authserv-id the operator trusts for the account the message
/// came through. Empty when no trusted server stamped the message: a field anyone could have
/// written proves nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthFacts {
    /// Domains whose DKIM signature passed.
    pub dkim_pass: Vec<String>,
    /// The `header.from` domains DMARC passed for, lowercase.
    pub dmarc_pass: Vec<String>,
}

/// An incoming message carrying iTIP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingItip {
    /// The `text/calendar` part.
    pub calendar: Bytes,
    /// The addresses of the header `From`.
    pub from: Vec<String>,
    /// The real sender behind an alias service that signed the mail.
    pub signed_sender: Option<String>,
    /// The addresses the mail was sent to (`To`, `Cc`, the delivery address).
    pub recipients: Vec<String>,
    /// The user's own addresses, in any spelling.
    pub own: Vec<String>,
    /// Where it was found.
    pub arrival: Arrival,
    /// Its authentication facts.
    pub auth: AuthFacts,
    /// A served upstream calendar of the archive already holds the event's `UID`.
    pub upstream_holds_uid: bool,
    /// For a `REPLY`: the user's organizer copy of the event with the same `UID`, as served.
    pub organizer_copy: Option<Bytes>,
    /// "Apply anyway": the organizer accepts a `REPLY` that failed the authentication check.
    pub force: bool,
}

/// Why an incoming message is not scheduling input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ignored {
    /// Not a live arrival.
    NotLive,
    /// Found in a Sent folder.
    SentFolder,
    /// The archived copy of something Holtmail sent.
    OutboxCopy,
    /// `From` one of the user's own addresses.
    OwnSender,
    /// Not addressed to any of the user's addresses.
    NotAddressed,
    /// Not a readable `REQUEST`, `REPLY` or `CANCEL` of an event.
    NotItip,
}

/// What an incoming `REQUEST` or `CANCEL` does to the attendee copy in "From mail".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FromMail {
    /// The "From mail" copy follows the message.
    Update,
    /// A served upstream calendar already holds the `UID`; that copy is the user's.
    UpstreamHolds,
    /// The organizer is one of the user's own addresses: the organizer's copy already exists.
    OwnOrganizer,
}

/// Why an incoming `REPLY` was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unapplied {
    /// It does not name exactly one attendee who is the mail's `From` (or the signed sender
    /// behind an alias).
    NotFromAttendee,
    /// Neither DKIM nor DMARC passed for the sender's domain; "Apply anyway" can override.
    Unauthenticated,
    /// The user organizes no event with this `UID`.
    NoCopy,
    /// The copy already holds the answer, or the reply is older than the copy.
    Unchanged,
}

/// What an incoming `REPLY` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyOutcome {
    /// Record these bytes as the organizer's copy, as a change with origin `itip`. The
    /// `Schedule-Tag` stays, and nothing is sent.
    Merged {
        /// The organizer's copy with the attendee's `PARTSTAT`.
        body: Bytes,
    },
    /// Shown in the schedule inbox with this note, nothing changed.
    Unapplied(Unapplied),
}

/// What an incoming iTIP message does. Every outcome but [`Incoming::Ignored`] makes the
/// message a schedule-inbox resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// Not scheduling input.
    Ignored(Ignored),
    /// A `REQUEST` or `CANCEL` for the user as an attendee.
    Invitation {
        /// `REQUEST` or `CANCEL`.
        method: ItipMethod,
        /// The event's `UID`.
        uid: String,
        /// What happens to the "From mail" copy.
        from_mail: FromMail,
    },
    /// An attendee's answer to an event the user organizes.
    Reply {
        /// The event's `UID`.
        uid: String,
        /// What it does.
        outcome: ReplyOutcome,
    },
}

/// Why scheduling failed. Messages never carry addresses or content.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    /// The iCalendar object cannot be read or prepared.
    #[error("invalid: {0}")]
    Invalid(String),
    /// The outbox could not be reached; nothing was queued, a later try may pass.
    #[error("scheduling is unavailable: {0}")]
    Unavailable(String),
}

/// Schedules events. Safe to share between tasks.
#[async_trait]
pub trait Scheduler: Send + Sync {
    /// Makes a client's object fit to record. Client-sent `SCHEDULE-STATUS` and
    /// `SCHEDULE-FORCE-SEND` are removed everywhere; `SCHEDULE-AGENT` stays. An attendee's write
    /// over an existing copy keeps only what RFC 6638 section 3.2.2.1 allows: the user's own
    /// `PARTSTAT` on the series and on instances (an instance added or an `EXDATE` added by the
    /// attendee is a decline of that instance), alarms, `TRANSP`, `PERCENT-COMPLETE`,
    /// `COMPLETED` and client properties; everything else stays as the copy has it and
    /// `reduced` is set. An organizer's write and a new object are kept otherwise unchanged.
    /// The same input always gives the same output.
    async fn prepare(&self, put: &ClientPut) -> Result<Prepared, ScheduleError>;

    /// Delivers the iTIP messages `change` calls for, before the caller records
    /// [`Delivery::body`]. Messages are held: none leaves before [`Scheduler::confirm`], and
    /// [`Scheduler::withdraw`] takes them back if recording fails. The caller records
    /// [`Delivery::queued`] with the change, so messages of a change recorded before a stop are
    /// released at the next start and the others stay held.
    ///
    /// - Origin [`Origin::Itip`], and a change where the user is neither organizer nor
    ///   attendee, send nothing and return `next` unchanged.
    /// - Organizer: a create sends `REQUEST`, a delete `CANCEL`, an update what RFC 5546 asks
    ///   (removed attendees `CANCEL`, other changes `REQUEST`); answers, alarms and client
    ///   properties alone send nothing. Where the upstream server schedules and may write the
    ///   change, it sends ([`Way::Server`], `1.1`); where it schedules but the change is held,
    ///   it sends later ([`Way::Deferred`], `1.0`); otherwise Holtmail mails ([`Way::Mail`]).
    ///   Attendees with `SCHEDULE-AGENT=NONE` are never told, those with `CLIENT` only when the
    ///   origin is [`Origin::Web`] and the upstream server schedules.
    /// - Attendee: a changed own `PARTSTAT`, or deleting the event (a decline), answers the
    ///   organizer: through the upstream server where it schedules and `respond` is allowed
    ///   ([`Way::Server`]), not at all for `SCHEDULE-AGENT=NONE` on the organizer
    ///   ([`Way::Quiet`]), otherwise as an iMIP `REPLY` ([`Way::Mail`]) as `reply_as` or as the
    ///   invited own address.
    /// - Mail is submitted as `change.actor` from the identity of the organizer's or attendee's
    ///   address; without one the recipients get `3.7`, a refused or rate-limited submission
    ///   `5.1`, and nothing else is tried for them.
    /// - A message identical to one delivered within [`RECENT`] and not undone since is not
    ///   queued again; its recipients get `1.1` and it counts in `suppressed`. A message the
    ///   outbox already holds as cancelled is queued anew.
    /// - `SCHEDULE-STATUS` is written on each attendee the change concerns (on the `ORGANIZER`
    ///   for an answer); other attendees keep the status the previous copy had.
    ///
    /// [`ScheduleError::Unavailable`] leaves nothing queued.
    async fn deliver(&self, change: &Change) -> Result<Delivery, ScheduleError>;

    /// Lets the held messages of a delivery whose change was recorded go: each leaves once its
    /// undo window has passed from now. Calling it twice is harmless.
    async fn confirm(&self, actor: &Actor, delivery: &Delivery) -> Result<(), ScheduleError>;

    /// Takes back the messages of a delivery whose change was not recorded. Messages that
    /// already left are left alone; calling it twice is harmless. Only messages actually taken
    /// back stop counting as just sent.
    async fn withdraw(&self, actor: &Actor, delivery: &Delivery) -> Result<(), ScheduleError>;

    /// Decides what an incoming iTIP message does. Never sends anything, also not for a
    /// merged `REPLY`.
    ///
    /// Ignored, in this order: not live, in a Sent folder, an outbox copy, `From` an own
    /// address, not readable iTIP, not addressed to an own address. A `REQUEST` or `CANCEL`
    /// updates "From mail" unless the organizer is own or an upstream calendar holds the `UID`.
    /// A `REPLY` is applied only when it names exactly one attendee who is `From` (or the
    /// signed sender behind an alias), the mail has exactly one `From` address, DKIM passed
    /// for that sender's domain or a parent of it or DMARC passed for exactly its domain (or
    /// the alias service signed it, or `force`), and the organizer copy is the user's own.
    async fn incoming(&self, mail: &IncomingItip) -> Result<Incoming, ScheduleError>;

    /// Whether this scheduler delivered a message with `key` in `archive` to every one of
    /// `recipients` (in any spelling) within [`RECENT`], not undone since. False for no
    /// recipients.
    async fn just_sent(
        &self,
        archive: ArchiveId,
        key: &ItipKey,
        recipients: &[String],
    ) -> Result<bool, ScheduleError>;
}
