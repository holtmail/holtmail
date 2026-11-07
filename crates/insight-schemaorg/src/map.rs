use holtmail_insight::limits::MAX_ITEMS_PER_EXTRACTOR;
use holtmail_insight::{
    BusReservation, Confidence, EventReservation, Explanation, FlightReservation, FoodReservation,
    Insight, InsightInput, InsightKind, LinkAction, LocalOrZoned, LodgingReservation, Money,
    PartRef, Provenance, RentalCarReservation, Reservation, TrainReservation, Url, UrlScheme,
    fits_identifier, truncate_display,
};

use crate::data::Data;
use crate::graph::{Graph, Root, Ty, Val, local_name};
use crate::money::parse_decimal;
use crate::time::{ends_before, local_date, parse_date, parse_time};

/// People, seats and tickets kept on one item.
pub(crate) const MAX_LIST: usize = 9;

/// Explanation steps, collected only for `explain`.
pub(crate) struct Log(Option<Vec<Explanation>>);

impl Log {
    pub(crate) fn off() -> Self {
        Self(None)
    }

    pub(crate) fn on() -> Self {
        Self(Some(Vec::new()))
    }

    fn step(&mut self, make: impl FnOnce() -> Explanation) {
        if let Some(steps) = &mut self.0 {
            steps.push(make().part(PartRef::Html));
        }
    }

    pub(crate) fn into_steps(self) -> Vec<Explanation> {
        self.0.unwrap_or_default()
    }
}

struct Draft {
    kind: InsightKind,
    key: Vec<String>,
    provenance: Provenance,
    forwarded: bool,
}

struct Mapper<'a> {
    graph: &'a Graph,
    data: &'a Data,
    log: &'a mut Log,
    drafts: Vec<Draft>,
    root: Root,
    at: usize,
}

/// The items of `input`'s JSON-LD and microdata, merged by key, in document order.
pub(crate) fn extract(data: &Data, input: &InsightInput, log: &mut Log) -> Vec<Insight> {
    let Some(html) = &input.html else {
        return Vec::new();
    };
    let graph = Graph::build(&html.ld_json, html.ld_json_forward_at, &html.microdata);
    let cuts = graph.cuts;
    if cuts.unparsable > 0 {
        log.step(|| Explanation::new("ld_json_unparsable").count(cuts.unparsable));
    }
    if cuts.depth > 0 {
        log.step(|| Explanation::new("depth_cut").count(cuts.depth));
    }
    if cuts.references > 0 {
        log.step(|| Explanation::new("reference_cut").count(cuts.references));
    }
    if cuts.budget {
        log.step(|| Explanation::new("node_budget").count(crate::graph::MAX_NODES));
    }
    let mut mapper = Mapper {
        graph: &graph,
        data,
        log,
        drafts: Vec::new(),
        root: Root {
            node: 0,
            provenance: Provenance::JsonLd,
            forwarded: false,
            block: 0,
        },
        at: 0,
    };
    for root in &graph.roots {
        mapper.root = *root;
        mapper.at = root.block;
        mapper.map_root(root.node);
    }
    let drafts = mapper.drafts;
    let total = drafts.len();
    let items: Vec<Insight> = drafts
        .into_iter()
        .take(MAX_ITEMS_PER_EXTRACTOR)
        .map(|draft| Insight {
            kind: draft.kind,
            provenance: draft.provenance,
            confidence: Confidence::Certain,
            score: 1000,
            origin: PartRef::Html,
            forwarded: draft.forwarded,
        })
        .collect();
    if total > items.len() {
        log.step(|| Explanation::new("item_cap").count(total));
    }
    for index in 0..items.len() {
        log.step(|| Explanation::new("chosen").at(index).score(1000));
    }
    items
}

impl<'a> Mapper<'a> {
    fn note(&mut self, rule: &'static str, ty: Ty) {
        let at = self.at;
        self.log
            .step(|| Explanation::new(rule).at(at).keyword(ty.key()));
    }

