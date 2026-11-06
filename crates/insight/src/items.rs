use serde::Deserialize;

use crate::{Insight, InsightError};

/// The version of the stored items JSON this build writes.
pub const ITEMS_VERSION: u64 = 1;

/// The items of one message, as stored in the `items` column.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Items {
    /// The items, in stored order.
    pub items: Vec<Insight>,
}

/// What [`Items::from_json`] read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemsRead {
    /// The items this build could read.
    pub items: Items,
    /// The `v` of the stored JSON.
    pub version: u64,
    /// Items left out because their type is unknown or they did not deserialize.
    pub skipped: usize,
}

impl ItemsRead {
    /// Whether the row should be recomputed: an item was skipped or the shape is not
    /// [`ITEMS_VERSION`].
    pub fn outdated(&self) -> bool {
        self.skipped > 0 || self.version != ITEMS_VERSION
    }
}

#[derive(Deserialize)]
struct Stored {
    v: u64,
    items: Vec<serde_json::Value>,
}

impl Items {
    /// The stored form, `{"v":1,"items":[...]}`.
    ///
    /// Equal items give equal bytes. An item that cannot be written as JSON is left out rather than
    /// failing the row; the conformance suite checks that extractors never produce one.
    pub fn to_json(&self) -> String {
        let parts: Vec<String> = self
            .items
            .iter()
            .filter_map(|item| serde_json::to_string(item).ok())
            .collect();
        format!("{{\"v\":{ITEMS_VERSION},\"items\":[{}]}}", parts.join(","))
    }

