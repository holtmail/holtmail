//! The conformance suite and the mapping rules, with built inputs.

use holtmail_core::{Address, Timestamp};
use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{
    Confidence, HeaderField, HtmlView, Insight, InsightExtractor, InsightInput, InsightKind,
    LinkAction, LocalOrZoned, MicrodataItem, MicrodataProperty, MicrodataValue, PartRef,
    Provenance, Reservation,
};
use holtmail_insight_schemaorg::{ID, SchemaOrgExtractor};

fn mail(view: HtmlView) -> InsightInput {
    InsightInput {
        headers: vec![
            HeaderField::new("From", "Example Travel <bookings@example.com>"),
            HeaderField::new("To", "Ada Example <ada@example.org>"),
            HeaderField::new("Subject", "Your booking"),
        ],
        from: vec![Address {
            name: Some("Example Travel".into()),
            email: "bookings@example.com".into(),
        }],
        subject: Some("Your booking".into()),
        sent_at: Some(Timestamp::from_secs(1_793_952_720)),
        html: Some(view),
        ..InsightInput::default()
    }
}

fn ld(blocks: &[&str]) -> InsightInput {
    mail(HtmlView::new().with_ld_json(blocks.iter().map(|b| (*b).to_owned()).collect()))
}

fn extract(input: &InsightInput) -> Vec<Insight> {
    SchemaOrgExtractor::new().extract(input)
}

fn flights(items: &[Insight]) -> Vec<&holtmail_insight::FlightReservation> {
    items
        .iter()
        .filter_map(|item| match &item.kind {
            InsightKind::Reservation(Reservation::Flight(f)) => Some(f),
            _ => None,
        })
        .collect()
}

fn time(at: &LocalOrZoned) -> String {
    match at {
        LocalOrZoned::Local(at) => at.to_string(),
        LocalOrZoned::Zoned(at, zone) => format!("{}{zone:?}", at.to_rfc3339()),
    }
}

const FLIGHT_GRAPH: &str = r##"{"@context":"https://schema.org","@graph":[
  {"@type":"FlightReservation","@id":"#res","reservationNumber":"QX7K2P",
   "underName":{"@type":"Person","name":"Ada Example"},
   "reservationFor":{"@id":"#leg"},"airplaneSeat":"14C",
   "url":"https://example.com/trips/QX7K2P",
   "checkinUrl":"https://example.com/checkin/QX7K2P"},
  {"@type":"Flight","@id":"#leg","flightNumber":"XA 482",
   "airline":{"@type":"Airline","name":"Example Air","iataCode":"xa"},
   "departureAirport":{"@type":"Airport","iataCode":"lhr"},
   "arrivalAirport":{"@type":"Airport","iataCode":"AMS"},
   "departureTime":"2027-03-14T09:30:00+00:00","arrivalTime":"2027-03-14T12:05:00+01:00"}
]}"##;

fn round_trip() -> String {
    let mut nodes = Vec::new();
    for (number, from, to, at) in [
        ("101", "LHR", "AMS", "2027-03-14T09:30:00Z"),
        ("102", "AMS", "LHR", "2027-03-21T18:10:00+01:00"),
    ] {
        for (i, name) in ["Ada Example", "Ben Example", "Cleo Example", "Dan Example"]
            .iter()
            .enumerate()
        {
            nodes.push(format!(
                r#"{{"@type":"FlightReservation","reservationNumber":"RT4PAX","underName":{{"name":"{name}"}},"airplaneSeat":"2{i}A","reservationFor":{{"@type":"Flight","flightNumber":"{number}","airline":{{"iataCode":"XA"}},"departureAirport":{{"iataCode":"{from}"}},"arrivalAirport":{{"iataCode":"{to}"}},"departureTime":"{at}"}}}}"#
            ));
        }
    }
    format!("[{}]", nodes.join(","))
}

fn prop(name: &str, value: MicrodataValue) -> MicrodataProperty {
    MicrodataProperty {
        name: name.into(),
        value,
    }
}