    fn map_root(&mut self, node: usize) {
        match self.graph.node(node).and_then(|n| n.types.first()).copied() {
            None => {
                let at = self.at;
                self.log.step(|| Explanation::new("unknown_type").at(at));
            }
            Some(ty) => self.map_typed(node, ty),
        }
        let actions: Vec<usize> = self
            .graph
            .values(node, "potentialAction")
            .filter_map(|v| match v {
                Val::Node(n) => Some(*n),
                Val::Text(_) => None,
            })
            .collect();
        for action in actions {
            if let Some(&ty) = self.graph.node(action).and_then(|n| n.types.first())
                && matches!(ty, Ty::Confirm | Ty::View | Ty::Track | Ty::CheckIn)
            {
                self.action(action, ty);
            }
        }
    }

    fn map_typed(&mut self, node: usize, ty: Ty) {
        self.note("root", ty);
        match ty {
            Ty::Flight => self.flight(node),
            Ty::Lodging => self.lodging(node),
            Ty::Train | Ty::Bus => self.trip(node, ty),
            Ty::Event => self.event(node),
            Ty::Food => self.food(node),
            Ty::RentalCar => self.rental_car(node),
            Ty::Order => self.order(node),
            Ty::Invoice => self.invoice(node),
            Ty::Parcel => self.parcel(node),
            Ty::Confirm | Ty::View | Ty::Track | Ty::CheckIn => self.action(node, ty),
        }
    }

    fn push(&mut self, kind: InsightKind, key: Vec<String>) {
        let provenance = self.root.provenance;
        let forwarded = self.root.forwarded;
        if let Some(existing) = self.drafts.iter_mut().find(|d| d.key == key) {
            merge(&mut existing.kind, kind);
            if provenance == Provenance::JsonLd {
                existing.provenance = Provenance::JsonLd;
            }
            existing.forwarded &= forwarded;
            let at = self.at;
            self.log.step(|| Explanation::new("merged").at(at));
            return;
        }
        self.drafts.push(Draft {
            kind,
            key,
            provenance,
            forwarded,
        });
    }

    fn missing(&mut self, ty: Ty) {
        self.note("missing_required", ty);
    }

    fn ordered(&mut self, ty: Ty, start: &LocalOrZoned, end: Option<&LocalOrZoned>) -> bool {
        if end.is_some_and(|end| ends_before(start, end)) {
            self.note("end_before_start", ty);
            return false;
        }
        true
    }

