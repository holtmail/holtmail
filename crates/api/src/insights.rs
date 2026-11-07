use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::Json;
use axum::extract::{Path, State};
use holtmail_category::{CategorySource, Topic, TopicSet, TrustState};
use holtmail_core::{ArchiveId, MessageId, Timestamp};
use holtmail_fetch::OneClick;
use holtmail_insight::{
    Confidence, Date, Insight, InsightKind, Items, KindTag, KindTags, LinkAction, LocalOrZoned,
    Money, PassDate, PassStyle, Reservation, Signal, Signals, Url,
};
use holtmail_insights::Deriver;
use holtmail_store::{
    CategoryPolicy, CategoryView, InsightOutcome, InsightView, Message, UnsubscribeOutcome,
    UnsubscribeRequest, User,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::dto::ErrorDto;
use crate::insights_data::DataStatus;
use crate::{ApiError, ApiResult, ApiState};

/// What the API needs to serve message insights. `ApiState.insights` is `None` when the server
/// runs no insights runner; messages then carry no `insights` and the status route answers `404`.
#[derive(Clone)]
pub struct InsightsConfig {
    /// The runner's current `extract_fp`. A stored row with another fingerprint is `outdated`.
    pub fingerprint: String,
    /// The lowest score at which a `Possible` item of a kind is shown. Kinds not listed show
    /// `Likely` and `Certain` items only.
    pub show_possible: HashMap<KindTag, u16>,
    /// The walk's progress, shared with the server that runs it.
    pub progress: Arc<InsightProgress>,
    /// `[insights.categories] health`. When `false`, the `health` topic is left out of every
    /// response, counts included, and overrides store it without applying it.
    pub health: bool,
    /// The stage-B deriver the server derives categories with. With it the status route counts
    /// the categories rows derived under another fingerprint; without it that count is 0.
    pub deriver: Option<Arc<Deriver>>,
    /// Sends one-click unsubscribe requests. `None` turns them off (`[insights] one_click =
    /// false`): the reader then gives the reason `disabled` and the unsubscribe route refuses.
    pub one_click: Option<Arc<dyn OneClick>>,
    /// The category data files the server loaded and its update state, shared with the server
    /// that loads them; `GET /api/insights/data` serves its latest value.
    pub data: Arc<DataStatus>,
}

impl std::fmt::Debug for InsightsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InsightsConfig")
            .field("fingerprint", &self.fingerprint)
            .field("show_possible", &self.show_possible)
            .field("health", &self.health)
            .field("deriver", &self.deriver.is_some())
            .field("one_click", &self.one_click.is_some())
            .finish_non_exhaustive()
    }
}

impl InsightsConfig {
    /// A config for `fingerprint` that shows no `Possible` items and the `health` topic, with a
    /// fresh progress handle, an empty data status and one-click unsubscribe turned off.
    pub fn new(fingerprint: impl Into<String>) -> Self {
        Self {
            fingerprint: fingerprint.into(),
            show_possible: HashMap::new(),
            progress: Arc::new(InsightProgress::default()),
            health: true,
            deriver: None,
            one_click: None,
            data: Arc::new(DataStatus::default()),
        }
    }

    pub(crate) fn policy(&self) -> CategoryPolicy {
        CategoryPolicy {
            health: self.health,
        }
    }
}

/// Whether insights are being computed, and how many messages were done in the current or last
/// run. Safe to share between threads; readers see each update as a whole value.
#[derive(Debug, Default)]
pub struct InsightProgress {
    running: AtomicBool,
    done: AtomicU64,
}

impl InsightProgress {
    /// Marks a run as started and resets the count.
    pub fn start(&self) {
        self.done.store(0, Ordering::Relaxed);
        self.running.store(true, Ordering::Release);
    }

    /// Adds `messages` to the count of the current run.
    pub fn advance(&self, messages: u64) {
        self.done.fetch_add(messages, Ordering::Relaxed);
    }

    /// Marks the run as finished; the count is kept until the next start.
    pub fn finish(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// Whether a run is going on.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Messages done in the current or last run.
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }
}

/// How current a message's insights are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightStatusDto {
    /// Computed by the current extractors.
    Ready,
    /// Not computed yet.
    Pending,
    /// Computed by older extractors; the items are shown until they are recomputed.
    Outdated,
    /// An extractor failed; the items of the others are shown.
    Partial,
    /// The message could not be parsed.
    Unparsable,
    /// The message bytes could not be read.
    Unreadable,
}

/// Whether the sender is who the message says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustStateDto {
    /// A trusted server recorded a DKIM pass aligned with the `From` domain.
    Aligned,
    /// Relayed by a signed alias service; the original sender is not verified.
    Relayed,
    /// A trusted server's field was read and the sender did not check out, or an unsigned alias.
    Unverified,
    /// Nothing trusted to read: no trusted authentication server, or none stamped the message.
    /// Also the state before categories are derived.
    Unknown,
}

/// The sender trust of a message, from its derived categories row.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrustDto {
    pub state: TrustStateDto,
    /// The aligned domain, for `aligned`.
    pub domain: Option<String>,
    /// The alias service, for `relayed`.
    pub provider: Option<String>,
}

impl TrustDto {
    fn of(trust: Option<&TrustState>) -> Self {
        let (state, domain, provider) = match trust {
            Some(TrustState::Aligned { domain }) => {
                (TrustStateDto::Aligned, Some(domain.clone()), None)
            }
            Some(TrustState::Relayed { provider }) => {
                (TrustStateDto::Relayed, None, Some(provider.clone()))
            }
            Some(TrustState::Unverified) => (TrustStateDto::Unverified, None, None),
            Some(TrustState::Unknown) | None => (TrustStateDto::Unknown, None, None),
        };
        Self {
            state,
            domain,
            provider,
        }
    }
}