    /// Reads a stored row leniently.
    ///
    /// An item with an unknown type or fields that do not deserialize is skipped and counted, and
    /// the rest are kept in order; it never fails the row for one item. Fails with
    /// [`InsightError::Unparsable`] only when the text is not an object with an integer `v` and an
    /// `items` array.
    pub fn from_json(json: &str) -> Result<ItemsRead, InsightError> {
        let stored: Stored = serde_json::from_str(json).map_err(|_| InsightError::Unparsable)?;
        let total = stored.items.len();
        let items: Vec<Insight> = stored
            .items
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect();
        Ok(ItemsRead {
            skipped: total - items.len(),
            items: Items { items },
            version: stored.v,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, NaiveDate};

    use super::*;
    use crate::{
        BoardingLeg, BoardingPass, CarrierId, Confidence, Currency, Date, FlightReservation,
        InsightKind, LinkAction, LocalOrZoned, Money, PartRef, PassDate, PassStyle, Provenance,
        Reservation, Url,
    };

    fn item(kind: InsightKind) -> Insight {
        Insight {
            kind,
            provenance: Provenance::Heuristic,
            confidence: Confidence::Likely,
            score: 800,
            origin: PartRef::Text,
            forwarded: false,
        }
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn every_kind() -> Items {
        let departure = NaiveDate::from_ymd_opt(2026, 11, 20)
            .unwrap()
            .and_hms_opt(9, 30, 0)
            .unwrap();
        Items {
            items: vec![
                item(InsightKind::Code {
                    value: "482913".into(),
                    origin_bound: Some("example.com".into()),
                }),
                item(InsightKind::Action {
                    action: LinkAction::ResetPassword,
                    url: url("https://example.com/reset?t=abc"),
                    destination: url("https://example.com/reset?t=abc"),
                    via: None,
                    tracked: false,
                    text_host: None,
                    label: Some("Reset password".into()),
                }),
                Insight {
                    provenance: Provenance::Header,
                    confidence: Confidence::Certain,
                    score: 1000,
                    origin: PartRef::Header("list-unsubscribe".into()),
                    ..item(InsightKind::Unsubscribe {
                        https: Some(url("https://news.example.com/u/1")),
                        mailto: Some("mailto:leave@news.example.com".into()),
                        http: None,
                        one_click: true,
                        list_id: Some("news.example.com".into()),
                    })
                },
                Insight {
                    provenance: Provenance::JsonLd,
                    origin: PartRef::Html,
                    forwarded: true,
                    ..item(InsightKind::Reservation(Reservation::Flight(
                        FlightReservation {
                            reservation_number: Some("XK4P2Q".into()),
                            carrier: Some("XA".into()),
                            flight_number: Some("123".into()),
                            departure_airport: "AAA".into(),
                            arrival_airport: "BBB".into(),
                            departure: LocalOrZoned::Local(departure),
                            arrival: Some(LocalOrZoned::Zoned(
                                DateTime::parse_from_rfc3339("2026-11-20T13:05:00+02:00").unwrap(),
                                Some("Europe/Berlin".into()),
                            )),
                            passengers: vec!["Ada Example".into()],
                            seats: vec!["12A".into()],
                            url: None,
                        },
                    )))
                },
                item(InsightKind::Order {
                    number: Some("A-1001".into()),
                    merchant: Some("Example Shop".into()),
                    total: Some(Money {
                        minor: 1999,
                        currency: Currency::new("USD", 2).unwrap(),
                    }),
                    status: None,
                    url: Some(url("https://shop.example.com/orders/1001")),
                }),
                item(InsightKind::Invoice {
                    number: None,
                    payee: Some("Example Power".into()),
                    total: None,
                    due: Some(Date(NaiveDate::from_ymd_opt(2026, 12, 1).unwrap())),
                    url: None,
                }),
                item(InsightKind::Parcel {
                    carrier: CarrierId::new("upu_s10").unwrap(),
                    number: "EE123456785XA".into(),
                    check_digit: true,
                    track_url: None,
                }),
                Insight {
                    origin: PartRef::Attachment(0),
                    provenance: Provenance::PkPass,
                    ..item(InsightKind::Pass {
                        style: PassStyle::BoardingPass,
                        organization: "Example Air".into(),
                        description: "Boarding pass".into(),
                        relevant: None,
                        boarding: Some(BoardingPass {
                            pnr: Some("XK4P2Q".into()),
                            passenger: None,
                            legs: vec![BoardingLeg {
                                from: "AAA".into(),
                                to: "BBB".into(),
                                carrier: Some("XA".into()),
                                flight: Some("123".into()),
                                date: Some(PassDate::MonthDay { month: 11, day: 20 }),
                                seat: None,
                            }],
                        }),
                        attachment: 0,
                        signature_checked: false,
                    })
                },
            ],
        }
    }

    #[test]
    fn every_kind_round_trips() {
        let items = every_kind();
        let json = items.to_json();
        let read = Items::from_json(&json).unwrap();
        assert_eq!(read.items, items);
        assert_eq!(read.skipped, 0);
        assert!(!read.outdated());
        assert_eq!(read.items.to_json(), json);
    }

    #[test]
    fn writes_the_documented_shape() {
        let json = every_kind().to_json();
        assert!(json.starts_with(r#"{"v":1,"items":[{"type":"code","value":"482913""#));
        assert!(json.contains(
            r#""p":"heuristic","c":"likely","score":800,"origin":"text","forwarded":false"#
        ));
        assert!(json.contains(r#"{"type":"reservation","r":{"type":"flight","#));
        assert!(json.contains(r#""origin":{"header":"list-unsubscribe"}"#));
        assert_eq!(Items::default().to_json(), r#"{"v":1,"items":[]}"#);
    }

    #[test]
    fn reads_a_hand_written_v1_row() {
        let row = r#"{"v":1,"items":[
            {"type":"code","value":"482913","origin_bound":null,"p":"heuristic","c":"likely","score":820,"origin":"text","forwarded":false},
            {"type":"unsubscribe","https":"https://news.example.com/u/1","one_click":false,"p":"header","c":"certain","score":1000,"origin":{"header":"list-unsubscribe"},"forwarded":false}
        ]}"#;
        let read = Items::from_json(row).unwrap();
        assert_eq!(read.skipped, 0);
        assert_eq!(read.items.items.len(), 2);
        assert!(matches!(
            &read.items.items[1].kind,
            InsightKind::Unsubscribe {
                mailto: None,
                list_id: None,
                ..
            }
        ));
    }

    #[test]
    fn skips_unknown_and_bad_items() {
        let row = r#"{"v":1,"items":[
            {"type":"hologram","p":"heuristic","c":"likely","score":1,"origin":"text","forwarded":false},
            {"type":"code","value":"482913","p":"heuristic","c":"likely","score":820,"origin":"text","forwarded":false},
            {"type":"action","action":"sign_in","url":"javascript:alert(1)","destination":"https://example.com/","tracked":false,"p":"heuristic","c":"likely","score":700,"origin":"html","forwarded":false},
            {"type":"code","value":"111222","p":"telepathy","c":"likely","score":820,"origin":"text","forwarded":false},
            42
        ]}"#;
        let read = Items::from_json(row).unwrap();
        assert_eq!(read.skipped, 4);
        assert_eq!(read.items.items.len(), 1);
        assert!(read.outdated());
    }

    #[test]
    fn newer_shapes_are_outdated() {
        let read = Items::from_json(r#"{"v":2,"items":[]}"#).unwrap();
        assert!(read.outdated());
    }

    #[test]
    fn broken_rows_fail_without_content() {
        for row in [
            "",
            "[]",
            "{\"items\":[]}",
            "{\"v\":1}",
            "{\"v\":\"1\",\"items\":[]}",
        ] {
            assert_eq!(Items::from_json(row), Err(InsightError::Unparsable));
        }
    }

    #[test]
    fn far_times_round_trip() {
        let far = NaiveDate::from_ymd_opt(12_000, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let items = Items {
            items: vec![item(InsightKind::Pass {
                style: PassStyle::Generic,
                organization: "Example".into(),
                description: "Card".into(),
                relevant: Some(LocalOrZoned::Zoned(far.and_utc().fixed_offset(), None)),
                boarding: None,
                attachment: 0,
                signature_checked: false,
            })],
        };
        let read = Items::from_json(&items.to_json()).unwrap();
        assert_eq!(read.items, items);
        assert_eq!(read.skipped, 0);
    }
}
