#![no_main]

use holtmail_insight::limits::MAX_ITEMS_PER_EXTRACTOR;
use holtmail_insight::{
    HtmlView, InsightExtractor, InsightInput, MicrodataItem, MicrodataProperty, MicrodataValue,
};
use holtmail_insight_schemaorg::SchemaOrgExtractor;
use libfuzzer_sys::fuzz_target;

const TYPES: &[&str] = &[
    "https://schema.org/FlightReservation",
    "http://schema.org/LodgingReservation",
    "https://schema.org/Order",
    "https://schema.org/ParcelDelivery",
    "https://schema.org/Flight",
    "https://schema.org/Thing",
];
const NAMES: &[&str] = &[
    "reservationNumber",
    "reservationFor",
    "departureAirport",
    "arrivalAirport",
    "iataCode",
    "departureTime",
    "checkinTime",
    "name",
    "orderNumber",
    "price",
    "priceCurrency",
    "trackingNumber",
    "carrier",
    "potentialAction",
    "target",
    "underName",
];
const TEXTS: &[&str] = &[
    "LHR",
    "AMS",
    "2027-03-14T09:30:00+01:00",
    "2027-03-14",
    "QX7K2P",
    "12.50",
    "KWD",
    "UPS",
    "https://example.com/a",
    "Example Inn",
];

fn microdata(bytes: &[u8]) -> Vec<MicrodataItem> {
    let mut items = Vec::new();
    for chunk in bytes.chunks(4) {
        let [kind, name, value, flags] = [0, 1, 2, 3].map(|i| chunk.get(i).copied().unwrap_or(0));
        if flags & 1 == 1 || items.is_empty() {
            let ty = TYPES[usize::from(kind) % TYPES.len()];
            let mut item = MicrodataItem::new(vec![ty.to_owned()]);
            item.top_level = flags & 2 == 2;
            item.below_forward = flags & 4 == 4;
            items.push(item);
        }
        let property = MicrodataProperty {
            name: NAMES[usize::from(name) % NAMES.len()].to_owned(),
            value: if flags & 8 == 8 {
                MicrodataValue::Item(usize::from(value))
            } else {
                MicrodataValue::Text(TEXTS[usize::from(value) % TEXTS.len()].to_owned())
            },
        };
        if let Some(item) = items.last_mut() {
            item.properties.push(property);
        }
    }
    items
}

fuzz_target!(|data: &[u8]| {
    let mut parts = data.split(|b| *b == 0);
    let head = parts.next().unwrap_or_default();
    let blocks: Vec<String> = parts
        .by_ref()
        .take(8)
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    let rest: Vec<u8> = parts.flatten().copied().collect();
    let forward_at = head.first().map(|b| usize::from(*b) % 10);
    let input = InsightInput {
        html: Some(
            HtmlView::new()
                .with_ld_json(blocks)
                .with_ld_json_forward_at(forward_at)
                .with_microdata(microdata(&rest)),
        ),
        ..InsightInput::default()
    };
    let extractor = SchemaOrgExtractor::new();
    let first = extractor.extract(&input);
    let second = extractor.extract(&input);
    assert_eq!(first, second, "two runs differ");
    assert!(first.len() <= MAX_ITEMS_PER_EXTRACTOR);
    let _ = extractor.explain(&input);
});