/// Which layer decided a kind or topic. A higher layer wins: `override` over `content` over
/// `domain` over `headers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CategorySourceDto {
    /// A user's override for the sender's address or domain.
    Override,
    /// What was found in the message.
    Content,
    /// The domain data file.
    Domain,
    /// The message's header fields.
    Headers,
}

impl From<CategorySource> for CategorySourceDto {
    fn from(source: CategorySource) -> Self {
        match source {
            CategorySource::Override => Self::Override,
            CategorySource::Content => Self::Content,
            CategorySource::Domain => Self::Domain,
            CategorySource::Headers => Self::Headers,
        }
    }
}

/// The kind of a message: who sent it and how.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryKindDto {
    /// `personal`, `list`, `bulk` or `notification`.
    pub id: String,
    pub source: CategorySourceDto,
    /// An override set it.
    pub overridden: bool,
}

/// Why a topic of an override does not apply to this message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeldBackDto {
    /// A sensitive topic needs an aligned sender, and this one is not.
    NotVerified,
}

/// One topic of a message: what it is about.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryTopicDto {
    /// `purchases`, `travel`, `finance`, `health`, `social`, `jobs`, `events` or `government`.
    pub id: String,
    pub source: CategorySourceDto,
    /// An override set it.
    pub overridden: bool,
    /// Set on a topic of the override that the trust gate kept off this message; such a topic
    /// does not apply to it. Absent on topics that apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_back: Option<HeldBackDto>,
}

/// How sure the extractor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceDto {
    Possible,
    Likely,
    Certain,
}

/// One item found in a message, tagged by `type`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InsightDto {
    /// A one-time code to copy.
    Code {
        value: String,
        /// The origin of an origin-bound code line (`@example.com #123456`).
        origin_bound: Option<String>,
        confidence: ConfidenceDto,
        /// Rank among items of the same kind, `0..=1000`.
        score: u16,
        /// Found below an inline-forward marker.
        forwarded: bool,
    },
    /// The unsubscribe options the message declares.
    Unsubscribe {
        https: Option<String>,
        /// A `mailto:` URI.
        mailto: Option<String>,
        http: Option<String>,
        /// RFC 8058 one-click is declared. Whether the server may use it is in
        /// `MessageInsightsDto.unsubscribe.one_click`.
        one_click: bool,
        list_id: Option<String>,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
    /// A link the reader can act on. The flags are computed when the response is built, from the
    /// stored hosts and the Public Suffix List the server runs with.
    Action {
        action: LinkActionDto,
        /// The link as found in the message.
        url: String,
        /// Where it leads after unwrapping; equal to `url` when nothing was unwrapped.
        destination: String,
        /// The destination's host to show: Unicode, or the A-label form when `confusable`.
        display_host: String,
        /// The unwrapping service, such as `outlook.com`.
        via: Option<String>,
        /// A known opaque click tracker remains in `destination`.
        tracked: bool,
        /// The anchor text reads as a host whose registrable domain is not the destination's.
        text_domain_mismatch: bool,
        /// The destination's registrable domain is not the sender's: the claimed real sender of
        /// an alias message once categories are derived, the `From` domain before.
        destination_differs_from_sender: bool,
        /// A destination label is mixed-script or confusable (UTS 39).
        confusable: bool,
        /// The anchor's label, display text.
        label: Option<String>,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
    /// A parcel tracking number.
    Parcel {
        /// The carrier id, such as `ups`.
        carrier: String,
        /// The tracking number as the carrier prints it.
        number: String,
        /// The carrier's check digit was verified.
        check_digit: bool,
        /// The carrier's tracking page, built from its template, never from the message.
        track_url: Option<String>,
        /// The display host of `track_url`.
        track_host: Option<String>,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
    /// A reservation from schema.org markup, one flight leg, stay, trip, event, table or car.
    Reservation {
        reservation: ReservationDto,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
    /// An order.
    Order {
        /// The order number.
        number: Option<String>,
        /// The seller, display text.
        merchant: Option<String>,
        total: Option<MoneyDto>,
        /// The status as the sender names it, display text.
        status: Option<String>,
        /// The order's page; `null` when it has no host.
        url: Option<String>,
        /// The display host of `url`: Unicode, or the A-label form when confusable.
        url_host: Option<String>,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
    /// An invoice.
    Invoice {
        /// The invoice number.
        number: Option<String>,
        /// Who is to be paid, display text.
        payee: Option<String>,
        /// The amount due.
        total: Option<MoneyDto>,
        /// The due date, `YYYY-MM-DD`.
        due: Option<String>,
        /// The invoice's page; `null` when it has no host.
        url: Option<String>,
        /// The display host of `url`.
        url_host: Option<String>,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
    /// A wallet pass attachment.
    Pass {
        style: PassStyleDto,
        /// The issuer, display text.
        organization: String,
        /// What the pass is, display text.
        description: String,
        /// When the pass is relevant.
        relevant: Option<TimeDto>,
        /// The boarding details of a boarding pass.
        boarding: Option<BoardingPassDto>,
        /// The pass attachment's position among the message's attachments, counting from 0.
        attachment: u16,
        /// The pass signature was verified. Always `false` for now.
        signature_checked: bool,
        confidence: ConfidenceDto,
        score: u16,
        forwarded: bool,
    },
}

/// A time as the message wrote it. Never converted to UTC or to the server's zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TimeDto {
    /// Wall-clock time as written, `YYYY-MM-DDTHH:MM:SS`.
    pub local: String,
    /// The offset written with it, such as `+01:00`; `null` for a time written without one.
    pub offset: Option<String>,
    /// The IANA zone name, when the message gave one.
    pub zone: Option<String>,
}

/// An exact amount of money. Never a float.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MoneyDto {
    /// The decimal amount with exactly `exponent` fraction digits, such as `1234.50`, `-3.000` or
    /// `1200`. A string, since amounts can exceed what a JSON number holds exactly.
    pub amount: String,
    /// The ISO 4217 code.
    pub currency: String,
    /// Fraction digits of the currency: 0 for JPY, 2 for USD, 3 for KWD.
    pub exponent: u8,
}

/// A reservation, tagged by `kind`. Every `url` comes with its `url_host`, the display host
/// (Unicode, or the A-label form when confusable); a URL without a host is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReservationDto {
    /// One flight leg.
    Flight {
        /// The booking reference.
        reservation_number: Option<String>,
        /// The airline's IATA code.
        carrier: Option<String>,
        /// The flight number without the airline code.
        flight_number: Option<String>,
        /// IATA code.
        departure_airport: String,
        /// IATA code.
        arrival_airport: String,
        departure: TimeDto,
        arrival: Option<TimeDto>,
        /// Passenger names, at most 9.
        passengers: Vec<String>,
        /// Seats, at most 9.
        seats: Vec<String>,
        url: Option<String>,
        url_host: Option<String>,
    },
    /// A hotel or other lodging.
    Lodging {
        /// The booking reference.
        reservation_number: String,
        name: String,
        address: Option<String>,
        checkin: TimeDto,
        checkout: Option<TimeDto>,
        guests: Vec<String>,
        url: Option<String>,
        url_host: Option<String>,
    },
    /// A train trip.
    Train {
        /// The booking reference.
        reservation_number: Option<String>,
        /// The train number.
        number: Option<String>,
        departure_station: String,
        arrival_station: Option<String>,
        departure: TimeDto,
        arrival: Option<TimeDto>,
        passengers: Vec<String>,
        seats: Vec<String>,
        url: Option<String>,
        url_host: Option<String>,
    },
    /// A bus trip.
    Bus {
        /// The booking reference.
        reservation_number: Option<String>,
        /// The bus number.
        number: Option<String>,
        departure_station: String,
        arrival_station: Option<String>,
        departure: TimeDto,
        arrival: Option<TimeDto>,
        passengers: Vec<String>,
        seats: Vec<String>,
        url: Option<String>,
        url_host: Option<String>,
    },
    /// An event ticket.
    Event {
        /// The booking reference.
        reservation_number: Option<String>,
        name: String,
        start: TimeDto,
        end: Option<TimeDto>,
        venue: Option<String>,
        /// Ticket numbers or seats.
        tickets: Vec<String>,
        url: Option<String>,
        url_host: Option<String>,
    },
    /// A restaurant table.
    Food {
        /// The booking reference.
        reservation_number: Option<String>,
        name: String,
        start: TimeDto,
        party_size: Option<u16>,
        url: Option<String>,
        url_host: Option<String>,
    },
    /// A rental car.
    RentalCar {
        /// The booking reference.
        reservation_number: String,
        company: Option<String>,
        pickup: TimeDto,
        pickup_location: Option<String>,
        dropoff: Option<TimeDto>,
        dropoff_location: Option<String>,
        url: Option<String>,
        url_host: Option<String>,
    },
}

/// The style of a wallet pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PassStyleDto {
    BoardingPass,
    EventTicket,
    Coupon,
    StoreCard,
    Generic,
}

/// Boarding details of a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BoardingPassDto {
    /// The booking reference.
    pub pnr: Option<String>,
    /// The passenger's name, display text.
    pub passenger: Option<String>,
    /// The legs, in travel order.
    pub legs: Vec<BoardingLegDto>,
}

