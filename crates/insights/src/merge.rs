use std::cmp::Reverse;

use holtmail_insight::limits::MAX_ITEMS_PER_EXTRACTOR;
use holtmail_insight::{
    Confidence, Insight, InsightInput, InsightKind, LocalOrZoned, Provenance, Reservation, Url,
    UrlScheme, fits_identifier, truncate_display,
};

pub(crate) const MAX_ITEMS: usize = 32;
pub(crate) const MAX_ITEMS_JSON_BYTES: usize = 16 * 1024;
const EMPTY_ITEMS_JSON_BYTES: usize = r#"{"v":1,"items":[]}"#.len();

pub(crate) fn per_extractor(mut items: Vec<Insight>) -> Vec<Insight> {
    items.truncate(MAX_ITEMS_PER_EXTRACTOR);
    items
}

pub(crate) fn normalise(items: Vec<Insight>) -> Vec<Insight> {
    items
}

pub(crate) fn merge(items: Vec<Insight>) -> Vec<Insight> {
    let mut unique: Vec<Insight> = Vec::with_capacity(items.len());
    for item in items {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    let references: Vec<String> = unique.iter().flat_map(references).collect();
    let structured_actions: Vec<&Url> = unique
        .iter()
        .filter(|item| is_structured(item.provenance))
        .filter_map(|item| match &item.kind {
            InsightKind::Action { url, .. } => Some(url),
            _ => None,
        })
        .collect();
    let keep: Vec<bool> = unique
        .iter()
        .map(|item| match &item.kind {
            InsightKind::Code { value, .. } => !references.contains(&reference_key(value)),
            InsightKind::Action { url, .. } if item.provenance == Provenance::Heuristic => {
                !structured_actions.contains(&url)
            }
            _ => true,
        })
        .collect();
    unique
        .into_iter()
        .zip(keep)
        .filter_map(|(item, keep)| keep.then_some(item))
        .collect()
}

fn references(item: &Insight) -> Vec<String> {
    let values: Vec<&str> = match &item.kind {
        InsightKind::Reservation(reservation) => match reservation {
            Reservation::Flight(r) => r.reservation_number.iter().map(String::as_str).collect(),
            Reservation::Lodging(r) => vec![r.reservation_number.as_str()],
            Reservation::Train(r) => r.reservation_number.iter().map(String::as_str).collect(),
            Reservation::Bus(r) => r.reservation_number.iter().map(String::as_str).collect(),
            Reservation::Event(r) => r.reservation_number.iter().map(String::as_str).collect(),
            Reservation::Food(r) => r.reservation_number.iter().map(String::as_str).collect(),
            Reservation::RentalCar(r) => vec![r.reservation_number.as_str()],
        },
        InsightKind::Order { number, .. } | InsightKind::Invoice { number, .. } => {
            number.iter().map(String::as_str).collect()
        }
        InsightKind::Parcel { number, .. } => vec![number.as_str()],
        _ => Vec::new(),
    };
    values
        .into_iter()
        .map(reference_key)
        .filter(|key| !key.is_empty())
        .collect()
}

fn reference_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_structured(provenance: Provenance) -> bool {
    matches!(
        provenance,
        Provenance::JsonLd | Provenance::Microdata | Provenance::PkPass
    )
}

pub(crate) fn validate(input: &InsightInput, items: Vec<Insight>) -> Vec<Insight> {
    items
        .into_iter()
        .filter_map(|item| valid(input, item))
        .collect()
}

fn valid(input: &InsightInput, mut item: Insight) -> Option<Insight> {
    item.score = if item.confidence == Confidence::Certain {
        1000
    } else {
        item.score.min(1000)
    };
    let identifier = |value: &Option<String>| value.as_deref().is_none_or(fits_identifier);
    let display = |value: &mut Option<String>| {
        if let Some(text) = value.as_mut() {
            *text = truncate_display(text);
        }
    };
    match &mut item.kind {
        InsightKind::Code { value, .. } => {
            if !fits_identifier(value) {
                return None;
            }
        }
        InsightKind::Action {
            url,
            destination,
            label,
            ..
        } => {
            if url.scheme() == UrlScheme::Mailto
                || destination.scheme() == UrlScheme::Mailto
                || !destination.fits_destination()
            {
                return None;
            }
            display(label);
        }
        InsightKind::Unsubscribe {
            https,
            mailto,
            http,
            one_click,
            ..
        } => {
            if https
                .as_ref()
                .is_some_and(|u| u.scheme() != UrlScheme::Https)
                || http.as_ref().is_some_and(|u| u.scheme() != UrlScheme::Http)
                || mailto
                    .as_deref()
                    .is_some_and(|m| Url::parse(m).map(|u| u.scheme()) != Ok(UrlScheme::Mailto))
                || (*one_click && https.is_none())
                || (https.is_none() && http.is_none() && mailto.is_none())
            {
                return None;
            }
        }
        InsightKind::Reservation(reservation) => {
            if !valid_reservation(reservation) {
                return None;
            }
        }
        InsightKind::Order {
            number, merchant, ..
        } => {
            if !identifier(number) || (number.is_none() && merchant.is_none()) {
                return None;
            }
            display(merchant);
        }
        InsightKind::Invoice {
            number,
            payee,
            total,
            due,
            ..
        } => {
            if !identifier(number) || (total.is_none() && due.is_none()) {
                return None;
            }
            display(payee);
        }
        InsightKind::Parcel { number, .. } => {
            if !fits_identifier(number) {
                return None;
            }
        }
        InsightKind::Pass {
            organization,
            description,
            attachment,
            ..
        } => {
            if organization.is_empty()
                || description.is_empty()
                || !input.attachments.iter().any(|a| a.ordinal == *attachment)
            {
                return None;
            }
            *organization = truncate_display(organization);
            *description = truncate_display(description);
        }
    }
    if let holtmail_insight::PartRef::Attachment(ordinal) = item.origin
        && !input.attachments.iter().any(|a| a.ordinal == ordinal)
    {
        return None;
    }
    Some(item)
}

fn valid_reservation(reservation: &mut Reservation) -> bool {
    let identifier = |value: &Option<String>| value.as_deref().is_none_or(fits_identifier);
    let display = |value: &mut String| {
        *value = truncate_display(value);
        !value.is_empty()
    };
    match reservation {
        Reservation::Flight(r) => {
            identifier(&r.reservation_number)
                && identifier(&r.flight_number)
                && (r.reservation_number.is_some() || r.flight_number.is_some())
                && !r.departure_airport.is_empty()
                && !r.arrival_airport.is_empty()
                && ordered(&r.departure, r.arrival.as_ref())
        }
        Reservation::Lodging(r) => {
            fits_identifier(&r.reservation_number)
                && display(&mut r.name)
                && ordered(&r.checkin, r.checkout.as_ref())
        }
        Reservation::Train(r) => {
            identifier(&r.reservation_number)
                && identifier(&r.train_number)
                && (r.reservation_number.is_some() || r.train_number.is_some())
                && display(&mut r.departure_station)
                && ordered(&r.departure, r.arrival.as_ref())
        }
        Reservation::Bus(r) => {
            identifier(&r.reservation_number)
                && identifier(&r.bus_number)
                && (r.reservation_number.is_some() || r.bus_number.is_some())
                && display(&mut r.departure_station)
                && ordered(&r.departure, r.arrival.as_ref())
        }
        Reservation::Event(r) => {
            identifier(&r.reservation_number)
                && display(&mut r.name)
                && ordered(&r.start, r.end.as_ref())
        }
        Reservation::Food(r) => identifier(&r.reservation_number) && display(&mut r.name),
        Reservation::RentalCar(r) => {
            fits_identifier(&r.reservation_number) && ordered(&r.pickup, r.dropoff.as_ref())
        }
    }
}

fn ordered(start: &LocalOrZoned, end: Option<&LocalOrZoned>) -> bool {
    match (start, end) {
        (LocalOrZoned::Local(start), Some(LocalOrZoned::Local(end))) => end >= start,
        (LocalOrZoned::Zoned(start, _), Some(LocalOrZoned::Zoned(end, _))) => end >= start,
        _ => true,
    }
}

pub(crate) fn cap(items: Vec<Insight>) -> (Vec<Insight>, bool) {
    let sizes: Vec<usize> = items
        .iter()
        .map(|item| serde_json::to_string(item).map_or(usize::MAX, |json| json.len()))
        .collect();
    let mut leg = 0usize;
    let mut order: Vec<usize> = (0..items.len()).collect();
    let legs: Vec<usize> = items
        .iter()
        .map(|item| match item.kind {
            InsightKind::Reservation(_) => {
                leg += 1;
                leg
            }
            _ => 0,
        })
        .collect();
    order.sort_by_key(|&i| {
        let item = &items[i];
        (
            Reverse(item.confidence),
            !is_structured(item.provenance),
            item.forwarded,
            legs[i],
            Reverse(item.score),
            i,
        )
    });

    let mut keep = vec![false; items.len()];
    let mut bytes = EMPTY_ITEMS_JSON_BYTES;
    let mut kept = 0;
    for i in order {
        let added = sizes[i].saturating_add(usize::from(kept > 0));
        if kept == MAX_ITEMS || bytes.saturating_add(added) > MAX_ITEMS_JSON_BYTES {
            continue;
        }
        bytes += added;
        kept += 1;
        keep[i] = true;
    }
    let dropped = kept < items.len();
    let items = items
        .into_iter()
        .zip(keep)
        .filter_map(|(item, keep)| keep.then_some(item))
        .collect();
    (items, dropped)
}