    fn text(&self, node: usize, name: &str) -> Option<&'a str> {
        self.graph.text(node, name)
    }

    fn ident(&self, node: usize, name: &str) -> Option<String> {
        self.text(node, name)
            .filter(|t| fits_identifier(t))
            .map(str::to_owned)
    }

    fn time(&self, node: usize, names: &[&str]) -> Option<LocalOrZoned> {
        names.iter().find_map(|name| {
            self.graph.values(node, name).find_map(|v| match v {
                Val::Text(text) => parse_time(text),
                Val::Node(_) => None,
            })
        })
    }

    fn url(&mut self, node: usize, names: &[&str]) -> Option<Url> {
        for name in names {
            if let Some(text) = self.text(node, name) {
                match Url::parse(text) {
                    Ok(url) => return Some(url),
                    Err(_) => {
                        let at = self.at;
                        self.log.step(|| Explanation::new("url_refused").at(at));
                    }
                }
            }
        }
        None
    }

    fn people(&self, node: usize) -> Vec<String> {
        let mut out = Vec::new();
        for value in self.graph.values(node, "underName") {
            let name = match value {
                Val::Text(text) => display(text),
                Val::Node(person) => self.person(*person),
            };
            add(&mut out, name);
        }
        out
    }

    fn person(&self, node: usize) -> Option<String> {
        if let Some(name) = self.text(node, "name") {
            return display(name);
        }
        let parts: Vec<&str> = ["givenName", "familyName"]
            .iter()
            .filter_map(|name| self.text(node, name))
            .collect();
        display(&parts.join(" "))
    }

    fn tickets(&self, node: usize, numbers: bool) -> Vec<String> {
        let mut out = Vec::new();
        for value in self.graph.values(node, "reservedTicket") {
            let Val::Node(ticket) = value else {
                continue;
            };
            let seat = self.graph.text_at(*ticket, &["ticketedSeat", "seatNumber"]);
            let value = if numbers {
                self.text(*ticket, "ticketNumber").or(seat)
            } else {
                seat
            };
            add(
                &mut out,
                value.filter(|v| fits_identifier(v)).map(str::to_owned),
            );
        }
        out
    }

    fn place(&self, node: usize, name: &str) -> Option<String> {
        if let Some(name) = self.graph.name_of(node, name) {
            return display(name);
        }
        self.graph
            .child(node, name)
            .and_then(|place| self.address(place))
    }

    fn address(&self, node: usize) -> Option<String> {
        for value in self.graph.values(node, "address") {
            match value {
                Val::Text(text) => {
                    if let Some(text) = display(text) {
                        return Some(text);
                    }
                }
                Val::Node(address) => {
                    let parts: Vec<&str> = [
                        "streetAddress",
                        "addressLocality",
                        "addressRegion",
                        "postalCode",
                    ]
                    .iter()
                    .filter_map(|name| self.text(*address, name))
                    .chain(self.graph.name_of(*address, "addressCountry"))
                    .collect();
                    if let Some(text) = display(&parts.join(", ")) {
                        return Some(text);
                    }
                }
            }
        }
        None
    }

    fn money(&mut self, node: usize) -> Option<Money> {
        let graph = self.graph;
        for name in [
            "totalPrice",
            "totalPaymentDue",
            "price",
            "priceSpecification",
        ] {
            let Some(value) = graph.values(node, name).next() else {
                continue;
            };
            let (amount, holder) = match value {
                Val::Text(text) => (Some(text.as_str()), node),
                Val::Node(spec) => (
                    self.text(*spec, "price")
                        .or_else(|| self.text(*spec, "value")),
                    *spec,
                ),
            };
            let currency = self
                .text(holder, "priceCurrency")
                .or_else(|| self.text(holder, "currency"))
                .and_then(|code| self.data.currency(code));
            let money = amount.zip(currency).and_then(|(amount, currency)| {
                parse_decimal(amount, currency.exponent()).map(|minor| Money { minor, currency })
            });
            if money.is_none() {
                let at = self.at;
                self.log.step(|| Explanation::new("money_dropped").at(at));
            }
            return money;
        }
        None
    }

    fn flight(&mut self, node: usize) {
        let graph = self.graph;
        let flight = graph.child(node, "reservationFor");
        let field = |name: &str| flight.and_then(|f| graph.text(f, name));
        let carrier = flight
            .and_then(|f| graph.text_at(f, &["airline", "iataCode"]))
            .and_then(airline);
        let flight_number = field("flightNumber").and_then(|number| {
            let upper = number.to_ascii_uppercase();
            let bare = carrier
                .as_deref()
                .and_then(|c| upper.strip_prefix(c))
                .map(|rest| rest.trim_start_matches([' ', '-']))
                .filter(|rest| !rest.is_empty())
                .unwrap_or(number.trim());
            fits_identifier(bare).then(|| bare.to_owned())
        });
        let reservation_number = self.ident(node, "reservationNumber");
        let airport = |name: &str| {
            flight
                .and_then(|f| graph.text_at(f, &[name, "iataCode"]))
                .and_then(iata)
        };
        let departure = flight.and_then(|f| self.time(f, &["departureTime"]));
        let (Some(departure_airport), Some(arrival_airport), Some(departure)) = (
            airport("departureAirport"),
            airport("arrivalAirport"),
            departure,
        ) else {
            return self.missing(Ty::Flight);
        };
        if reservation_number.is_none() && flight_number.is_none() {
            return self.missing(Ty::Flight);
        }
        let arrival = flight.and_then(|f| self.time(f, &["arrivalTime"]));
        if !self.ordered(Ty::Flight, &departure, arrival.as_ref()) {
            return;
        }
        let mut seats = Vec::new();
        for value in graph.values(node, "airplaneSeat") {
            if let Val::Text(seat) = value {
                add(
                    &mut seats,
                    Some(seat.trim())
                        .filter(|s| fits_identifier(s))
                        .map(str::to_owned),
                );
            }
        }
        for seat in self.tickets(node, false) {
            add(&mut seats, Some(seat));
        }
        let date = local_date(&departure).to_string();
        let key = match &flight_number {
            Some(number) => vec![
                "flight".into(),
                carrier.clone().unwrap_or_default(),
                number.clone(),
                date,
            ],
            None => vec![
                "flight#".into(),
                reservation_number.clone().unwrap_or_default(),
                departure_airport.clone(),
                date,
            ],
        };
        let url = self.url(node, &["url", "modifyReservationUrl"]);
        let checkin = self.url(node, &["checkinUrl"]);
        let reservation = FlightReservation {
            reservation_number,
            carrier,
            flight_number,
            departure_airport,
            arrival_airport,
            departure,
            arrival,
            passengers: self.people(node),
            seats,
            url,
        };
        self.push(
            InsightKind::Reservation(Reservation::Flight(reservation)),
            key,
        );
        if let Some(url) = checkin {
            self.push_action(LinkAction::CheckIn, url, None);
        }
    }

    fn lodging(&mut self, node: usize) {
        let place = self.graph.child(node, "reservationFor");
        let name = place.and_then(|p| self.text(p, "name")).and_then(display);
        let checkin = self.time(node, &["checkinTime", "checkinDate"]);
        let (Some(reservation_number), Some(name), Some(checkin)) =
            (self.ident(node, "reservationNumber"), name, checkin)
        else {
            return self.missing(Ty::Lodging);
        };
        let checkout = self.time(node, &["checkoutTime", "checkoutDate"]);
        if !self.ordered(Ty::Lodging, &checkin, checkout.as_ref()) {
            return;
        }
        let key = vec![
            "lodging".into(),
            reservation_number.clone(),
            local_date(&checkin).to_string(),
        ];
        let reservation = LodgingReservation {
            reservation_number,
            name,
            address: place.and_then(|p| self.address(p)),
            checkin,
            checkout,
            guests: self.people(node),
            url: self.url(node, &["url", "modifyReservationUrl"]),
        };
        self.push(
            InsightKind::Reservation(Reservation::Lodging(reservation)),
            key,
        );
    }

    fn trip(&mut self, node: usize, ty: Ty) {
        let graph = self.graph;
        let trip = graph.child(node, "reservationFor");
        let (number_field, from, to) = if ty == Ty::Train {
            (
                "trainNumber",
                ["departureStation", ""],
                ["arrivalStation", ""],
            )
        } else {
            (
                "busNumber",
                ["departureBusStop", "departureStation"],
                ["arrivalBusStop", "arrivalStation"],
            )
        };
        let station = |names: [&str; 2]| {
            trip.and_then(|t| {
                names
                    .iter()
                    .filter(|n| !n.is_empty())
                    .find_map(|n| self.place(t, n))
            })
        };
        let number = trip
            .and_then(|t| graph.text(t, number_field))
            .filter(|n| fits_identifier(n))
            .map(str::to_owned);
        let reservation_number = self.ident(node, "reservationNumber");
        let departure = trip.and_then(|t| self.time(t, &["departureTime"]));
        let (Some(departure_station), Some(departure)) = (station(from), departure) else {
            return self.missing(ty);
        };
        if number.is_none() && reservation_number.is_none() {
            return self.missing(ty);
        }
        let arrival = trip.and_then(|t| self.time(t, &["arrivalTime"]));
        let arrival_station = station(to);
        let key = vec![
            if ty == Ty::Train { "train" } else { "bus" }.into(),
            number
                .clone()
                .or_else(|| reservation_number.clone().map(|r| format!("#{r}")))
                .unwrap_or_default(),
            departure_station.clone(),
            local_date(&departure).to_string(),
        ];
        let passengers = self.people(node);
        let seats = self.tickets(node, false);
        if !self.ordered(ty, &departure, arrival.as_ref()) {
            return;
        }
        let url = self.url(node, &["url", "modifyReservationUrl"]);
        let reservation = if ty == Ty::Train {
            Reservation::Train(TrainReservation {
                reservation_number,
                train_number: number,
                departure_station,
                arrival_station,
                departure,
                arrival,
                passengers,
                seats,
                url,
            })
        } else {
            Reservation::Bus(BusReservation {
                reservation_number,
                bus_number: number,
                departure_station,
                arrival_station,
                departure,
                arrival,
                passengers,
                seats,
                url,
            })
        };
        self.push(InsightKind::Reservation(reservation), key);
    }

    fn event(&mut self, node: usize) {
        let event = self.graph.child(node, "reservationFor");
        let name = event.and_then(|e| self.text(e, "name")).and_then(display);
        let start = event.and_then(|e| self.time(e, &["startDate"]));
        let (Some(name), Some(start)) = (name, start) else {
            return self.missing(Ty::Event);
        };
        let end = event.and_then(|e| self.time(e, &["endDate"]));
        if !self.ordered(Ty::Event, &start, end.as_ref()) {
            return;
        }
        let key = vec!["event".into(), name.clone(), time_key(&start)];
        let reservation = EventReservation {
            reservation_number: self.ident(node, "reservationNumber"),
            name,
            start,
            end,
            venue: event.and_then(|e| self.place(e, "location")),
            tickets: self.tickets(node, true),
            url: self.url(node, &["url", "modifyReservationUrl"]),
        };
        self.push(
            InsightKind::Reservation(Reservation::Event(reservation)),
            key,
        );
    }

    fn food(&mut self, node: usize) {
        let place = self.graph.child(node, "reservationFor");
        let name = place.and_then(|p| self.text(p, "name")).and_then(display);
        let (Some(name), Some(start)) = (name, self.time(node, &["startTime"])) else {
            return self.missing(Ty::Food);
        };
        let key = vec!["food".into(), name.clone(), time_key(&start)];
        let reservation = FoodReservation {
            reservation_number: self.ident(node, "reservationNumber"),
            name,
            start,
            party_size: self
                .text(node, "partySize")
                .and_then(|size| size.parse::<u16>().ok()),
            url: self.url(node, &["url", "modifyReservationUrl"]),
        };
        self.push(
            InsightKind::Reservation(Reservation::Food(reservation)),
            key,
        );
    }

    fn rental_car(&mut self, node: usize) {
        let (Some(reservation_number), Some(pickup)) = (
            self.ident(node, "reservationNumber"),
            self.time(node, &["pickupTime"]),
        ) else {
            return self.missing(Ty::RentalCar);
        };
        let dropoff = self.time(node, &["dropoffTime"]);
        if !self.ordered(Ty::RentalCar, &pickup, dropoff.as_ref()) {
            return;
        }
        let key = vec![
            "rental_car".into(),
            reservation_number.clone(),
            local_date(&pickup).to_string(),
        ];
        let company = self
            .graph
            .child(node, "reservationFor")
            .and_then(|car| self.graph.name_of(car, "rentalCompany"))
            .or_else(|| self.graph.name_of(node, "provider"))
            .and_then(display);
        let reservation = RentalCarReservation {
            reservation_number,
            company,
            pickup,
            pickup_location: self.place(node, "pickupLocation"),
            dropoff,
            dropoff_location: self.place(node, "dropoffLocation"),
            url: self.url(node, &["url", "modifyReservationUrl"]),
        };
        self.push(
            InsightKind::Reservation(Reservation::RentalCar(reservation)),
            key,
        );
    }

    fn order(&mut self, node: usize) {
        let number = self.ident(node, "orderNumber");
        let merchant = self
            .graph
            .name_of(node, "seller")
            .or_else(|| self.graph.name_of(node, "merchant"))
            .and_then(display);
        let key = match (&number, &merchant) {
            (Some(number), _) => vec!["order#".into(), number.clone()],
            (None, Some(merchant)) => vec!["order@".into(), merchant.clone()],
            (None, None) => return self.missing(Ty::Order),
        };
        let status = self
            .graph
            .name_of(node, "orderStatus")
            .map(|s| local_name(s).unwrap_or(s))
            .and_then(display);
        let kind = InsightKind::Order {
            number,
            merchant,
            total: self.money(node),
            status,
            url: self.url(node, &["url"]),
        };
        self.push(kind, key);
    }

    fn invoice(&mut self, node: usize) {
        let number = self
            .ident(node, "confirmationNumber")
            .or_else(|| self.ident(node, "identifier"));
        let payee = self.graph.name_of(node, "provider").and_then(display);
        let total = self.money(node);
        let due = ["paymentDueDate", "paymentDue"]
            .iter()
            .find_map(|name| self.text(node, name).and_then(parse_date));
        if total.is_none() && due.is_none() {
            return self.missing(Ty::Invoice);
        }
        let key = match (&number, &payee) {
            (Some(number), _) => vec!["invoice#".into(), number.clone()],
            (None, Some(payee)) => vec!["invoice@".into(), payee.clone()],
            (None, None) => vec!["invoice".into(), format!("{total:?}{due:?}")],
        };
        let kind = InsightKind::Invoice {
            number,
            payee,
            total,
            due,
            url: self.url(node, &["url"]),
        };
        self.push(kind, key);
    }

    fn parcel(&mut self, node: usize) {
        let number: Option<String> = self.text(node, "trackingNumber").map(|n| {
            n.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_uppercase()
        });
        let number = number.filter(|n| fits_identifier(n));
        let mut names: Vec<&str> = Vec::new();
        let mut urls: Vec<&str> = Vec::new();
        for field in ["carrier", "provider"] {
            for value in self.graph.values(node, field) {
                match value {
                    Val::Text(text) => {
                        names.push(text.as_str());
                        urls.push(text.as_str());
                    }
                    Val::Node(org) => {
                        names.extend(
                            ["name", "legalName", "alternateName"]
                                .iter()
                                .filter_map(|n| self.text(*org, n)),
                        );
                        urls.extend(self.text(*org, "url"));
                        urls.extend(self.graph.node(*org).and_then(|o| o.id.as_deref()));
                    }
                }
            }
        }
        let carrier = self.data.carrier(&names, &urls);
        let (Some(number), Some(carrier)) = (number, carrier) else {
            return self.missing(Ty::Parcel);
        };
        let key = vec!["parcel".into(), carrier.as_str().to_owned(), number.clone()];
        let track = self.url(node, &["trackingUrl"]);
        let kind = InsightKind::Parcel {
            carrier,
            number,
            check_digit: false,
            track_url: None,
        };
        self.push(kind, key);
        if let Some(url) = track {
            self.push_action(LinkAction::Track, url, None);
        }
    }

    fn action(&mut self, node: usize, ty: Ty) {
        let action = match ty {
            Ty::Confirm => LinkAction::Confirm,
            Ty::View => LinkAction::View,
            Ty::Track => LinkAction::Track,
            _ => LinkAction::CheckIn,
        };
        let mut candidates: Vec<&str> = Vec::new();
        for value in self.graph.values(node, "target") {
            match value {
                Val::Text(text) => candidates.push(text),
                Val::Node(entry) => {
                    candidates.extend(self.text(*entry, "urlTemplate"));
                    candidates.extend(self.text(*entry, "url"));
                }
            }
        }
        candidates.extend(self.text(node, "url"));
        candidates.extend(self.text(node, "urlTemplate"));
        let url = candidates.iter().find_map(|text| {
            Url::parse(text.trim())
                .ok()
                .filter(|url| url.scheme() != UrlScheme::Mailto && url.fits_destination())
        });
        let Some(url) = url else {
            if !candidates.is_empty() {
                let at = self.at;
                self.log.step(|| Explanation::new("url_refused").at(at));
            }
            return self.missing(ty);
        };
        let label = self.text(node, "name").and_then(display);
        self.push_action(action, url, label);
    }

    fn push_action(&mut self, action: LinkAction, url: Url, label: Option<String>) {
        if url.scheme() == UrlScheme::Mailto || !url.fits_destination() {
            return;
        }
        let key = vec![
            "action".into(),
            format!("{action:?}"),
            url.as_str().to_owned(),
        ];
        let kind = InsightKind::Action {
            action,
            destination: url.clone(),
            url,
            via: None,
            tracked: false,
            text_host: None,
            label,
        };
        self.push(kind, key);
    }
}

