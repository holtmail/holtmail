use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};
use holtmail_insight::{
    BoardingLeg, BoardingPass, Date, LocalOrZoned, PassDate, PassStyle, fits_identifier,
    truncate_display,
};
use serde_json::{Map, Value};

const MAX_TIME_CHARS: usize = 64;
const MAX_BARCODES_COUNTED: usize = 64;

const STYLES: [(&str, PassStyle); 5] = [
    ("boardingPass", PassStyle::BoardingPass),
    ("eventTicket", PassStyle::EventTicket),
    ("coupon", PassStyle::Coupon),
    ("storeCard", PassStyle::StoreCard),
    ("generic", PassStyle::Generic),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    Json,
    NotObject,
    StyleMissing,
    StyleAmbiguous,
    OrganizationMissing,
    DescriptionMissing,
}

impl Rejection {
    pub(crate) fn rule(self) -> &'static str {
        match self {
            Self::Json => "json_invalid",
            Self::NotObject => "json_not_object",
            Self::StyleMissing => "style_missing",
            Self::StyleAmbiguous => "style_ambiguous",
            Self::OrganizationMissing => "organization_missing",
            Self::DescriptionMissing => "description_missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoardingOutcome {
    NotBoardingPass,
    NotAir,
    NoSemantics,
    AirportsMissing,
    Found,
}

impl BoardingOutcome {
    pub(crate) fn rule(self) -> &'static str {
        match self {
            Self::NotBoardingPass => "boarding:not_boarding_pass",
            Self::NotAir => "boarding:not_air",
            Self::NoSemantics => "boarding:no_semantics",
            Self::AirportsMissing => "boarding:airports_missing",
            Self::Found => "boarding",
        }
    }
}

pub(crate) struct Pass {
    pub style: PassStyle,
    pub organization: String,
    pub description: String,
    pub relevant: Option<LocalOrZoned>,
    pub relevant_present: bool,
    pub boarding: Option<BoardingPass>,
    pub boarding_outcome: BoardingOutcome,
    pub semantics: bool,
    pub barcodes: usize,
}

pub(crate) fn read(bytes: &[u8]) -> Result<Pass, Rejection> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Rejection::Json)?;
    let Value::Object(root) = value else {
        return Err(Rejection::NotObject);
    };

    let mut styles = STYLES.iter().filter_map(|(key, style)| {
        root.get(*key)
            .and_then(Value::as_object)
            .map(|o| (*style, o))
    });
    let (style, style_fields) = styles.next().ok_or(Rejection::StyleMissing)?;
    if styles.next().is_some() {
        return Err(Rejection::StyleAmbiguous);
    }

    let organization =
        display(root.get("organizationName")).ok_or(Rejection::OrganizationMissing)?;
    let description = display(root.get("description")).ok_or(Rejection::DescriptionMissing)?;

    let relevant_value = root.get("relevantDate").and_then(Value::as_str);
    let relevant = relevant_value.and_then(|text| match parse_time(text)? {
        Parsed::Time(time) => Some(time),
        Parsed::Date(_) => None,
    });

    let semantics = root.get("semantics").and_then(Value::as_object);
    let (boarding, boarding_outcome) = if style != PassStyle::BoardingPass {
        (None, BoardingOutcome::NotBoardingPass)
    } else if style_fields.get("transitType").and_then(Value::as_str) != Some("PKTransitTypeAir") {
        (None, BoardingOutcome::NotAir)
    } else if let Some(semantics) = semantics {
        match boarding(semantics) {
            Some(boarding) => (Some(boarding), BoardingOutcome::Found),
            None => (None, BoardingOutcome::AirportsMissing),
        }
    } else {
        (None, BoardingOutcome::NoSemantics)
    };

    let barcodes = root
        .get("barcodes")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .take(MAX_BARCODES_COUNTED)
                .filter(|code| code.get("message").and_then(Value::as_str).is_some())
                .count()
        })
        .unwrap_or(0);

    Ok(Pass {
        style,
        organization,
        description,
        relevant,
        relevant_present: relevant_value.is_some(),
        boarding,
        boarding_outcome,
        semantics: semantics.is_some(),
        barcodes,
    })
}

fn display(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| truncate_display(text))
}