/// One leg of a boarding pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BoardingLegDto {
    /// Departure airport, IATA code.
    pub from: String,
    /// Arrival airport, IATA code.
    pub to: String,
    /// The operating airline's IATA code.
    pub carrier: Option<String>,
    /// The flight number without the airline code.
    pub flight: Option<String>,
    pub date: Option<PassDateDto>,
    pub seat: Option<String>,
}

/// A flight date on a pass: a full `YYYY-MM-DD` date, or month and day when no year is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(untagged)]
pub enum PassDateDto {
    Full(String),
    MonthDay {
        /// 1–12.
        month: u8,
        /// 1–31.
        day: u8,
    },
}

/// What an action link does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkActionDto {
    Verify,
    Confirm,
    Activate,
    ResetPassword,
    SignIn,
    MagicLink,
    ViewOrder,
    Track,
    CheckIn,
    WalletSave,
    View,
}

impl From<LinkAction> for LinkActionDto {
    fn from(action: LinkAction) -> Self {
        match action {
            LinkAction::Verify => Self::Verify,
            LinkAction::Confirm => Self::Confirm,
            LinkAction::Activate => Self::Activate,
            LinkAction::ResetPassword => Self::ResetPassword,
            LinkAction::SignIn => Self::SignIn,
            LinkAction::MagicLink => Self::MagicLink,
            LinkAction::ViewOrder => Self::ViewOrder,
            LinkAction::Track => Self::Track,
            LinkAction::CheckIn => Self::CheckIn,
            LinkAction::WalletSave => Self::WalletSave,
            LinkAction::View => Self::View,
        }
    }
}

/// Why one-click unsubscribe is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OneClickReasonDto {
    /// No usable `List-Unsubscribe-Post: List-Unsubscribe=One-Click` field.
    NoPostHeader,
    /// The message declares no https unsubscribe link on port 443 without a user name or
    /// password.
    NotHttps,
    /// More than one `List-Unsubscribe` or `List-Unsubscribe-Post` field.
    MultipleFields,
    /// No aligned signature covers both unsubscribe fields.
    NotCovered,
    /// The sender is not aligned.
    NotAligned,
    /// No trusted authentication server stamped the message.
    NoTrustedAuthserv,
    /// One-click requests are turned off on this server.
    Disabled,
    /// A request for the same list was accepted or redirected less than 24 hours ago; see
    /// `UnsubscribeDto.last_request`.
    RecentlyRequested,
    /// Members cannot unsubscribe.
    Members,
}

