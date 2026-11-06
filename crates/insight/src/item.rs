use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::limits::{MAX_DISPLAY_CHARS, MAX_IDENTIFIER_CHARS};
use crate::{InsightError, KindTag, Url};

/// One extracted item.
///
/// In stored JSON an item is one object: the fields of its [`InsightKind`] tagged by `type`, plus
/// `p` (provenance), `c` (confidence), `score`, `origin` and `forwarded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Insight {
    /// What was found.
    #[serde(flatten)]
    pub kind: InsightKind,
    /// Where the evidence came from.
    #[serde(rename = "p")]
    pub provenance: Provenance,
    /// How sure the extractor is. Its meaning is part of the extractor's version.
    #[serde(rename = "c")]
    pub confidence: Confidence,
    /// Rank among items of the same kind, `0..=1000`; always `1000` for [`Confidence::Certain`].
    pub score: u16,
    /// The part of the message it was found in.
    pub origin: PartRef,
    /// Found below an inline-forward marker. Such items never set the message's kind or topics.
    pub forwarded: bool,
}

/// What an item is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InsightKind {
    /// A one-time code to copy.
    Code {
        /// The code as the user types it, ASCII where the message used fullwidth digits.
        value: String,
        /// The origin of a WICG origin-bound code (`@example.com #123456`).
        origin_bound: Option<String>,
    },
    /// A link the reader can act on.
    Action {
        /// What the link does.
        action: LinkAction,
        /// The link as found in the message.
        url: Url,
        /// Where it really leads after unwrapping, at most
        /// [`MAX_DESTINATION_BYTES`](crate::limits::MAX_DESTINATION_BYTES); equal to `url` when
        /// nothing was unwrapped.
        destination: Url,
        /// The unwrapping service, such as `outlook.com`.
        via: Option<String>,
        /// A known opaque tracker remains in `destination`.
        tracked: bool,
        /// The anchor text, kept only when it reads as a host, as an A-label.
        text_host: Option<String>,
        /// The anchor's label, display text.
        label: Option<String>,
    },
    /// The RFC 2369 unsubscribe options of a list message.
    Unsubscribe {
        /// The first `https` link.
        https: Option<Url>,
        /// The first `mailto` link, as a full `mailto:` URI.
        mailto: Option<String>,
        /// The first `http` link.
        http: Option<Url>,
        /// RFC 8058 one-click is declared, with an `https` link present.
        one_click: bool,
        /// The `List-Id` content, when present.
        list_id: Option<String>,
    },
    /// A reservation.
    Reservation(#[serde(with = "nested_reservation")] Reservation),
    /// An order.
    Order {
        /// The order number, an identifier.
        number: Option<String>,
        /// The seller, display text.
        merchant: Option<String>,
        /// The order total.
        total: Option<Money>,
        /// The order status as the sender names it, display text.
        status: Option<String>,
        /// The order's page.
        url: Option<Url>,
    },
    /// An invoice.
    Invoice {
        /// The invoice number, an identifier.
        number: Option<String>,
        /// Who is to be paid, display text.
        payee: Option<String>,
        /// The amount due.
        total: Option<Money>,
        /// The due date.
        due: Option<Date>,
        /// The invoice's page.
        url: Option<Url>,
    },
    /// A parcel tracking number.
    Parcel {
        /// The carrier.
        carrier: CarrierId,
        /// The tracking number, an identifier, normalised as the carrier prints it.
        number: String,
        /// The carrier's check digit was verified.
        check_digit: bool,
        /// The tracking page, built from the carrier's template, never from the message.
        track_url: Option<Url>,
    },
    /// A wallet pass attachment.
    Pass {
        /// The pass style.
        style: PassStyle,
        /// The issuer, display text.
        organization: String,
        /// What the pass is, display text.
        description: String,
        /// When the pass is relevant.
        relevant: Option<LocalOrZoned>,
        /// The boarding details of a boarding pass.
        boarding: Option<BoardingPass>,
        /// The attachment's ordinal.
        attachment: u16,
        /// The pass signature was verified. Always `false` for now.
        signature_checked: bool,
    },
}

impl InsightKind {
    /// The persisted tag of this kind.
    pub fn tag(&self) -> KindTag {
        match self {
            Self::Code { .. } => KindTag::Code,
            Self::Action { .. } => KindTag::Link,
            Self::Unsubscribe { .. } => KindTag::Unsubscribe,
            Self::Reservation(_) => KindTag::Reservation,
            Self::Order { .. } => KindTag::Order,
            Self::Invoice { .. } => KindTag::Invoice,
            Self::Parcel { .. } => KindTag::Parcel,
            Self::Pass { .. } => KindTag::Pass,
        }
    }
}