fn lodging_microdata(below_forward: bool) -> InsightInput {
    let mut stay = MicrodataItem::new(vec!["http://schema.org/LodgingReservation".into()]);
    stay.top_level = true;
    stay.below_forward = below_forward;
    stay.properties = vec![
        prop("reservationNumber", MicrodataValue::Text("HX-4417".into())),
        prop("reservationFor", MicrodataValue::Item(1)),
        prop(
            "checkinTime",
            MicrodataValue::Text("2026-12-01T15:00:00+01:00".into()),
        ),
        prop("checkoutTime", MicrodataValue::Text("2026-12-03".into())),
        prop("underName", MicrodataValue::Item(3)),
    ];
    let mut hotel = MicrodataItem::new(vec!["http://schema.org/LodgingBusiness".into()]);
    hotel.properties = vec![
        prop("name", MicrodataValue::Text("Example Harbour Hotel".into())),
        prop("address", MicrodataValue::Item(2)),
    ];
    let mut address = MicrodataItem::new(vec!["http://schema.org/PostalAddress".into()]);
    address.properties = vec![
        prop(
            "streetAddress",
            MicrodataValue::Text("1 Example Quay".into()),
        ),
        prop("addressLocality", MicrodataValue::Text("Exampleton".into())),
    ];
    let mut guest = MicrodataItem::new(vec!["http://schema.org/Person".into()]);
    guest.properties = vec![prop("name", MicrodataValue::Text("Ada Example".into()))];
    mail(HtmlView::new().with_microdata(vec![stay, hotel, address, guest]))
}

const TRAIN: &str = r#"{"@type":"TrainReservation","reservationNumber":"TR-55","reservationFor":{"@type":"TrainTrip","trainNumber":"ICE 72","departureStation":{"name":"Example Hbf"},"arrivalStation":{"name":"Sample Süd"},"departureTime":"2027-04-02T07:12","arrivalTime":"2027-04-02T10:40"},"reservedTicket":{"ticketedSeat":{"seatNumber":"42"}}}"#;
const BUS: &str = r#"{"@type":"BusReservation","reservationNumber":"BUS-9","reservationFor":{"@type":"BusTrip","busNumber":"N12","departureBusStop":{"name":"Example Square"},"arrivalBusStop":"Sample Gate","departureTime":"2027-04-03T22:00:00+02:00"}}"#;
const EVENT: &str = r#"{"@type":"EventReservation","reservationNumber":"EV-1","reservationFor":{"@type":"Event","name":"Example Quartet Live","startDate":"2027-05-01T20:00:00+02:00","endDate":"2027-05-01T22:30:00+02:00","location":{"@type":"Place","name":"Example Hall"}},"reservedTicket":[{"ticketNumber":"T-1"},{"ticketNumber":"T-2"}]}"#;
const FOOD: &str = r#"{"@type":"FoodEstablishmentReservation","reservationNumber":"F-77","reservationFor":{"@type":"Restaurant","name":"Example Bistro"},"startTime":"2027-05-02T19:30:00","partySize":4}"#;
const CAR: &str = r#"{"@type":"RentalCarReservation","reservationNumber":"CAR-31","reservationFor":{"@type":"Car","rentalCompany":{"name":"Example Cars"}},"pickupTime":"2027-06-01T09:00:00+02:00","pickupLocation":{"name":"Example Airport"},"dropoffTime":"2027-06-05T09:00:00+02:00","dropoffLocation":{"address":"2 Sample Road, Sampleton"}}"#;
const ORDER_JPY: &str = r#"{"@type":"Order","orderNumber":"ORD-2041","seller":{"@type":"Organization","name":"Example Shop"},"orderStatus":"http://schema.org/OrderProcessing","price":12800,"priceCurrency":"JPY","url":"https://shop.example.com/orders/2041","potentialAction":{"@type":"ViewAction","name":"View order","target":"https://shop.example.com/orders/2041/view"}}"#;
const INVOICE: &str = r#"{"@type":"Invoice","confirmationNumber":"INV-0091","provider":{"name":"Example Utilities"},"totalPaymentDue":{"@type":"MonetaryAmount","value":"84.20","currency":"EUR"},"paymentDueDate":"2027-01-15","url":"https://billing.example.com/inv/0091"}"#;
const PARCEL: &str = r#"{"@type":"ParcelDelivery","trackingNumber":"1z 4e8 752 a03 8192 654","carrier":{"@type":"Organization","name":"United Parcel Service"},"trackingUrl":"https://store.example.com/track/7731"}"#;
const ACTIONS: &str = r#"[{"@type":"ConfirmAction","name":"Confirm subscription","target":{"@type":"EntryPoint","urlTemplate":"https://news.example.com/confirm/abc"}},{"@type":"TrackAction","url":"https://store.example.com/track/1"},{"@type":"CheckInAction","target":"javascript:alert(1)"},{"@type":"ViewAction","target":"mailto:help@example.com"}]"#;

