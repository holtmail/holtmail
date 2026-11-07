# holtmail-insight-pass

An `InsightExtractor` (id `pass`) that reads wallet pass (`.pkpass`) attachments. It opens each pkpass the runner hands over as a zip archive within fixed caps, reads only `pass.json` and reports one `InsightKind::Pass` item per attachment: the pass style, issuer, description, relevant date and, for air boarding passes, boarding details from the pass's `semantics`. The pass signature is never checked. The design is in [INSIGHTS.md](../../../docs/INSIGHTS.md) §4.1, §6.1 and §7.6.

## Public API

### `PassExtractor`

- `PassExtractor::new()` (and `Default`). It has no data and no configuration.
- `ID` is `"pass"`.
- `ALGORITHM` is the version of the reading code.
- `MAX_ENTRIES` (64), `MAX_PASS_JSON_BYTES` (512 KiB) and `MAX_UNCOMPRESSED_BYTES` (10 MiB) are the archive caps below.

Guarantees, on top of the `InsightExtractor` contract:

- **Only `InsightKind::Pass`, at most one item per attachment.**
  - Items come in attachment order, one per ordinal. An attachment that repeats an earlier ordinal is skipped.
  - Only the first `MAX_PKPASS_ATTACHMENTS` (8) attachments with content are looked at, so there are at most 8 items. The runner already fills `content` only for pkpass attachments (by media type or `.pkpass` file name) within those caps.
