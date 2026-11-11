//! Typed parameters of every node type; their JSON Schemas are what the editor shows.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters of a node type that takes none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// `event.upcoming`: fires a set time before an event starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpcomingParams {
    /// Minutes before the start, 1 to 40320 (four weeks).
    #[schemars(range(min = 1, max = 40320))]
    pub minutes_before: u32,
}

/// `schedule`: fires on a cron schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleParams {
    /// Five fields: minute, hour, day of month, month, day of week.
    #[schemars(extend("x-holtmail" = "cron"))]
    pub cron: String,
    /// IANA time zone the fields are read in.
    #[serde(default = "utc")]
    #[schemars(extend("x-holtmail" = "timezone"))]
    pub timezone: String,
}

fn utc() -> String {
    "UTC".to_owned()
}

/// `query`: a search in the search language, matched against the event's message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryParams {
    /// The search, as typed into the search box.
    #[schemars(length(min = 1, max = 2000), extend("x-holtmail" = "query"))]
    pub query: String,
}

/// How a header value is compared. Every comparison ignores ASCII case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeaderOp {
    /// The header is present.
    Exists,
    /// A value equals the text after trimming.
    Equals,
    /// A value contains the text.
    Contains,
    /// A value starts with the text.
    StartsWith,
    /// A value ends with the text.
    EndsWith,
}

/// `header`: a test on one header field of the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderParams {
    /// Field name, such as `List-Id`.
    #[schemars(length(min = 1, max = 200))]
    pub name: String,
    /// The comparison.
    pub op: HeaderOp,
    /// The text compared; ignored by `exists`.
    #[serde(default)]
    #[schemars(length(max = 1000))]
    pub value: String,
}

/// `alias`: the message reached the archive through an alias.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AliasParams {
    /// Only aliases of this service id (`simplelogin`, `addy`, ...); any alias when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

/// Sender trust as derived for the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// DKIM aligned with the `From` domain.
    Aligned,
    /// Relayed by a signing alias service.
    Relayed,
    /// Checked and not aligned.
    Unverified,
    /// Nothing trusted to read.
    Unknown,
}

/// `trust`: the sender's trust is one of the listed states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustParams {
    /// Accepted states, at least one.
    #[schemars(length(min = 1))]
    pub any_of: Vec<Trust>,
}

/// `insight`: the message has an insight of this kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InsightParams {
    /// Insight kind id, such as `code` or `parcel`.
    #[schemars(length(min = 1, max = 64))]
    pub kind: String,
}

/// What a counter counts.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CounterScope {
    /// Archived messages from the event message's sender address.
    Sender,
    /// Runs of this flow that planned at least one action.
    Flow,
}

/// The rolling window a counter looks back over, ending at the event.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CounterWindow {
    /// The last 60 minutes.
    Hour,
    /// The last 24 hours.
    Day,
    /// The last 7 days.
    Week,
}

/// `counter`: more than a number of things happened in a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CounterParams {
    /// What is counted.
    pub scope: CounterScope,
    /// How far back.
    pub window: CounterWindow,
    /// `yes` when the count is greater than this.
    pub above: u32,
}

/// Day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Day {
    /// Monday.
    Mon,
    /// Tuesday.
    Tue,
    /// Wednesday.
    Wed,
    /// Thursday.
    Thu,
    /// Friday.
    Fri,
    /// Saturday.
    Sat,
    /// Sunday.
    Sun,
}

/// `time_window`: the event happened on one of the days, between two times of day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeWindowParams {
    /// Days the window is open; every day when empty.
    #[serde(default)]
    pub days: Vec<Day>,
    /// Start, `HH:MM`, inclusive.
    #[schemars(extend("x-holtmail" = "time"))]
    pub from: String,
    /// End, `HH:MM`, exclusive; earlier than `from` wraps past midnight.
    #[schemars(extend("x-holtmail" = "time"))]
    pub to: String,
    /// IANA time zone.
    #[serde(default = "utc")]
    #[schemars(extend("x-holtmail" = "timezone"))]
    pub timezone: String,
}

/// `label`: a local label on the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelParams {
    /// The label.
    #[schemars(length(min = 1, max = 200))]
    pub name: String,
}

/// A system flag on the mail server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    /// `\Seen`.
    Seen,
    /// `\Flagged`.
    Flagged,
    /// `\Answered`.
    Answered,
}

/// `set_flags`: set and clear flags on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlagsParams {
    /// Flags to set.
    #[serde(default)]
    pub set: Vec<Flag>,
    /// Flags to clear.
    #[serde(default)]
    pub clear: Vec<Flag>,
}

/// `move`: move the message to a folder on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveParams {
    /// Folder path as the server names it.
    #[schemars(length(min = 1, max = 500), extend("x-holtmail" = "folder"))]
    pub folder: String,
}