#[test]
fn flight_graph_maps_every_field() {
    let items = extract(&ld(&[FLIGHT_GRAPH]));
    assert_eq!(items.len(), 2, "{items:?}");
    let flight = flights(&items)[0];
    assert_eq!(flight.reservation_number.as_deref(), Some("QX7K2P"));
    assert_eq!(flight.carrier.as_deref(), Some("XA"));
    assert_eq!(flight.flight_number.as_deref(), Some("482"));
    assert_eq!(flight.departure_airport, "LHR");
    assert_eq!(flight.arrival_airport, "AMS");
    assert_eq!(time(&flight.departure), "2027-03-14T09:30:00+00:00None");
    assert_eq!(
        flight.arrival.as_ref().map(time).as_deref(),
        Some("2027-03-14T12:05:00+01:00None")
    );
    assert_eq!(flight.passengers, ["Ada Example"]);
    assert_eq!(flight.seats, ["14C"]);
    assert_eq!(
        flight.url.as_ref().map(|u| u.as_str()),
        Some("https://example.com/trips/QX7K2P")
    );
    let first = &items[0];
    assert_eq!(first.provenance, Provenance::JsonLd);
    assert_eq!(first.confidence, Confidence::Certain);
    assert_eq!(first.score, 1000);
    assert_eq!(first.origin, PartRef::Html);
    assert!(!first.forwarded);
    let InsightKind::Action {
        action,
        url,
        destination,
        ..
    } = &items[1].kind
    else {
        panic!("not an action");
    };
    assert_eq!(*action, LinkAction::CheckIn);
    assert_eq!(url, destination);
}

#[test]
fn four_passengers_two_legs_give_two_items() {
    let items = extract(&ld(&[&round_trip()]));
    let legs = flights(&items);
    assert_eq!(legs.len(), 2);
    for leg in &legs {
        assert_eq!(leg.passengers.len(), 4);
        assert_eq!(leg.seats, ["20A", "21A", "22A", "23A"]);
    }
    assert_eq!(legs[0].flight_number.as_deref(), Some("101"));
    assert_eq!(legs[1].flight_number.as_deref(), Some("102"));
}

#[test]
fn microdata_lodging_maps() {
    let items = extract(&lodging_microdata(false));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].provenance, Provenance::Microdata);
    let InsightKind::Reservation(Reservation::Lodging(stay)) = &items[0].kind else {
        panic!("not lodging");
    };
    assert_eq!(stay.reservation_number, "HX-4417");
    assert_eq!(stay.name, "Example Harbour Hotel");
    assert_eq!(stay.address.as_deref(), Some("1 Example Quay, Exampleton"));
    assert_eq!(time(&stay.checkin), "2026-12-01T15:00:00+01:00None");
    assert_eq!(
        stay.checkout.as_ref().map(time).as_deref(),
        Some("2026-12-03 00:00:00")
    );
    assert_eq!(stay.guests, ["Ada Example"]);
    assert!(extract(&lodging_microdata(true))[0].forwarded);
}

