# holtmail-insight-schemaorg

An `InsightExtractor` (id `schemaorg`) that reads the schema.org markup senders put in their HTML mail: JSON-LD from `HtmlView.ld_json` and microdata from `HtmlView.microdata`. It maps the seven reservation types, `Order`, `Invoice`, `ParcelDelivery` and the `ConfirmAction`, `ViewAction`, `TrackAction` and `CheckInAction` actions to insight items. ISO 4217 exponents and parcel carrier names are data in `data/`, embedded at build time. The design is in [INSIGHTS.md](../../../docs/INSIGHTS.md) §7.5.

## Public API

- `SchemaOrgExtractor::new()` (and `Default`) uses the embedded tables.
- `ID` is `"schemaorg"`.
- `ALGORITHM` is the version of the reading and mapping code.
- `MAX_NODES` (2,000), `MAX_JSON_DEPTH` (32) and `MAX_MICRODATA_DEPTH` (16) are the walk's budgets.
- `version()` is `ALGORITHM + "+" + blake3(currencies.toml, carriers.toml)[..12]`, for example `1+3f9a…`. It is `1+unreadable` if the embedded tables ever failed to parse, and then nothing is extracted.

Guarantees, on top of the `InsightExtractor` contract:

- **Only declared markup.** Items come from JSON-LD blocks and microdata items, never from visible text. A message without HTML gives nothing. Forged markup is extracted like any other, because judging the sender is the runner's job.
- **Known types only.**
  - `@type` (or `itemtype`) may be a bare name, `schema:X`, `http://schema.org/X` or `https://schema.org/X`.
  - In a type array only the known members count, and the first known one decides.
  - Other types give nothing, but their `potentialAction` is still read (Gmail's `EmailMessage` with a `ConfirmAction`).
- **Required fields.** An item missing a required field of its type, or whose end is before its start, is dropped whole. Every kept item is `Certain` with score 1000, origin `Html`, provenance `JsonLd` or `Microdata`.
- **Merged by key.** Items with equal merge keys become one item. It keeps the first item's place and fields, fills missing fields from later items, and joins passengers, seats, guests and tickets (at most 9 each, first seen first). The party size is the larger one. A merged item is `JsonLd` when any part was, and `forwarded` only when every part was.
- **Forwards.** An item is `forwarded` when its JSON-LD block index is at or after `ld_json_forward_at`, or its microdata item is `below_forward`.
- **Exact values.**
  - Times keep their written offset and are never converted (see Times).
  - Money parses as a checked decimal in a currency of the embedded table, or is left out; the item stays.
  - URLs go through `holtmail_insight::Url`. A URL with a refused scheme, userinfo or over 8 KiB is left out.
  - Identifiers (reservation, order, invoice and tracking numbers, seats, tickets) over 64 chars are left out, never cut.
  - Display text (names, addresses, stations, labels) has its whitespace collapsed and is cut to 512 chars with `…`.
- **Document order.** JSON-LD blocks first, then microdata items, each in document order; a node's actions follow it. At most 16 items.

### Types, required fields and merge keys

| schema.org type | Item | Required | Merge key | Joined on merge |
|---|---|---|---|---|
| `FlightReservation` | `Reservation::Flight` | `reservationNumber` or `reservationFor.flightNumber`; `reservationFor.departureAirport.iataCode`, `arrivalAirport.iataCode`, `departureTime` | airline + flight number + local departure date; without a flight number, reservation number + departure airport + date | passengers, seats |
| `LodgingReservation` | `Reservation::Lodging` | `reservationNumber`, `reservationFor.name`, `checkinTime` (or `checkinDate`) | reservation number + check-in date | guests |
| `TrainReservation`, `BusReservation` | `Reservation::Train`, `Reservation::Bus` | `reservationNumber` or `trainNumber`/`busNumber`; departure station; `departureTime` | number (else reservation number) + departure station + local departure date | passengers, seats |
| `EventReservation` | `Reservation::Event` | `reservationFor.name`, `reservationFor.startDate` | name + start | tickets |
| `FoodEstablishmentReservation` | `Reservation::Food` | `reservationFor.name`, `startTime` | name + start | party size |
| `RentalCarReservation` | `Reservation::RentalCar` | `reservationNumber`, `pickupTime` | reservation number + pickup date | |
| `Order` | `Order` | `orderNumber` or `seller.name` (or `merchant.name`) | order number, else seller | |
| `Invoice` | `Invoice` | `totalPaymentDue` or `paymentDueDate` | `confirmationNumber` (or `identifier`), else payee | |
| `ParcelDelivery` | `Parcel` | `trackingNumber` and `carrier` | carrier + number | |
| `ConfirmAction`, `ViewAction`, `TrackAction`, `CheckInAction` | `Action` | a usable URL | action + URL | |

Where the fields come from:

- **Flight.** The airline is `reservationFor.airline.iataCode`: two ASCII letters or digits, uppercased. Airports are three ASCII letters, uppercased; anything else is missing. A flight number that starts with the airline code (`XA482`, `XA 482`) loses it. Passengers are `underName` (`name`, else `givenName familyName`). Seats are `airplaneSeat`, then `reservedTicket.ticketedSeat.seatNumber`. `checkinUrl` also gives a `CheckIn` action.
- **Lodging.** The address is `reservationFor.address`: text, or a `PostalAddress` joined as street, locality, region, postal code, country.
- **Train and bus.** Stations are `departureStation`/`arrivalStation`, for buses first `departureBusStop`/`arrivalBusStop`: a name, or the place's address.
- **Event.** The venue is `reservationFor.location` (name, else address). Tickets are `reservedTicket.ticketNumber`, else its seat number.
- **Rental car.** The company is `reservationFor.rentalCompany.name`, else `provider.name`. Locations are `pickupLocation`/`dropoffLocation` (name, else address).
- **Order.** The status is `orderStatus` without the schema.org prefix (`OrderProcessing`).
- **Invoice.** The payee is `provider.name`; the due date is the date of `paymentDueDate` in its own offset.
- **Booking pages.** A reservation's `url` is `url`, else `modifyReservationUrl`.
- **Parcel.**
  - The tracking number has its whitespace removed and is uppercased, as `insight-parcel` prints it.
  - `carrier` (else `provider`) becomes a `CarrierId` through `data/carriers.toml`: by name, or by the host of its `url` or `@id` (subdomains included), giving the ids of `insight-parcel`'s table (`ups`, `usps`, `fedex`, `dhl_express`, `russian_post`). The runner's carrier + number key then matches the heuristic parcel.
  - An unknown carrier gets the slug of its name: ASCII letters and digits lowercased, other runs as one `_`, at most 32 chars. A name with no ASCII letter or digit drops the item.
  - `check_digit` is `false`: the number is not checked here.
  - `track_url` is always `None`, because the contract builds it only from a carrier template. `trackingUrl` becomes a separate `Track` action instead.
- **Actions.** The URL is the first usable of `target` (text, or an `EntryPoint`'s `urlTemplate` or `url`), `url` and `urlTemplate`. `mailto:` and URLs over 2 KiB are not usable. `url` and `destination` are equal, `via` and `text_host` are `None`, `tracked` is `false`; the runner unwraps and fills them. `label` is `name`. Actions come from top-level action nodes and from any top-level node's `potentialAction`. `HttpActionHandler` endpoints are never read.

### Times

- `2027-03-14T09:30:00+01:00`, `…Z`, `…+0100` and `…+01` give `LocalOrZoned::Zoned(time, None)` in the written offset, never converted.
- Without an offset (`2027-03-14T09:30`) the time is `Local`.
- A bare date (`2027-03-14`) is `Local` at 00:00.
- Seconds and a fraction are optional; `T` may be a space. Anything else (`14 March 2027`, fullwidth digits, impossible dates) is missing.
- Start and end compare as instants when both are zoned, else as wall times.
- No clock is read; nothing here resolves relative dates.

### Money

- The amount is `totalPrice`, `totalPaymentDue`, `price` or `priceSpecification`, in that order; the first one present decides.
  - A text value takes its currency from the same node's `priceCurrency` (or `currency`).
  - A `PriceSpecification` gives `price` and `priceCurrency`; a `MonetaryAmount` gives `value` and `currency`.
- The currency code is trimmed and uppercased, then looked up in `data/currencies.toml`. An unknown code leaves the money out.
- The amount is an optional `-`, ASCII digits, and optionally `.` with at least one digit. It is refused for an exponent, `+`, thousands separators, more than 18 digits, more fraction digits than the currency's exponent, or an `i64` overflow (checked arithmetic). Never a float.
- A JSON integer is read as its digits. A JSON float is read through its shortest decimal text, written without an exponent and without trailing fraction zeros, so `1e3` and `1000.0` are read as `1000` (valid in JPY) and `12.50` as `12.5`; a float whose decimal exponent is beyond ±32 is refused, and a float with more digits than an `f64` holds loses them. Prices in strings are exact.

## Budgets

Nothing recurses over input; both walks use an explicit stack.

| Input | Bound |
|---|---|
| A JSON-LD block | serde_json's recursion limit (128); a block that fails to parse is skipped |
| JSON-LD depth | `MAX_JSON_DEPTH` (32) levels from a top-level node; deeper values are skipped |
| Microdata depth | `MAX_MICRODATA_DEPTH` (16) items from a top-level item; deeper items are skipped |
| Nodes | `MAX_NODES` (2,000) JSON values, microdata items and property values per message, counted while expanding. Past it the walk stops, the top-level node being read is dropped, and nothing later is read |
| `@id` | A reference is an object with only `@id`. It resolves against the same block's top-level nodes (`@graph` members, array elements, or the top object), one level deep: references inside the resolved node are not followed. Each id expands at most once per top-level node, and a top-level node never expands into itself |
| Microdata `Item(i)` | Each index expands at most once per top-level item; an index out of range or seen before is skipped |

A JSON-LD top level is an object with `@type`, the members of its `@graph`, or the objects of a top-level array. A microdata top level is an item with `top_level` set.

## `explain`

`explain` gives the walk's cuts, then one step per top-level node and decision. Steps hold rule names, positions (the JSON-LD block index or the microdata item index), counts and the type name in lowercase snake case (`flight_reservation`) from the extractor's own type table. They never hold a value, host or URL.

| Rule | Meaning |
|---|---|
| `ld_json_unparsable` | blocks that did not parse, counted |
| `depth_cut` | values or items skipped by a depth cap, counted |
| `reference_cut` | `@id` references or microdata indexes not followed, counted |
| `node_budget` | the 2,000-node budget ran out |
| `root` | a top-level node of a known type |
| `unknown_type` | a top-level node of no known type |
| `missing_required` | dropped: a required field is missing or invalid |
| `end_before_start` | dropped: the end is before the start |
| `money_dropped` | an amount or currency did not read; the item stays |
| `url_refused` | a URL did not read as a `holtmail_insight::Url` |
| `merged` | joined into an earlier item with the same key |
| `item_cap` | more than 16 items; the count before the cap |
| `chosen` | an item, by its position in the output |

## Data files

- `data/currencies.toml`: every ISO 4217 code with a minor unit and its exponent (JPY 0, USD 2, KWD and BHD 3, CLF and UYW 4), plus codes withdrawn since 2015 that archived mail still carries (`HRK`, `SLL`, `ANG`, …). Codes without a minor unit (`XAU`, `XDR`, …) are absent. These are public facts from the ISO 4217 list, offered under CC0.
- `data/carriers.toml`: carrier names and hosts mapped to `insight-parcel`'s carrier ids. Our own table, CC0.

Both files are hashed into `version()`, so changing either reruns the extractor.

## Limits and quirks

- One-level `@id` resolution means a `Flight` reached by reference keeps its own nested objects but not further references: `"airline": {"@id": "#a"}` inside it is not followed, so the airline code is missing.
- Microdata is read as the scanner reports it: `itemref` is already expanded, and `content`, `datetime` and `value` attributes are already the values.
- A nested `ParcelDelivery` inside an `Order` (`orderDelivery`), or an `Order` inside a `ParcelDelivery` (`partOfOrder`), gives no second item; only top-level nodes and their `potentialAction` are mapped.
- An invoice with neither number nor payee gets a merge key from its total and due date.

## Tests

- Unit tests in `src/` cover the walk budgets (2,000 nodes, depth 32, microdata depth 16, `@id` cycles, fan-in, microdata index cycles and out-of-range indexes), the time and decimal parsers (with `proptest`: round-trips, no panics, extra fraction digits and exponents refused) and the data tables.
- `tests/conformance.rs` runs `holtmail_insight::testing::conformance::extractor` with built positives for every type and hard negatives, and checks the mapping, merging, money, forwards, the 16-item cap and `explain`.
- `tests/fixtures.rs` runs every `.eml` in `tests/fixtures/` through `holtmail_mime::parse` and the full `holtmail_insights::Runner`, compares the kept items field by field, and runs the conformance suite over the same inputs.
- `fuzz/` holds the `extract` target: arbitrary bytes become up to 8 JSON-LD blocks, a forward position and a microdata list with arbitrary item indexes; it asserts equal output on two runs and at most 16 items. Run it as the html-scan README describes, with the nightly from `fuzz/rust-toolchain.toml`:

  ```sh
  TC=$(rustup run nightly-2026-10-01 rustc --print sysroot)
  PATH="$TC/bin:$PATH" RUSTC="$TC/bin/rustc" cargo fuzz run extract -- -max_total_time=60
  ```