/// `reply`: an automatic reply to the sender from a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyParams {
    /// Identity id the reply is sent as; `auto` lets the route pick it as for a reply written by
    /// hand, so mail that came through an alias is answered through it.
    #[schemars(extend("x-holtmail" = "identity"))]
    pub identity: String,
    /// Subject template.
    #[serde(default = "re_subject")]
    #[schemars(extend("x-holtmail" = "template"))]
    pub subject: String,
    /// Body template, plain text.
    #[schemars(extend("x-holtmail" = "template"))]
    pub body: String,
    /// At most one reply per sender in this many days (RFC 3834), 1 to 365.
    #[serde(default = "seven")]
    #[schemars(range(min = 1, max = 365))]
    pub days: u32,
}

fn re_subject() -> String {
    "Re: {{ subject }}".to_owned()
}

fn seven() -> u32 {
    7
}

/// `forward`: forward the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForwardParams {
    /// Recipients, at least one.
    #[schemars(length(min = 1, max = 20), extend("x-holtmail" = "addresses"))]
    pub to: Vec<String>,
    /// Identity id the forward is sent as; `auto` lets the route pick it.
    #[schemars(extend("x-holtmail" = "identity"))]
    pub identity: String,
    /// Note above the forwarded message, a template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-holtmail" = "template"))]
    pub note: Option<String>,
    /// Attach the original as `message/rfc822` instead of quoting it.
    #[serde(default)]
    pub as_attachment: bool,
}

/// `send`: send a new message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendParams {
    /// Recipients, at least one.
    #[schemars(length(min = 1, max = 20), extend("x-holtmail" = "addresses"))]
    pub to: Vec<String>,
    /// Identity id the message is sent as; `auto` is the saver's default identity.
    #[schemars(extend("x-holtmail" = "identity"))]
    pub identity: String,
    /// Subject template.
    #[schemars(extend("x-holtmail" = "template"))]
    pub subject: String,
    /// Body template, plain text.
    #[schemars(extend("x-holtmail" = "template"))]
    pub body: String,
}

/// An answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// `ACCEPTED`.
    Accepted,
    /// `TENTATIVE`.
    Tentative,
    /// `DECLINED`.
    Declined,
}

/// `rsvp`: answer the invitation the event carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RsvpParams {
    /// The answer.
    pub answer: Answer,
    /// Comment for the organizer, a template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-holtmail" = "template"))]
    pub comment: Option<String>,
}

/// `create_event`: a new event in a calendar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateEventParams {
    /// Calendar id; the archive's default calendar when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-holtmail" = "calendar"))]
    pub calendar: Option<String>,
    /// Title template.
    #[schemars(extend("x-holtmail" = "template"))]
    pub title: String,
    /// Start template that renders to an RFC 3339 time.
    #[schemars(extend("x-holtmail" = "template"))]
    pub starts: String,
    /// Length in minutes, 1 to 10080.
    #[serde(default = "sixty")]
    #[schemars(range(min = 1, max = 10080))]
    pub duration_minutes: u32,
}

fn sixty() -> u32 {
    60
}

/// `create_contact`: the message's sender as a new contact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateContactParams {
    /// Address book id; the archive's default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-holtmail" = "address_book"))]
    pub address_book: Option<String>,
}

/// `notify`: a notification on the owner's devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotifyParams {
    /// Title template.
    #[schemars(extend("x-holtmail" = "template"))]
    pub title: String,
    /// Body template.
    #[serde(default)]
    #[schemars(extend("x-holtmail" = "template"))]
    pub body: String,
}

/// `webhook`: call a configured webhook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookParams {
    /// Webhook id.
    #[schemars(extend("x-holtmail" = "webhook"))]
    pub hook: String,
}

/// `set_identity`: send the outgoing message as another identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityParams {
    /// Identity id.
    #[schemars(extend("x-holtmail" = "identity"))]
    pub identity: String,
}

/// `set_transport`: send the outgoing message through another transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransportParams {
    /// Transport id.
    #[schemars(extend("x-holtmail" = "transport"))]
    pub transport: String,
}

/// How a message to an alias contact is routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Sent as is.
    Direct,
    /// Through the alias service's reverse address.
    ReverseAlias,
    /// To the `Reply-To` the alias service set.
    ReplyHeader,
}

/// `set_alias_strategy`: route the outgoing message with another alias strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StrategyParams {
    /// The strategy.
    pub strategy: Strategy,
}

/// `add_bcc`: a blind copy of the outgoing message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BccParams {
    /// The address.
    #[schemars(length(min = 3, max = 320), extend("x-holtmail" = "address"))]
    pub address: String,
}

/// `require_confirmation` and `block`: a reason shown to the sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReasonParams {
    /// The reason.
    #[schemars(length(min = 1, max = 500))]
    pub reason: String,
}