fn boarding(semantics: &Map<String, Value>) -> Option<BoardingPass> {
    let from = airport(semantics.get("departureAirportCode"))?;
    let to = airport(semantics.get("destinationAirportCode"))?;
    let mut carrier = semantics
        .get("airlineCode")
        .and_then(Value::as_str)
        .and_then(airline);
    let flight = match semantics.get("flightNumber") {
        Some(Value::Number(number)) => number.as_u64().map(|n| n.to_string()),
        Some(Value::String(text)) => Some(text.trim().to_owned()),
        _ => None,
    }
    .filter(|flight| flight_shape(flight))
    .or_else(|| {
        let code = semantics.get("flightCode")?.as_str()?;
        let (designator, number) = split_flight_code(code, carrier.as_deref())?;
        if carrier.is_none() {
            carrier = designator;
        }
        Some(number)
    });
    let date = ["currentDepartureDate", "originalDepartureDate"]
        .iter()
        .find_map(|key| {
            let parsed = parse_time(semantics.get(*key)?.as_str()?)?;
            Some(PassDate::Full(Date(match parsed {
                Parsed::Date(date) => date,
                Parsed::Time(LocalOrZoned::Local(time)) => time.date(),
                Parsed::Time(LocalOrZoned::Zoned(time, _)) => time.date_naive(),
            })))
        });
    let seat = semantics
        .get("seats")
        .and_then(Value::as_array)
        .and_then(|seats| seats.first())
        .and_then(seat);
    let pnr = semantics
        .get("confirmationNumber")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|pnr| fits_identifier(pnr) && pnr.chars().all(|c| c.is_alphanumeric() || c == '-'))
        .map(str::to_owned);
    let passenger = semantics
        .get("passengerName")
        .and_then(Value::as_object)
        .and_then(|name| {
            let parts: Vec<&str> = ["givenName", "familyName"]
                .iter()
                .filter_map(|key| name.get(*key)?.as_str())
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect();
            (!parts.is_empty()).then(|| truncate_display(&parts.join(" ")))
        });

    Some(BoardingPass {
        pnr,
        passenger,
        legs: vec![BoardingLeg {
            from,
            to,
            carrier,
            flight,
            date,
            seat,
        }],
    })
}