mod nested_reservation {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Reservation;

    #[derive(Serialize)]
    struct Out<'a> {
        r: &'a Reservation,
    }

    #[derive(Deserialize)]
    struct In {
        r: Reservation,
    }

    pub fn serialize<S: Serializer>(value: &Reservation, serializer: S) -> Result<S::Ok, S::Error> {
        Out { r: value }.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Reservation, D::Error> {
        In::deserialize(deserializer).map(|wrapped| wrapped.r)
    }
}

/// Where an item's evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// A header field, such as `List-Unsubscribe`.
    Header,
    /// schema.org JSON-LD.
    JsonLd,
    /// schema.org microdata.
    Microdata,
    /// A `.pkpass` attachment.
    PkPass,
    /// A WICG origin-bound code line.
    OriginBound,
    /// Text or link heuristics.
    Heuristic,
}

/// How sure an extractor is, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Shown only when the operator opts in.
    Possible,
    /// Shown on the card and counted for kinds and search.
    Likely,
    /// Structurally certain: a header, validated markup or an origin-bound code.
    Certain,
}

/// The part of the message an item was found in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartRef {
    /// A header field, by name in lowercase.
    Header(String),
    /// The subject.
    Subject,
    /// The plain text body.
    Text,
    /// The HTML body.
    Html,
    /// The attachment with this ordinal.
    Attachment(u16),
}

/// What an action link does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkAction {
    /// Verify an address or a device.
    Verify,
    /// Confirm a request or a subscription.
    Confirm,
    /// Activate an account.
    Activate,
    /// Reset a password.
    ResetPassword,
    /// Sign in.
    SignIn,
    /// Sign in through a one-time link.
    MagicLink,
    /// View an order.
    ViewOrder,
    /// Track a parcel.
    Track,
    /// Check in for a trip.
    CheckIn,
    /// Save a pass to a wallet.
    WalletSave,
    /// View something else the sender points to (schema.org `ViewAction`).
    View,
}

/// A reservation, one of the seven schema.org reservation types.
///
/// In stored JSON it nests under the item as `{"type":"reservation","r":{"type":"flight",...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reservation {
    /// A flight, one leg per item.
    Flight(FlightReservation),
    /// A hotel or other lodging.
    Lodging(LodgingReservation),
    /// A train trip.
    Train(TrainReservation),
    /// A bus trip.
    Bus(BusReservation),
    /// An event ticket.
    Event(EventReservation),
    /// A restaurant table.
    Food(FoodReservation),
    /// A rental car.
    RentalCar(RentalCarReservation),
}

/// One flight leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlightReservation {
    /// The booking reference, an identifier.
    pub reservation_number: Option<String>,
    /// The airline's IATA code.
    pub carrier: Option<String>,
    /// The flight number without the airline code.
    pub flight_number: Option<String>,
    /// The departure airport's IATA code.
    pub departure_airport: String,
    /// The arrival airport's IATA code.
    pub arrival_airport: String,
    /// Departure time.
    pub departure: LocalOrZoned,
    /// Arrival time.
    pub arrival: Option<LocalOrZoned>,
    /// Passenger names, display text, at most 9.
    pub passengers: Vec<String>,
    /// Seats, at most 9.
    pub seats: Vec<String>,
    /// The booking's page.
    pub url: Option<Url>,
}

/// A lodging stay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LodgingReservation {
    /// The booking reference, an identifier.
    pub reservation_number: String,
    /// The lodging's name, display text.
    pub name: String,
    /// The lodging's address, display text.
    pub address: Option<String>,
    /// Check-in time.
    pub checkin: LocalOrZoned,
    /// Check-out time.
    pub checkout: Option<LocalOrZoned>,
    /// Guest names, display text.
    pub guests: Vec<String>,
    /// The booking's page.
    pub url: Option<Url>,
}

/// A train trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainReservation {
    /// The booking reference, an identifier.
    pub reservation_number: Option<String>,
    /// The train number.
    pub train_number: Option<String>,
    /// The departure station, display text.
    pub departure_station: String,
    /// The arrival station, display text.
    pub arrival_station: Option<String>,
    /// Departure time.
    pub departure: LocalOrZoned,
    /// Arrival time.
    pub arrival: Option<LocalOrZoned>,
    /// Passenger names, display text, at most 9.
    pub passengers: Vec<String>,
    /// Seats, at most 9.
    pub seats: Vec<String>,
    /// The booking's page.
    pub url: Option<Url>,
}