/// Whether the server can unsubscribe with one click.
#[derive(Debug, Serialize, ToSchema)]
pub struct OneClickDto {
    pub available: bool,
    /// Why not, when not available.
    pub reason: Option<OneClickReasonDto>,
}

/// How to leave the list a message came from.
#[derive(Debug, Serialize, ToSchema)]
pub struct UnsubscribeDto {
    pub https: Option<String>,
    /// A `mailto:` URI, opened by the reader's own mail program.
    pub mailto: Option<String>,
    pub http: Option<String>,
    pub one_click: OneClickDto,
    /// The latest one-click request for the same list in this archive, from any message; left
    /// out when there is none, and always for members.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_request: Option<UnsubscribeRequestDto>,
}

/// How a one-click unsubscribe request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnsubscribeOutcomeDto {
    /// The list answered with a 2xx status.
    Accepted,
    /// The list answered with a redirect, which was not followed; opening the https link may
    /// finish the unsubscribe.
    Redirected,
    /// The request was not sent: the list's address is not public or not allowed.
    Refused,
    /// The list answered with another status.
    HttpError,
    /// The list could not be reached: name resolution, connection, TLS or the 10 s timeout.
    Network,
}

impl From<UnsubscribeOutcome> for UnsubscribeOutcomeDto {
    fn from(outcome: UnsubscribeOutcome) -> Self {
        match outcome {
            UnsubscribeOutcome::Accepted => Self::Accepted,
            UnsubscribeOutcome::Redirected => Self::Redirected,
            UnsubscribeOutcome::Refused => Self::Refused,
            UnsubscribeOutcome::HttpError => Self::HttpError,
            UnsubscribeOutcome::Network => Self::Network,
        }
    }
}

/// One recorded one-click unsubscribe request.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UnsubscribeRequestDto {
    pub outcome: UnsubscribeOutcomeDto,
    /// The HTTP status, when the list answered.
    pub status: Option<u16>,
    /// Unix milliseconds.
    pub requested_at: i64,
}

impl From<&UnsubscribeRequest> for UnsubscribeRequestDto {
    fn from(request: &UnsubscribeRequest) -> Self {
        Self {
            outcome: request.outcome.into(),
            status: request.status,
            requested_at: request.requested_at.as_millis(),
        }
    }
}

/// What was found in a message.
#[derive(Debug, Serialize, ToSchema)]
pub struct MessageInsightsDto {
    pub status: InsightStatusDto,
    /// `unknown` until categories are derived.
    pub trust: TrustDto,
    /// The `From` is one of the archive's own addresses: an account's or an alias's.
    pub own: bool,
    /// The effective kind; `null` until categories are derived.
    pub kind: Option<CategoryKindDto>,
    /// The effective topics in taxonomy order, with the override topics the trust gate kept off
    /// marked `held_back`. `health` is left out when the server turns it off.
    pub topics: Vec<CategoryTopicDto>,
    /// `Likely` and `Certain` items, and `Possible` ones the server is set to show.
    pub items: Vec<InsightDto>,
    /// Present when the message declares a way to unsubscribe.
    pub unsubscribe: Option<UnsubscribeDto>,
}

/// Progress of computing insights in an archive. The web shows "Some messages are still being
/// read (N left)" with N = `pending + categorize + reindex` while N is above zero.
#[derive(Debug, Serialize, ToSchema)]
pub struct InsightsStatusDto {
    /// Messages with no insights or with insights from older extractors.
    pub pending: u64,
    /// Messages whose search entry is behind their categories or insights: `category:`, `has:`
    /// and `is:` searches may miss them until they are indexed again. Counted apart from
    /// `pending`; a message can be in both.
    pub reindex: u64,
    /// Messages whose categories were derived under older settings (category data, trusted
    /// authserv-ids, own addresses, the `health` flag): `category:` and `is:` searches may use
    /// the old values until they are derived again. Counted apart from `pending` and `reindex`.
    pub categorize: u64,
    /// The server is computing insights now.
    pub running: bool,
}

/// Who reads, and what counts as their own mail.
pub(crate) struct Viewer {
    member: bool,
    own: HashSet<String>,
}

impl Viewer {
    pub(crate) async fn new(state: &ApiState, user: &User, archive: ArchiveId) -> ApiResult<Self> {
        let mut own: HashSet<String> = state
            .store
            .list_accounts(archive)
            .await?
            .into_iter()
            .filter_map(|a| a.address)
            .map(|a| a.trim().to_lowercase())
            .collect();
        own.extend(state.store.own_alias_addresses(archive).await?);
        Ok(Self {
            member: !crate::access::manages(user),
            own,
        })
    }

    fn owns(&self, message: &Message) -> bool {
        message
            .from
            .iter()
            .any(|a| self.own.contains(&a.email.trim().to_lowercase()))
    }

    /// The insight kinds a list row shows for `message`.
    pub(crate) fn kinds(&self, message: &Message, bits: u64) -> Vec<String> {
        let own = bits != 0 && self.owns(message);
        KindTags::from_bits(bits)
            .iter()
            .filter(|k| match k {
                KindTag::Code => !self.member && !own,
                KindTag::Link => !own,
                _ => true,
            })
            .map(|k| k.name().to_owned())
            .collect()
    }
}

/// The views of `ids`, read once; empty when the server has no insights runner.
pub(crate) async fn views(
    state: &ApiState,
    ids: &[MessageId],
) -> ApiResult<HashMap<MessageId, InsightView>> {
    if state.insights.is_none() || ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(state.store.insight_views_among(ids).await?)
}

/// The view of one message; `None` when it has none or the server has no insights runner.
pub(crate) async fn view(state: &ApiState, id: MessageId) -> ApiResult<Option<InsightView>> {
    if state.insights.is_none() {
        return Ok(None);
    }
    Ok(state.store.insight_view(id).await?)
}