#[test]
fn every_reservation_type_maps() {
    let items = extract(&ld(&[TRAIN, BUS, EVENT, FOOD, CAR]));
    assert_eq!(items.len(), 5, "{items:?}");
    let InsightKind::Reservation(Reservation::Train(train)) = &items[0].kind else {
        panic!("train");
    };
    assert_eq!(train.train_number.as_deref(), Some("ICE 72"));
    assert_eq!(train.departure_station, "Example Hbf");
    assert_eq!(train.arrival_station.as_deref(), Some("Sample Süd"));
    assert_eq!(time(&train.departure), "2027-04-02 07:12:00");
    assert_eq!(train.seats, ["42"]);
    let InsightKind::Reservation(Reservation::Bus(bus)) = &items[1].kind else {
        panic!("bus");
    };
    assert_eq!(bus.bus_number.as_deref(), Some("N12"));
    assert_eq!(bus.arrival_station.as_deref(), Some("Sample Gate"));
    let InsightKind::Reservation(Reservation::Event(event)) = &items[2].kind else {
        panic!("event");
    };
    assert_eq!(event.name, "Example Quartet Live");
    assert_eq!(event.venue.as_deref(), Some("Example Hall"));
    assert_eq!(event.tickets, ["T-1", "T-2"]);
    let InsightKind::Reservation(Reservation::Food(food)) = &items[3].kind else {
        panic!("food");
    };
    assert_eq!(food.party_size, Some(4));
    assert_eq!(food.name, "Example Bistro");
    let InsightKind::Reservation(Reservation::RentalCar(car)) = &items[4].kind else {
        panic!("car");
    };
    assert_eq!(car.company.as_deref(), Some("Example Cars"));
    assert_eq!(car.pickup_location.as_deref(), Some("Example Airport"));
    assert_eq!(
        car.dropoff_location.as_deref(),
        Some("2 Sample Road, Sampleton")
    );
}

#[test]
fn orders_invoices_parcels_and_actions_map() {
    let items = extract(&ld(&[ORDER_JPY, INVOICE, PARCEL, ACTIONS]));
    let kinds: Vec<String> = items
        .iter()
        .map(|item| match &item.kind {
            InsightKind::Action { action, .. } => format!("{action:?}"),
            other => format!("{:?}", other.tag()),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "Order", "View", "Invoice", "Parcel", "Track", "Confirm", "Track"
        ]
    );
    let InsightKind::Order {
        number,
        merchant,
        total,
        status,
        url,
    } = &items[0].kind
    else {
        panic!("order");
    };
    assert_eq!(number.as_deref(), Some("ORD-2041"));
    assert_eq!(merchant.as_deref(), Some("Example Shop"));
    let total = total.as_ref().unwrap();
    assert_eq!((total.minor, total.currency.code()), (12800, "JPY"));
    assert_eq!(status.as_deref(), Some("OrderProcessing"));
    assert!(url.is_some());
    let InsightKind::Invoice {
        number,
        payee,
        total,
        due,
        ..
    } = &items[2].kind
    else {
        panic!("invoice");
    };
    assert_eq!(number.as_deref(), Some("INV-0091"));
    assert_eq!(payee.as_deref(), Some("Example Utilities"));
    assert_eq!(total.as_ref().map(|t| t.minor), Some(8420));
    assert_eq!(due.map(|d| d.0.to_string()).as_deref(), Some("2027-01-15"));
    let InsightKind::Parcel {
        carrier,
        number,
        check_digit,
        track_url,
    } = &items[3].kind
    else {
        panic!("parcel");
    };
    assert_eq!(carrier.as_str(), "ups");
    assert_eq!(number, "1Z4E8752A038192654");
    assert!(!check_digit);
    assert!(track_url.is_none());
    let InsightKind::Action { label, .. } = &items[5].kind else {
        panic!("confirm");
    };
    assert_eq!(label.as_deref(), Some("Confirm subscription"));
}