fn airport(value: Option<&Value>) -> Option<String> {
    let code = value?.as_str()?.trim();
    (code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

fn airline(code: &str) -> Option<String> {
    let code = code.trim();
    (code.len() == 2
        && code.bytes().all(|b| b.is_ascii_alphanumeric())
        && !code.bytes().all(|b| b.is_ascii_digit()))
    .then(|| code.to_ascii_uppercase())
}

fn flight_shape(flight: &str) -> bool {
    let digits = flight.bytes().take_while(u8::is_ascii_digit).count();
    let rest = flight.len() - digits;
    (1..=4).contains(&digits)
        && (rest == 0
            || (rest == 1
                && flight
                    .bytes()
                    .last()
                    .is_some_and(|b| b.is_ascii_alphabetic())))
}

fn split_flight_code(code: &str, carrier: Option<&str>) -> Option<(Option<String>, String)> {
    let compact: String = code
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    if flight_shape(&compact) {
        return Some((None, compact));
    }
    let designator: String = compact.chars().take(2).collect();
    if let Some(carrier) = carrier
        && carrier != designator
    {
        return None;
    }
    let designator = airline(&designator)?;
    let number: String = compact.chars().skip(2).collect();
    flight_shape(&number).then_some((Some(designator), number.to_ascii_uppercase()))
}

fn seat(seat: &Value) -> Option<String> {
    let text = |key: &str| match seat.get(key)? {
        Value::String(text) => Some(text.trim().to_owned()),
        Value::Number(number) => number.as_u64().map(|n| n.to_string()),
        _ => None,
    };
    let number = text("seatNumber").filter(|n| !n.is_empty())?;
    let seat = match text("seatRow").filter(|row| !row.is_empty()) {
        Some(row) if !number.starts_with(&row) => format!("{row}{number}"),
        _ => number,
    };
    let compact: String = seat.chars().filter(|c| !c.is_whitespace()).collect();
    ((1..=8).contains(&compact.len()) && compact.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| compact.to_ascii_uppercase())
}

enum Parsed {
    Time(LocalOrZoned),
    Date(NaiveDate),
}

fn parse_time(text: &str) -> Option<Parsed> {
    let text = text.trim();
    if text.chars().nth(MAX_TIME_CHARS).is_some() {
        return None;
    }
    let zulu = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix('z'))
        .map(|rest| format!("{rest}+00:00"));
    let zoned = zulu.as_deref().unwrap_or(text);
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M%z",
    ] {
        if let Ok(time) = DateTime::<FixedOffset>::parse_from_str(zoned, format) {
            return Some(Parsed::Time(LocalOrZoned::Zoned(time, None)));
        }
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(time) = NaiveDateTime::parse_from_str(text, format) {
            return Some(Parsed::Time(LocalOrZoned::Local(time)));
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .map(Parsed::Date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(text: &str) -> Option<LocalOrZoned> {
        match parse_time(text)? {
            Parsed::Time(time) => Some(time),
            Parsed::Date(_) => None,
        }
    }

    #[test]
    fn times_keep_their_offset() {
        let Some(LocalOrZoned::Zoned(zoned, None)) = time("2026-11-20T08:15+01:00") else {
            panic!("not zoned");
        };
        assert_eq!(zoned.to_rfc3339(), "2026-11-20T08:15:00+01:00");
        assert!(matches!(
            time("2026-11-20T08:15:30Z"),
            Some(LocalOrZoned::Zoned(..))
        ));
        assert!(matches!(
            time("2026-11-20T08:15:30.5-0800"),
            Some(LocalOrZoned::Zoned(..))
        ));
        assert!(matches!(
            time("2026-11-20T08:15"),
            Some(LocalOrZoned::Local(_))
        ));
        assert!(time("2026-11-20").is_none());
        assert!(time("tomorrow").is_none());
        assert!(time(&"9".repeat(200)).is_none());
    }

    #[test]
    fn flight_codes_split_off_the_airline() {
        assert_eq!(
            split_flight_code("XA 123", None),
            Some((Some("XA".into()), "123".into()))
        );
        assert_eq!(
            split_flight_code("xa123", Some("XA")),
            Some((Some("XA".into()), "123".into()))
        );
        assert_eq!(split_flight_code("XB123", Some("XA")), None);
        assert_eq!(split_flight_code("0123", None), Some((None, "0123".into())));
        assert_eq!(split_flight_code("XA12345", None), None);
        assert_eq!(split_flight_code("X", None), None);
        assert_eq!(split_flight_code("Ä1234", None), None);
    }

    #[test]
    fn seats_join_row_and_number() {
        let seat_of = |json: &str| seat(&serde_json::from_str(json).unwrap());
        assert_eq!(
            seat_of(r#"{"seatRow":"12","seatNumber":"A"}"#),
            Some("12A".into())
        );
        assert_eq!(
            seat_of(r#"{"seatRow":"12","seatNumber":"12a"}"#),
            Some("12A".into())
        );
        assert_eq!(seat_of(r#"{"seatNumber":"14C"}"#), Some("14C".into()));
        assert_eq!(seat_of(r#"{"seatRow":"12"}"#), None);
        assert_eq!(seat_of(r#"{"seatNumber":"window please"}"#), None);
    }

    #[test]
    fn airports_and_airlines_are_codes() {
        let s = |text: &str| Value::String(text.into());
        assert_eq!(airport(Some(&s(" fra "))), Some("FRA".into()));
        assert_eq!(airport(Some(&s("FRAN"))), None);
        assert_eq!(airport(Some(&s("F1A"))), None);
        assert_eq!(airline("xa"), Some("XA".into()));
        assert_eq!(airline("12"), None);
        assert_eq!(airline("XAA"), None);
    }

    #[test]
    fn styles_must_be_unique() {
        let both =
            br#"{"organizationName":"Example Air","description":"Pass","generic":{},"coupon":{}}"#;
        assert_eq!(read(both).err(), Some(Rejection::StyleAmbiguous));
        let none = br#"{"organizationName":"Example Air","description":"Pass"}"#;
        assert_eq!(read(none).err(), Some(Rejection::StyleMissing));
        assert_eq!(read(b"[1]").err(), Some(Rejection::NotObject));
    }
}