/// A bus trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusReservation {
    /// The booking reference, an identifier.
    pub reservation_number: Option<String>,
    /// The bus number.
    pub bus_number: Option<String>,
    /// The departure stop, display text.
    pub departure_station: String,
    /// The arrival stop, display text.
    pub arrival_station: Option<String>,
    /// Departure time.
    pub departure: LocalOrZoned,
    /// Arrival time.
    pub arrival: Option<LocalOrZoned>,
    /// Passenger names, display text, at most 9.
    pub passengers: Vec<String>,
    /// Seats, at most 9.
    pub seats: Vec<String>,
    /// The booking's page.
    pub url: Option<Url>,
}

/// An event ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventReservation {
    /// The booking reference, an identifier.
    pub reservation_number: Option<String>,
    /// The event's name, display text.
    pub name: String,
    /// Start time.
    pub start: LocalOrZoned,
    /// End time.
    pub end: Option<LocalOrZoned>,
    /// The venue, display text.
    pub venue: Option<String>,
    /// Ticket numbers or seats.
    pub tickets: Vec<String>,
    /// The booking's page.
    pub url: Option<Url>,
}

/// A restaurant table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoodReservation {
    /// The booking reference, an identifier.
    pub reservation_number: Option<String>,
    /// The restaurant's name, display text.
    pub name: String,
    /// The reserved time.
    pub start: LocalOrZoned,
    /// Party size.
    pub party_size: Option<u16>,
    /// The booking's page.
    pub url: Option<Url>,
}

/// A rental car.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RentalCarReservation {
    /// The booking reference, an identifier.
    pub reservation_number: String,
    /// The rental company, display text.
    pub company: Option<String>,
    /// Pickup time.
    pub pickup: LocalOrZoned,
    /// Pickup location, display text.
    pub pickup_location: Option<String>,
    /// Drop-off time.
    pub dropoff: Option<LocalOrZoned>,
    /// Drop-off location, display text.
    pub dropoff_location: Option<String>,
    /// The booking's page.
    pub url: Option<Url>,
}

/// An amount of money in a currency's minor unit. Never a float.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    /// The amount in minor units: cents for USD, yen for JPY, fils for KWD.
    pub minor: i64,
    /// The currency.
    pub currency: Currency,
}

/// An ISO 4217 currency with its minor-unit exponent: JPY 0, USD 2, KWD 3.
///
/// The code is three ASCII uppercase letters and the exponent at most 4. Which codes exist is the
/// extractor's data, not the contract's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "CurrencyWire")]
pub struct Currency {
    code: String,
    exponent: u8,
}

#[derive(Deserialize)]
struct CurrencyWire {
    code: String,
    exponent: u8,
}

impl TryFrom<CurrencyWire> for Currency {
    type Error = InsightError;

    fn try_from(wire: CurrencyWire) -> Result<Self, Self::Error> {
        Self::new(&wire.code, wire.exponent)
    }
}

impl Currency {
    /// A currency, or an error when `code` is not three ASCII uppercase letters or `exponent`
    /// exceeds 4.
    pub fn new(code: &str, exponent: u8) -> Result<Self, InsightError> {
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(InsightError::Decode("currency code"));
        }
        if exponent > 4 {
            return Err(InsightError::Decode("currency exponent"));
        }
        Ok(Self {
            code: code.to_owned(),
            exponent,
        })
    }

    /// The ISO 4217 code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Digits after the decimal point in the minor unit.
    pub fn exponent(&self) -> u8 {
        self.exponent
    }
}

/// A calendar date without a time. Serializes as `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Date(pub NaiveDate);

/// A time as the message wrote it.
///
/// A time without an offset stays [`Local`](Self::Local) and is never shifted into a zone.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalOrZoned {
    /// Wall-clock time with no offset.
    Local(NaiveDateTime),
    /// A time with its offset, and the IANA zone name when the message gave one.
    Zoned(DateTime<FixedOffset>, Option<String>),
}

/// A parcel carrier, by its id in the carrier table (`ups`, `upu_s10`, ...).
///
/// The id is 1–32 chars of ASCII lowercase letters, digits and `_`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CarrierId(String);