#[test]
fn money_is_exact_or_absent() {
    let total = |block: &str| -> Option<(i64, String)> {
        extract(&ld(&[block]))
            .into_iter()
            .find_map(|item| match item.kind {
                InsightKind::Order { total, .. } => {
                    Some(total.map(|t| (t.minor, t.currency.code().to_owned())))
                }
                _ => None,
            })?
    };
    let order = |price: &str, currency: &str| {
        format!(
            r#"{{"@type":"Order","orderNumber":"M-1","priceSpecification":{{"@type":"PriceSpecification","price":{price},"priceCurrency":"{currency}"}}}}"#
        )
    };
    assert_eq!(
        total(&order("\"4.125\"", "KWD")),
        Some((4125, "KWD".into()))
    );
    assert_eq!(
        total(&order("\"19.99\"", "usd")),
        Some((1999, "USD".into()))
    );
    assert_eq!(total(&order("19.5", "EUR")), Some((1950, "EUR".into())));
    assert_eq!(total(&order("1e3", "JPY")), Some((1000, "JPY".into())));
    assert_eq!(total(&order("12800.0", "JPY")), Some((12800, "JPY".into())));
    assert_eq!(total(&order("1e3", "USD")), Some((100000, "USD".into())));
    assert_eq!(total(&order("12.50", "EUR")), Some((1250, "EUR".into())));
    assert_eq!(
        total(&order("1e16", "USD")),
        Some((1_000_000_000_000_000_000, "USD".into()))
    );
    assert_eq!(total(&order("1.5e-7", "USD")), None);
    assert_eq!(total(&order("\"12.5\"", "JPY")), None);
    assert_eq!(total(&order("\"1e3\"", "USD")), None);
    assert_eq!(total(&order("\"1,000.00\"", "USD")), None);
    assert_eq!(total(&order("\"99999999999999999.99\"", "USD")), None);
    assert_eq!(total(&order("\"10.00\"", "ZZZ")), None);
    assert_eq!(extract(&ld(&[&order("\"10.00\"", "ZZZ")])).len(), 1);
}

#[test]
fn hard_negatives_give_nothing() {
    for block in [
        r#"{"@type":"FlightReservation","reservationNumber":"E1","reservationFor":{"flightNumber":"9","departureAirport":{"iataCode":"LHR"},"arrivalAirport":{"iataCode":"AMS"},"departureTime":"2027-03-14T12:00:00Z","arrivalTime":"2027-03-14T11:00:00Z"}}"#,
        r#"{"@type":"FlightReservation","reservationNumber":"E2","reservationFor":{"departureAirport":{"iataCode":"LONDON"},"arrivalAirport":{"iataCode":"AMS"},"departureTime":"2027-03-14T12:00:00Z"}}"#,
        r#"{"@type":"LodgingReservation","reservationFor":{"name":"Example Inn"},"checkinTime":"2027-03-14"}"#,
        r#"{"@type":["Thing","CreativeWork"],"name":"Example"}"#,
        r#"{"@type":"https://example.org/Order","orderNumber":"X-1"}"#,
        r#"{"@type":"Order"}"#,
        r#"{"@type":"Invoice","confirmationNumber":"I-1"}"#,
        r#"{"@type":"ParcelDelivery","trackingNumber":"123","carrier":"Почта"}"#,
        r#"{"@type":"ViewAction","target":"javascript:alert(1)"}"#,
        r#"{"@type":"Order","orderNumber":"OVERLONG-1234567890123456789012345678901234567890123456789012345678901"}"#,
        "not json",
    ] {
        assert!(extract(&ld(&[block])).is_empty(), "{block}");
    }
    assert!(extract(&mail(HtmlView::new())).is_empty());
    assert!(extract(&InsightInput::default()).is_empty());
}

#[test]
fn mixed_type_arrays_keep_known_members() {
    let items = extract(&ld(&[
        r#"{"@type":["Thing","https://schema.org/Order","Invoice"],"orderNumber":"MIX-1"}"#,
    ]));
    assert_eq!(items.len(), 1);
    assert!(matches!(items[0].kind, InsightKind::Order { .. }));
}