fn display(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| truncate_display(&text))
}

fn add(list: &mut Vec<String>, value: Option<String>) {
    if let Some(value) = value
        && list.len() < MAX_LIST
        && !list.contains(&value)
    {
        list.push(value);
    }
}

fn union(list: &mut Vec<String>, more: Vec<String>) {
    for value in more {
        add(list, Some(value));
    }
}

fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
    if slot.is_none() {
        *slot = value;
    }
}

fn iata(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_uppercase();
    (code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase())).then_some(code)
}

fn airline(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_uppercase();
    (code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(code)
}

fn time_key(at: &LocalOrZoned) -> String {
    match at {
        LocalOrZoned::Local(at) => at.to_string(),
        LocalOrZoned::Zoned(at, _) => at.to_rfc3339(),
    }
}

fn merge(into: &mut InsightKind, from: InsightKind) {
    match (into, from) {
        (InsightKind::Reservation(into), InsightKind::Reservation(from)) => match (into, from) {
            (Reservation::Flight(a), Reservation::Flight(b)) => {
                fill(&mut a.reservation_number, b.reservation_number);
                fill(&mut a.carrier, b.carrier);
                fill(&mut a.arrival, b.arrival);
                fill(&mut a.url, b.url);
                union(&mut a.passengers, b.passengers);
                union(&mut a.seats, b.seats);
            }
            (Reservation::Lodging(a), Reservation::Lodging(b)) => {
                fill(&mut a.address, b.address);
                fill(&mut a.checkout, b.checkout);
                fill(&mut a.url, b.url);
                union(&mut a.guests, b.guests);
            }
            (Reservation::Train(a), Reservation::Train(b)) => {
                fill(&mut a.reservation_number, b.reservation_number);
                fill(&mut a.arrival_station, b.arrival_station);
                fill(&mut a.arrival, b.arrival);
                fill(&mut a.url, b.url);
                union(&mut a.passengers, b.passengers);
                union(&mut a.seats, b.seats);
            }
            (Reservation::Bus(a), Reservation::Bus(b)) => {
                fill(&mut a.reservation_number, b.reservation_number);
                fill(&mut a.arrival_station, b.arrival_station);
                fill(&mut a.arrival, b.arrival);
                fill(&mut a.url, b.url);
                union(&mut a.passengers, b.passengers);
                union(&mut a.seats, b.seats);
            }
            (Reservation::Event(a), Reservation::Event(b)) => {
                fill(&mut a.reservation_number, b.reservation_number);
                fill(&mut a.end, b.end);
                fill(&mut a.venue, b.venue);
                fill(&mut a.url, b.url);
                union(&mut a.tickets, b.tickets);
            }
            (Reservation::Food(a), Reservation::Food(b)) => {
                fill(&mut a.reservation_number, b.reservation_number);
                fill(&mut a.url, b.url);
                a.party_size = a.party_size.max(b.party_size);
            }
            (Reservation::RentalCar(a), Reservation::RentalCar(b)) => {
                fill(&mut a.company, b.company);
                fill(&mut a.pickup_location, b.pickup_location);
                fill(&mut a.dropoff, b.dropoff);
                fill(&mut a.dropoff_location, b.dropoff_location);
                fill(&mut a.url, b.url);
            }
            _ => {}
        },
        (
            InsightKind::Order {
                number,
                merchant,
                total,
                status,
                url,
            },
            InsightKind::Order {
                number: n,
                merchant: m,
                total: t,
                status: s,
                url: u,
            },
        ) => {
            fill(number, n);
            fill(merchant, m);
            fill(total, t);
            fill(status, s);
            fill(url, u);
        }
        (
            InsightKind::Invoice {
                number,
                payee,
                total,
                due,
                url,
            },
            InsightKind::Invoice {
                number: n,
                payee: p,
                total: t,
                due: d,
                url: u,
            },
        ) => {
            fill(number, n);
            fill(payee, p);
            fill(total, t);
            fill(due, d);
            fill(url, u);
        }
        (InsightKind::Action { label, .. }, InsightKind::Action { label: l, .. }) => {
            fill(label, l);
        }
        _ => {}
    }
}