impl CarrierId {
    /// A carrier id, or an error when `id` breaks the rule above.
    pub fn new(id: &str) -> Result<Self, InsightError> {
        let valid = (1..=32).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if valid {
            Ok(Self(id.to_owned()))
        } else {
            Err(InsightError::Decode("carrier id"))
        }
    }

    /// The id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CarrierId {
    type Error = InsightError;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        Self::new(&id)
    }
}

impl From<CarrierId> for String {
    fn from(id: CarrierId) -> Self {
        id.0
    }
}

/// The style of a wallet pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassStyle {
    /// A boarding pass.
    BoardingPass,
    /// An event ticket.
    EventTicket,
    /// A coupon.
    Coupon,
    /// A store or loyalty card.
    StoreCard,
    /// Anything else.
    Generic,
}

/// Boarding details of a pass. Plain data: nothing in the contract parses it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoardingPass {
    /// The booking reference, an identifier.
    pub pnr: Option<String>,
    /// The passenger's name, display text.
    pub passenger: Option<String>,
    /// The legs, in travel order.
    pub legs: Vec<BoardingLeg>,
}

/// One leg of a boarding pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoardingLeg {
    /// Departure airport, IATA code.
    pub from: String,
    /// Arrival airport, IATA code.
    pub to: String,
    /// The operating airline's IATA code.
    pub carrier: Option<String>,
    /// The flight number without the airline code.
    pub flight: Option<String>,
    /// The flight date.
    pub date: Option<PassDate>,
    /// The seat.
    pub seat: Option<String>,
}

/// A flight date on a pass, which may lack the year.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassDate {
    /// A full date.
    Full(Date),
    /// Day and month only, when no year could be resolved.
    MonthDay {
        /// Month, 1–12.
        month: u8,
        /// Day, 1–31.
        day: u8,
    },
}

/// Whether `value` may be an identifier: 1 to [`MAX_IDENTIFIER_CHARS`] chars. A longer value is
/// rejected as a whole, never truncated.
pub fn fits_identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().nth(MAX_IDENTIFIER_CHARS).is_none()
}

/// `text` cut to at most [`MAX_DISPLAY_CHARS`] chars on a char boundary, ending in `…` when cut.
pub fn truncate_display(text: &str) -> String {
    match text.char_indices().nth(MAX_DISPLAY_CHARS) {
        None => text.to_owned(),
        Some(_) => {
            let mut out: String = text.chars().take(MAX_DISPLAY_CHARS - 1).collect();
            out.push('…');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_bounded_not_cut() {
        assert!(fits_identifier("482913"));
        assert!(fits_identifier(&"9".repeat(MAX_IDENTIFIER_CHARS)));
        assert!(!fits_identifier(&"9".repeat(MAX_IDENTIFIER_CHARS + 1)));
        assert!(fits_identifier(&"验".repeat(MAX_IDENTIFIER_CHARS)));
        assert!(!fits_identifier(""));
    }

    #[test]
    fn display_text_is_cut_on_char_boundaries() {
        assert_eq!(truncate_display("Example Air"), "Example Air");
        let exact = "é".repeat(MAX_DISPLAY_CHARS);
        assert_eq!(truncate_display(&exact), exact);
        let cut = truncate_display(&"😀".repeat(MAX_DISPLAY_CHARS + 10));
        assert_eq!(cut.chars().count(), MAX_DISPLAY_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn currencies_and_carriers_are_checked() {
        assert_eq!(Currency::new("JPY", 0).unwrap().exponent(), 0);
        assert!(Currency::new("usd", 2).is_err());
        assert!(Currency::new("USDX", 2).is_err());
        assert!(Currency::new("KWD", 5).is_err());
        assert!(serde_json::from_str::<Currency>(r#"{"code":"EU","exponent":2}"#).is_err());
        assert_eq!(CarrierId::new("upu_s10").unwrap().as_str(), "upu_s10");
        assert!(CarrierId::new("UPS").is_err());
        assert!(serde_json::from_str::<CarrierId>("\"\"").is_err());
    }

    #[test]
    fn kinds_map_to_their_tags() {
        let code = InsightKind::Code {
            value: "482913".into(),
            origin_bound: None,
        };
        assert_eq!(code.tag(), KindTag::Code);
    }

    #[test]
    fn confidence_orders_weakest_first() {
        assert!(Confidence::Possible < Confidence::Likely);
        assert!(Confidence::Likely < Confidence::Certain);
    }
}