#[test]
fn equal_keys_merge_and_json_ld_wins() {
    let mut input = lodging_microdata(false);
    if let Some(html) = &mut input.html {
        html.ld_json = vec![r#"{"@type":"LodgingReservation","reservationNumber":"HX-4417","reservationFor":{"name":"Example Harbour Hotel"},"checkinTime":"2026-12-01T15:00:00+01:00","underName":"Ben Example"}"#.into()];
    }
    let items = extract(&input);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].provenance, Provenance::JsonLd);
    let InsightKind::Reservation(Reservation::Lodging(stay)) = &items[0].kind else {
        panic!("not lodging");
    };
    assert_eq!(stay.guests, ["Ben Example", "Ada Example"]);
    assert!(stay.address.is_some() && stay.checkout.is_some());
}

#[test]
fn at_most_sixteen_items_in_document_order() {
    let orders: Vec<String> = (0..30)
        .map(|i| format!(r#"{{"@type":"Order","orderNumber":"N-{i:02}"}}"#))
        .collect();
    let block = format!("[{}]", orders.join(","));
    let items = extract(&ld(&[&block]));
    assert_eq!(items.len(), 16);
    let InsightKind::Order { number, .. } = &items[15].kind else {
        panic!("order");
    };
    assert_eq!(number.as_deref(), Some("N-15"));
}

#[test]
fn forwarded_blocks_are_marked() {
    let input = mail(
        HtmlView::new()
            .with_ld_json(vec![ORDER_JPY.into(), FLIGHT_GRAPH.into()])
            .with_ld_json_forward_at(Some(1)),
    );
    let items = extract(&input);
    let flags: Vec<bool> = items.iter().map(|i| i.forwarded).collect();
    assert_eq!(flags, [false, false, true, true]);
}

#[test]
fn explanations_carry_no_values() {
    let extractor = SchemaOrgExtractor::new();
    let steps = extractor.explain(&ld(&[FLIGHT_GRAPH, "{", ORDER_JPY]));
    let rules: Vec<&str> = steps.iter().map(|s| s.rule).collect();
    for rule in ["ld_json_unparsable", "root", "unknown_type", "chosen"] {
        assert!(rules.contains(&rule), "{rule} missing in {rules:?}");
    }
    let printed = format!("{steps:?}");
    for value in [
        "QX7K2P",
        "Ada",
        "example.com",
        "482",
        "LHR",
        "ORD-2041",
        "12800",
    ] {
        assert!(!printed.contains(value), "{value} in the explanation");
    }
}

#[test]
fn keeps_the_contract() {
    let extractor = SchemaOrgExtractor::new();
    assert_eq!(extractor.id(), ID);
    assert!(extractor.version().starts_with("1+"));
    let trip = round_trip();
    let positives = [
        ("flight-graph", ld(&[FLIGHT_GRAPH])),
        ("round-trip", ld(&[&trip])),
        ("lodging-microdata", lodging_microdata(false)),
        ("trips-events", ld(&[TRAIN, BUS, EVENT, FOOD, CAR])),
        ("commerce", ld(&[ORDER_JPY, INVOICE, PARCEL, ACTIONS])),
    ];
    let mut cases: Vec<Case> = positives
        .into_iter()
        .map(|(name, input)| Case::new(name, input, Expect::Found))
        .collect();
    cases.push(Case::new(
        "end-before-start",
        ld(&[
            r#"{"@type":"EventReservation","reservationFor":{"name":"Example Night","startDate":"2027-05-01T20:00","endDate":"2027-05-01T19:00"}}"#,
        ]),
        Expect::Nothing,
    ));
    cases.push(Case::new(
        "bad-schemes",
        ld(&[ACTIONS.replace("https://", "file://").as_str()]),
        Expect::Nothing,
    ));
    conformance::extractor(&extractor, &cases);
}