- **Capped reading.** An attachment gives nothing when any of these hold:

  | Check | Explain rule |
  |---|---|
  | content is over `MAX_PKPASS_BYTES` (2 MiB) | `refused:content_too_large` |
  | not a zip archive the `zip` crate reads | `refused:not_zip` |
  | more than 64 entries in the central directory | `refused:too_many_entries` |
  | two entries with the same name | `refused:duplicate_names` |
  | an entry name starts with `/` or `\`, has a `..` segment (split on `/` and `\`) or holds a NUL byte | `refused:unsafe_path` |
  | the declared uncompressed sizes add up to more than 10 MiB | `refused:uncompressed_too_large` |
  | no `pass.json` at the archive root | `refused:no_pass_json` |
  | `pass.json` declares more than 512 KiB | `refused:pass_json_declared_too_large` |
  | `pass.json` is encrypted | `refused:encrypted` |
  | `pass.json` is neither stored nor deflated | `refused:compression` |
  | reading `pass.json` fails (bad local header, bad deflate stream, CRC mismatch) | `refused:read_failed` |
  | `pass.json` inflates to more than 512 KiB, whatever it declares | `refused:pass_json_too_large` |

  - No entry other than `pass.json` is ever inflated, so a bomb in `strip.png` costs nothing beyond the size check.
  - `pass.json` is read through `Read::take(512 KiB + 1)`. The buffer is preallocated to the declared size, which is at most 512 KiB.
- **What is read from `pass.json`.** A UTF-8 BOM is skipped. The JSON parses with `serde_json`'s default recursion limit (128). Only these keys are read:
  - the style key: exactly one of `boardingPass`, `eventTicket`, `coupon`, `storeCard` and `generic`, holding an object. None or several give nothing (`style_missing`, `style_ambiguous`).
  - `organizationName` and `description`: required, trimmed, non-empty, cut to 512 chars with `truncate_display`. Either missing gives nothing (`organization_missing`, `description_missing`).
  - `relevantDate`: an ISO 8601 date-time. With an offset (`+01:00`, `-0800`, `Z`) it becomes `LocalOrZoned::Zoned` in that offset with no zone name, never converted. Without one it becomes `LocalOrZoned::Local`. Seconds and fractions are optional. A date without a time, or anything else, gives `None`.
  - `semantics` at the top level: see boarding below. Field-level semantics are not read.
  - `barcodes[].message`: counted for `explain` only. The contract has no field for it, and BCBP parsing is Later.
- **Boarding from `semantics` only.** `boarding` is `Some` only for a `boardingPass` whose `transitType` is `PKTransitTypeAir`, whose top-level `semantics` exists and has both airports. It then holds one leg:
  - `from`, `to`: `departureAirportCode`, `destinationAirportCode`, three ASCII letters, uppercased. Either missing or malformed leaves `boarding` `None`.
  - `carrier`: `airlineCode`, two ASCII letters or digits (not two digits), uppercased.
  - `flight`: `flightNumber` (a JSON number or string) when it is 1–4 digits with an optional letter. Otherwise `flightCode` with spaces removed and uppercased: taken as is when it already has that shape, else its first two chars are the airline designator and the rest must have the shape. A designator that differs from `airlineCode` gives no flight. Without `airlineCode`, the designator fills `carrier`.
  - `date`: `PassDate::Full` from `currentDepartureDate`, else `originalDepartureDate`. A date-time gives its date as written in its own offset.
  - `seat`: the first entry of `seats`. `seatRow` and `seatNumber` are joined (`14` + `C` is `14C`), unless the number already starts with the row. Spaces are dropped, and the result must be 1–8 ASCII letters or digits, uppercased.
  - `pnr`: `confirmationNumber`, trimmed, at most 64 chars of letters, digits and `-`, never cut.
  - `passenger`: `passengerName.givenName` and `familyName` joined with a space, display text.
- **Never verified.** `signature_checked` is always `false`. `manifest.json` and `signature` are not read.
- Provenance `PkPass`, confidence `Certain`, score 1000, origin `PartRef::Attachment(ordinal)`, `forwarded: false`. The runner's merge key for a pass is its attachment ordinal.
- `version()` is `ALGORITHM + "+" + blake3(caps)[..12]`, where the caps are the five byte and count limits above. Changing a cap changes the version.

### `explain`

Steps carry the attachment as their part and counts only. They never carry a value, name or code from the pass.

| Rule | Meaning |
|---|---|
| `attachment` | an attachment with content; the count is its size in bytes |
| `refused:*` | the archive check that refused it (table above); the count is its entry count, 0 before the directory was read |
| `entries` | entries in the archive |
| `pass_json` | bytes of `pass.json` read |
| `json_invalid`, `json_not_object` | `pass.json` does not parse, or is not an object |
| `style_missing`, `style_ambiguous`, `organization_missing`, `description_missing` | required parts missing |
| `style:boarding_pass`, `style:event_ticket`, `style:coupon`, `style:store_card`, `style:generic` | the style |
| `relevant_date` | `relevantDate` present; count 1 when it parsed, 0 when not |
| `semantics` | count 1 when top-level `semantics` exists |
| `barcodes` | barcode entries with a `message` (counted up to 64) |
| `boarding` | boarding found; the count is its legs |
| `boarding:not_boarding_pass`, `boarding:not_air`, `boarding:no_semantics`, `boarding:airports_missing` | why there is no boarding |
| `chosen` | an item was kept |
| `duplicate_ordinal`, `attachments_capped` | skipped attachments |

## Limits and quirks

- **Field arrays are not read.** When `semantics` lacks the airports, `boarding` stays `None` even if `primaryFields` show them. Field keys and labels are the issuer's free choice, so reading them would be guessing.
- **One leg per pass.** A pkpass describes one flight. A multi-leg trip comes as several passes, each its own item.
- **Lying sizes.** The `zip` crate stops inflating at the declared uncompressed size, so a `pass.json` that declares little and inflates to more fails its CRC check (`refused:read_failed`). The counting reader is the backstop if that ever changes.
- **Duplicate names.** The `zip` crate keeps one entry per name and would hide the rest from the entry count, so an archive with any repeated name is refused.
- **Signature.** Passes are not verified against Apple's certificate chain. A forged pass reads like a real one.
- **Localisation.** `*.lproj/pass.strings` are not read. A `description` that is a localisation key shows as the key.

## Conformance and tests

- `tests/conformance.rs` runs `holtmail_insight::testing::conformance::extractor` with every fixture below, nine copies of a pass (eight items) and a mix of a bomb, an event ticket, an empty attachment and a boarding pass. The suite adds personal mail and the shared hostile corpus (a zip bomb and a pkpass with `..` entries).
- `tests/fixtures.rs` checks each fixture, the full boarding pass field by field, the explanation rules, and runs a `.eml` built at test time (the boarding pass as a base64 attachment) through `holtmail_mime` and `holtmail_insights::Runner`.
- `tests/bounds.rs` has proptests (arbitrary bytes, damaged and truncated passes) and a timed bomb test.
- Fixtures are built in-test from invented `pass.json` files (Example Air, `XA`, FRA to LIS) in `tests/common/mod.rs`: `pkpass-boarding-air`, `pkpass-event-ticket`, `pkpass-stored`, `pkpass-bom`, `pkpass-64-entries`, `pkpass-boarding-no-semantics` give a pass. `pkpass-zip-bomb` (declares 100 bytes, inflates to 20 MiB), `pkpass-zip-bomb-honest` (11 MiB `strip.png`), `pkpass-pass-json-too-large`, `pkpass-dotdot-entry`, `pkpass-absolute-entry`, `pkpass-too-many-entries`, `pkpass-duplicate-pass-json`, `pkpass-missing-description`, `pkpass-missing-organization`, `pkpass-nested-pass-json`, `pkpass-not-zip`, `pkpass-empty`, `pkpass-encrypted`, `pkpass-bzip2`, `pkpass-content-over-cap` and `pkpass-invalid-json` give nothing.
- `fuzz/` is a `cargo-fuzz` crate with its own workspace and a pinned nightly. Its `extract` target hands the bytes over as one pkpass attachment and checks determinism and at most one item. Run it with the pinned toolchain's `cargo` first on `PATH`:

  ```sh
  cd crates/insight-pass/fuzz
  PATH="$HOME/.rustup/toolchains/nightly-2026-10-01-aarch64-apple-darwin/bin:$PATH" cargo fuzz run extract -- -max_total_time=60
  ```