/// The latest unsubscribe requests of `archive` for the lists the views' unsubscribe items
/// name, keyed by list key. Nothing is read for members or without an insights runner; one
/// key is read with `last_unsubscribe`, more with one `last_unsubscribes`.
pub(crate) async fn last_requests<'a>(
    state: &ApiState,
    viewer: &Viewer,
    archive: ArchiveId,
    views: impl IntoIterator<Item = &'a InsightView>,
) -> ApiResult<HashMap<String, UnsubscribeRequest>> {
    let Some(config) = state.insights.as_ref() else {
        return Ok(HashMap::new());
    };
    if viewer.member {
        return Ok(HashMap::new());
    }
    let mut keys: Vec<String> = views
        .into_iter()
        .filter_map(|view| view_list_key(config, view))
        .collect();
    keys.sort();
    keys.dedup();
    Ok(match keys.as_slice() {
        [] => HashMap::new(),
        [key] => state
            .store
            .last_unsubscribe(archive, key)
            .await?
            .map(|request| (key.clone(), request))
            .into_iter()
            .collect(),
        _ => state.store.last_unsubscribes(archive, &keys).await?,
    })
}

/// The list key of the view's unsubscribe item, as [`list_key`] gives it.
pub(crate) fn view_list_key(config: &InsightsConfig, view: &InsightView) -> Option<String> {
    let items = Items::from_json(&view.insights.items).ok()?.items.items;
    match &unsubscribe_item(config, &items)?.kind {
        InsightKind::Unsubscribe { list_id, .. } => {
            list_key(list_id.as_deref(), view.categories.as_ref())
        }
        _ => None,
    }
}

/// What one-click requests are counted against: the `List-Id` identifier lowercased, else the
/// sender's registrable domain from the stage-B row; `None` when neither is known.
pub(crate) fn list_key(list_id: Option<&str>, categories: Option<&CategoryView>) -> Option<String> {
    list_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_ascii_lowercase)
        .or_else(|| categories.and_then(|c| c.row.write.sender_domain.clone()))
}

/// The first unsubscribe item the reader shows.
pub(crate) fn unsubscribe_item<'a>(
    config: &InsightsConfig,
    items: &'a [Insight],
) -> Option<&'a Insight> {
    items
        .iter()
        .filter(|i| visible(config, i))
        .find(|i| matches!(i.kind, InsightKind::Unsubscribe { .. }))
}

/// How long an accepted or redirected request blocks another for the same list.
pub(crate) const REPEAT_AFTER_MS: i64 = 24 * 60 * 60 * 1000;
/// How long a failed request blocks another for the same list.
pub(crate) const RETRY_AFTER_MS: i64 = 60 * 1000;

/// The request reached the list less than [`REPEAT_AFTER_MS`] before `now`.
pub(crate) fn recently_accepted(last: &UnsubscribeRequest, now: Timestamp) -> bool {
    matches!(
        last.outcome,
        UnsubscribeOutcome::Accepted | UnsubscribeOutcome::Redirected
    ) && now
        .as_millis()
        .saturating_sub(last.requested_at.as_millis())
        < REPEAT_AFTER_MS
}

/// An https URL on port 443, explicit or default, with a host and without user name or password.
pub(crate) fn one_click_url(url: &Url) -> bool {
    url::Url::parse(url.as_str()).is_ok_and(|url| {
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.host_str().is_some_and(|h| !h.is_empty())
    })
}

/// The insights of `message` as `viewer` may see them; `None` without an insights runner.
/// `last` holds the latest unsubscribe requests by list key, as [`last_requests`] reads them.
pub(crate) fn message_insights(
    config: Option<&InsightsConfig>,
    view: Option<&InsightView>,
    message: &Message,
    viewer: &Viewer,
    last: &HashMap<String, UnsubscribeRequest>,
    now: Timestamp,
) -> Option<MessageInsightsDto> {
    let config = config?;
    let categories = view.and_then(|v| v.categories.as_ref());
    let own = categories.map_or_else(|| viewer.owns(message), |c| c.row.write.own);
    let sender = match categories {
        Some(c) => c.row.write.sender_domain.as_deref(),
        None => message
            .from
            .first()
            .and_then(|a| a.email.rsplit_once('@'))
            .map(|(_, domain)| domain),
    };
    let (status, found) = match view {
        None => (InsightStatusDto::Pending, Vec::new()),
        Some(view) => {
            let row = &view.insights;
            let mut status = match row.outcome {
                InsightOutcome::Ok => InsightStatusDto::Ready,
                InsightOutcome::Partial => InsightStatusDto::Partial,
                InsightOutcome::Unparsable => InsightStatusDto::Unparsable,
                InsightOutcome::Unreadable => InsightStatusDto::Unreadable,
            };
            let found = match Items::from_json(&row.items) {
                Ok(read) => {
                    if read.outdated() {
                        status = InsightStatusDto::Outdated;
                    }
                    read.items.items
                }
                Err(_) => {
                    status = InsightStatusDto::Outdated;
                    Vec::new()
                }
            };
            if row.fingerprint != config.fingerprint {
                status = InsightStatusDto::Outdated;
            }
            (status, found)
        }
    };
    let shown: Vec<&Insight> = found
        .iter()
        .filter(|i| visible(config, i))
        .filter(|i| match i.kind {
            InsightKind::Code { .. } => !viewer.member && !own,
            InsightKind::Action { action, .. } => !own && !(viewer.member && signs_in(action)),
            InsightKind::Unsubscribe { .. } => !viewer.member,
            _ => true,
        })
        .collect();
    let unsubscribe = unsubscribe_item(config, &found).and_then(|i| match &i.kind {
        InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            list_id,
        } => Some(if viewer.member {
            UnsubscribeDto {
                https: None,
                mailto: None,
                http: None,
                one_click: OneClickDto {
                    available: false,
                    reason: Some(OneClickReasonDto::Members),
                },
                last_request: None,
            }
        } else {
            let last_request = list_key(list_id.as_deref(), categories).and_then(|k| last.get(&k));
            let reason = one_click_reason(
                view,
                categories,
                *one_click,
                https.as_ref(),
                config.one_click.is_some(),
                last_request,
                now,
            );
            UnsubscribeDto {
                https: https.as_ref().map(|u| u.as_str().to_owned()),
                mailto: mailto.clone(),
                http: http.as_ref().map(|u| u.as_str().to_owned()),
                one_click: OneClickDto {
                    available: reason.is_none(),
                    reason,
                },
                last_request: last_request.map(UnsubscribeRequestDto::from),
            }
        }),
        _ => None,
    });
    let (kind, topics) = categories.map_or_else(
        || (None, Vec::new()),
        |c| (Some(kind_dto(c)), topic_dtos(c, config.health)),
    );
    Some(MessageInsightsDto {
        status,
        trust: TrustDto::of(categories.map(|c| &c.row.write.trust)),
        own,
        kind,
        topics,
        items: shown
            .into_iter()
            .filter_map(|i| item_dto(i, sender))
            .collect(),
        unsubscribe,
    })
}

fn kind_dto(categories: &CategoryView) -> CategoryKindDto {
    let row = &categories.row;
    let overridden = row.kind_override.is_some();
    CategoryKindDto {
        id: row.kind.name().to_owned(),
        source: if overridden {
            CategorySourceDto::Override
        } else {
            row.write.derived_kind_source.into()
        },
        overridden,
    }
}

fn topic_dtos(categories: &CategoryView, health: bool) -> Vec<CategoryTopicDto> {
    let row = &categories.row;
    let overridden = row.topics_override.is_some();
    let held = categories
        .topics_override
        .as_ref()
        .and_then(|o| o.topics)
        .map(|topics| {
            let applied = topics.gated(&row.write.trust, health);
            topics
                .known()
                .iter()
                .filter(|t| !applied.contains(*t) && t.is_sensitive())
                .filter(|t| health || *t != Topic::Health)
                .collect::<TopicSet>()
        })
        .unwrap_or(TopicSet::EMPTY);
    Topic::ALL
        .iter()
        .copied()
        .filter(|t| health || *t != Topic::Health)
        .filter_map(|topic| {
            let held_back = if row.topics.contains(topic) {
                None
            } else if held.contains(topic) {
                Some(HeldBackDto::NotVerified)
            } else {
                return None;
            };
            Some(CategoryTopicDto {
                id: topic.name().to_owned(),
                source: if overridden || held_back.is_some() {
                    CategorySourceDto::Override
                } else {
                    row.write
                        .derived_sources
                        .get(&topic)
                        .copied()
                        .unwrap_or(CategorySource::Content)
                        .into()
                },
                overridden: overridden || held_back.is_some(),
                held_back,
            })
        })
        .collect()
}

/// Why one-click is not offered, the first that applies in a fixed order; `None` when it is.
/// `https` is the item's https link, `enabled` whether the server sends one-click requests and
/// `last` the latest request for the item's list.
pub(crate) fn one_click_reason(
    view: Option<&InsightView>,
    categories: Option<&CategoryView>,
    one_click: bool,
    https: Option<&Url>,
    enabled: bool,
    last: Option<&UnsubscribeRequest>,
    now: Timestamp,
) -> Option<OneClickReasonDto> {
    let signals = Signals::from_bits(view.map_or(0, |v| v.insights.signals));
    let auth = holtmail_insights::auth_summary(view.map_or("", |v| v.insights.auth.as_str()));
    let trust = categories.map(|c| &c.row.write.trust);
    Some(if !signals.contains(Signal::UnsubscribePost) {
        OneClickReasonDto::NoPostHeader
    } else if !https.is_some_and(one_click_url) {
        OneClickReasonDto::NotHttps
    } else if auth.unsubscribe_fields > 1 || auth.unsubscribe_post_fields > 1 {
        OneClickReasonDto::MultipleFields
    } else if !one_click {
        OneClickReasonDto::NoPostHeader
    } else if matches!(trust, None | Some(TrustState::Unknown)) {
        OneClickReasonDto::NoTrustedAuthserv
    } else if !trust.is_some_and(TrustState::is_aligned) {
        OneClickReasonDto::NotAligned
    } else if !categories.is_some_and(|c| c.row.write.one_click_ok) {
        OneClickReasonDto::NotCovered
    } else if !enabled {
        OneClickReasonDto::Disabled
    } else if last.is_some_and(|last| recently_accepted(last, now)) {
        OneClickReasonDto::RecentlyRequested
    } else {
        return None;
    })
}

fn signs_in(action: LinkAction) -> bool {
    matches!(
        action,
        LinkAction::SignIn
            | LinkAction::MagicLink
            | LinkAction::ResetPassword
            | LinkAction::Verify
            | LinkAction::Confirm
            | LinkAction::Activate
    )
}

fn visible(config: &InsightsConfig, item: &Insight) -> bool {
    item.confidence >= Confidence::Likely
        || config
            .show_possible
            .get(&item.kind.tag())
            .is_some_and(|min| item.score >= *min)
}

fn confidence(confidence: Confidence) -> ConfidenceDto {
    match confidence {
        Confidence::Possible => ConfidenceDto::Possible,
        Confidence::Likely => ConfidenceDto::Likely,
        Confidence::Certain => ConfidenceDto::Certain,
    }
}

fn same_site(a: &str, b: &str) -> bool {
    match (
        holtmail_links::registrable_domain(a),
        holtmail_links::registrable_domain(b),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => a
            .trim_end_matches('.')
            .eq_ignore_ascii_case(b.trim_end_matches('.')),
    }
}

fn item_dto(item: &Insight, sender: Option<&str>) -> Option<InsightDto> {
    match &item.kind {
        InsightKind::Code {
            value,
            origin_bound,
        } => Some(InsightDto::Code {
            value: value.clone(),
            origin_bound: origin_bound.clone(),
            confidence: confidence(item.confidence),
            score: item.score,
            forwarded: item.forwarded,
        }),
        InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            list_id,
        } => Some(InsightDto::Unsubscribe {
            https: https.as_ref().map(|u| u.as_str().to_owned()),
            mailto: mailto.clone(),
            http: http.as_ref().map(|u| u.as_str().to_owned()),
            one_click: *one_click,
            list_id: list_id.clone(),
            confidence: confidence(item.confidence),
            score: item.score,
            forwarded: item.forwarded,
        }),
        InsightKind::Action {
            action,
            url,
            destination,
            via,
            tracked,
            text_host,
            label,
        } => {
            let host = destination.host()?;
            let shown = holtmail_links::display_host(host);
            let differs = sender.is_some_and(|sender| !same_site(host, sender));
            Some(InsightDto::Action {
                action: (*action).into(),
                url: url.as_str().to_owned(),
                destination: destination.as_str().to_owned(),
                display_host: shown.host,
                via: via.clone(),
                tracked: *tracked,
                text_domain_mismatch: text_host.as_deref().is_some_and(|t| !same_site(t, host)),
                destination_differs_from_sender: differs,
                confusable: shown.confusable,
                label: label.clone(),
                confidence: confidence(item.confidence),
                score: item.score,
                forwarded: item.forwarded,
            })
        }
        InsightKind::Parcel {
            carrier,
            number,
            check_digit,
            track_url,
        } => Some(InsightDto::Parcel {
            carrier: carrier.as_str().to_owned(),
            number: number.clone(),
            check_digit: *check_digit,
            track_url: track_url.as_ref().map(|u| u.as_str().to_owned()),
            track_host: track_url
                .as_ref()
                .and_then(|u| u.host())
                .map(|h| holtmail_links::display_host(h).host),
            confidence: confidence(item.confidence),
            score: item.score,
            forwarded: item.forwarded,
        }),
        InsightKind::Reservation(reservation) => Some(InsightDto::Reservation {
            reservation: reservation_dto(reservation),
            confidence: confidence(item.confidence),
            score: item.score,
            forwarded: item.forwarded,
        }),
        InsightKind::Order {
            number,
            merchant,
            total,
            status,
            url,
        } => {
            let (url, url_host) = link(url.as_ref());
            Some(InsightDto::Order {
                number: number.clone(),
                merchant: merchant.clone(),
                total: total.as_ref().map(money),
                status: status.clone(),
                url,
                url_host,
                confidence: confidence(item.confidence),
                score: item.score,
                forwarded: item.forwarded,
            })
        }
        InsightKind::Invoice {
            number,
            payee,
            total,
            due,
            url,
        } => {
            let (url, url_host) = link(url.as_ref());
            Some(InsightDto::Invoice {
                number: number.clone(),
                payee: payee.clone(),
                total: total.as_ref().map(money),
                due: due.map(date),
                url,
                url_host,
                confidence: confidence(item.confidence),
                score: item.score,
                forwarded: item.forwarded,
            })
        }
        InsightKind::Pass {
            style,
            organization,
            description,
            relevant,
            boarding,
            attachment,
            signature_checked,
        } => Some(InsightDto::Pass {
            style: match style {
                PassStyle::BoardingPass => PassStyleDto::BoardingPass,
                PassStyle::EventTicket => PassStyleDto::EventTicket,
                PassStyle::Coupon => PassStyleDto::Coupon,
                PassStyle::StoreCard => PassStyleDto::StoreCard,
                PassStyle::Generic => PassStyleDto::Generic,
            },
            organization: organization.clone(),
            description: description.clone(),
            relevant: relevant.as_ref().map(time),
            boarding: boarding.as_ref().map(|b| BoardingPassDto {
                pnr: b.pnr.clone(),
                passenger: b.passenger.clone(),
                legs: b
                    .legs
                    .iter()
                    .map(|leg| BoardingLegDto {
                        from: leg.from.clone(),
                        to: leg.to.clone(),
                        carrier: leg.carrier.clone(),
                        flight: leg.flight.clone(),
                        date: leg.date.map(|d| match d {
                            PassDate::Full(d) => PassDateDto::Full(date(d)),
                            PassDate::MonthDay { month, day } => {
                                PassDateDto::MonthDay { month, day }
                            }
                        }),
                        seat: leg.seat.clone(),
                    })
                    .collect(),
            }),
            attachment: *attachment,
            signature_checked: *signature_checked,
            confidence: confidence(item.confidence),
            score: item.score,
            forwarded: item.forwarded,
        }),
    }
}

fn reservation_dto(reservation: &Reservation) -> ReservationDto {
    match reservation {
        Reservation::Flight(r) => {
            let (url, url_host) = link(r.url.as_ref());
            ReservationDto::Flight {
                reservation_number: r.reservation_number.clone(),
                carrier: r.carrier.clone(),
                flight_number: r.flight_number.clone(),
                departure_airport: r.departure_airport.clone(),
                arrival_airport: r.arrival_airport.clone(),
                departure: time(&r.departure),
                arrival: r.arrival.as_ref().map(time),
                passengers: r.passengers.clone(),
                seats: r.seats.clone(),
                url,
                url_host,
            }
        }
        Reservation::Lodging(r) => {
            let (url, url_host) = link(r.url.as_ref());
            ReservationDto::Lodging {
                reservation_number: r.reservation_number.clone(),
                name: r.name.clone(),
                address: r.address.clone(),
                checkin: time(&r.checkin),
                checkout: r.checkout.as_ref().map(time),
                guests: r.guests.clone(),
                url,
                url_host,
            }
        }
        Reservation::Train(r) => {
            let (url, url_host) = link(r.url.as_ref());
            ReservationDto::Train {
                reservation_number: r.reservation_number.clone(),
                number: r.train_number.clone(),
                departure_station: r.departure_station.clone(),
                arrival_station: r.arrival_station.clone(),
                departure: time(&r.departure),
                arrival: r.arrival.as_ref().map(time),
                passengers: r.passengers.clone(),
                seats: r.seats.clone(),
                url,
                url_host,
            }
        }
        Reservation::Bus(r) => {
            let (url, url_host) = link(r.url.as_ref());
            ReservationDto::Bus {
                reservation_number: r.reservation_number.clone(),
                number: r.bus_number.clone(),
                departure_station: r.departure_station.clone(),
                arrival_station: r.arrival_station.clone(),
                departure: time(&r.departure),
                arrival: r.arrival.as_ref().map(time),
                passengers: r.passengers.clone(),
                seats: r.seats.clone(),
                url,
                url_host,
            }
        }
        Reservation::Event(r) => {
            let (url, url_host) = link(r.url.as_ref());
            ReservationDto::Event {
                reservation_number: r.reservation_number.clone(),
                name: r.name.clone(),
                start: time(&r.start),
                end: r.end.as_ref().map(time),
                venue: r.venue.clone(),
                tickets: r.tickets.clone(),
                url,
                url_host,
            }
        }
        Reservation::Food(r) => {
            let (url, url_host) = link(r.url.as_ref());
            ReservationDto::Food {
                reservation_number: r.reservation_number.clone(),
                name: r.name.clone(),
                start: time(&r.start),
                party_size: r.party_size,
                url,
                url_host,
            }
        }
        Reservation::RentalCar(r) => {
            let (url, url_host) = link(r.url.as_ref());
            ReservationDto::RentalCar {
                reservation_number: r.reservation_number.clone(),
                company: r.company.clone(),
                pickup: time(&r.pickup),
                pickup_location: r.pickup_location.clone(),
                dropoff: r.dropoff.as_ref().map(time),
                dropoff_location: r.dropoff_location.clone(),
                url,
                url_host,
            }
        }
    }
}

fn link(url: Option<&Url>) -> (Option<String>, Option<String>) {
    match url.and_then(|u| u.host().map(|host| (u, host))) {
        Some((url, host)) => (
            Some(url.as_str().to_owned()),
            Some(holtmail_links::display_host(host).host),
        ),
        None => (None, None),
    }
}

fn time(time: &LocalOrZoned) -> TimeDto {
    const WALL: &str = "%Y-%m-%dT%H:%M:%S";
    match time {
        LocalOrZoned::Local(wall) => TimeDto {
            local: wall.format(WALL).to_string(),
            offset: None,
            zone: None,
        },
        LocalOrZoned::Zoned(at, zone) => TimeDto {
            local: at.naive_local().format(WALL).to_string(),
            offset: Some(at.format("%:z").to_string()),
            zone: zone.clone(),
        },
    }
}

fn date(date: Date) -> String {
    date.0.format("%Y-%m-%d").to_string()
}

fn money(money: &Money) -> MoneyDto {
    let exponent = money.currency.exponent();
    MoneyDto {
        amount: decimal(money.minor, exponent),
        currency: money.currency.code().to_owned(),
        exponent,
    }
}

fn decimal(minor: i64, exponent: u8) -> String {
    let digits = minor.unsigned_abs().to_string();
    let exponent = usize::from(exponent);
    let digits = format!("{digits:0>width$}", width = exponent + 1);
    let (whole, fraction) = digits.split_at(digits.len() - exponent);
    let sign = if minor < 0 { "-" } else { "" };
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

#[cfg(test)]
mod tests {
    use super::decimal;

    #[test]
    fn formats_minor_units_exactly() {
        assert_eq!(decimal(0, 0), "0");
        assert_eq!(decimal(1200, 0), "1200");
        assert_eq!(decimal(123450, 2), "1234.50");
        assert_eq!(decimal(5, 2), "0.05");
        assert_eq!(decimal(-3000, 3), "-3.000");
        assert_eq!(decimal(-1, 4), "-0.0001");
        assert_eq!(decimal(i64::MAX, 2), "92233720368547758.07");
        assert_eq!(decimal(i64::MIN, 3), "-9223372036854775.808");
    }
}

#[utoipa::path(
    get,
    path = "/api/archives/{id}/insights/status",
    params(("id" = String, Path, description = "Archive id")),
    responses(
        (status = 200, body = InsightsStatusDto),
        (status = 401, description = "Not signed in", body = ErrorDto),
        (status = 403, description = "Not allowed", body = ErrorDto),
        (status = 404, description = "No such archive, or the server computes no insights", body = ErrorDto),
        (status = 500, description = "The server failed", body = ErrorDto),
    ),
)]
pub(crate) async fn insights_status(
    State(state): State<Arc<ApiState>>,
    Path(raw): Path<String>,
) -> ApiResult<Json<InsightsStatusDto>> {
    let archive: ArchiveId = raw.parse().map_err(|_| ApiError::not_found("archive"))?;
    state
        .store
        .get_archive(archive)
        .await?
        .ok_or_else(|| ApiError::not_found("archive"))?;
    let config = state
        .insights
        .as_ref()
        .ok_or_else(|| ApiError::not_found("insights"))?;
    Ok(Json(InsightsStatusDto {
        pending: state
            .store
            .count_needing_insights(archive, &config.fingerprint)
            .await?,
        reindex: state.store.count_reindex_queue(archive).await?,
        categorize: match &config.deriver {
            Some(deriver) => {
                let ctx = deriver.context(&*state.store, archive).await?;
                state
                    .store
                    .count_needing_derive(archive, ctx.derive_fp())
                    .await?
            }
            None => 0,
        },
        running: config.progress.running(),
    }))
}
