# holtmail-insight-parcel

An `InsightExtractor` (id `parcel`) that finds parcel tracking numbers of UPU S10 items, UPS, USPS, FedEx, DHL Express and Russian Post in a message's own visible text. A number is reported only when its check digit is right and there is evidence for it: a tracking keyword next to it, or a sender domain that belongs to the carrier. Patterns, keywords, sender hints, scores and tracking URL templates are data in `data/carriers.toml`, embedded at build time. The design is in [INSIGHTS.md](../../../docs/INSIGHTS.md) §7.4.

## Public API

### `ParcelExtractor`

- `ParcelExtractor::new()` (and `Default`) uses the embedded table.
- `ParcelExtractor::with_data(toml: &str) -> Result<ParcelExtractor, DataError>` uses another table in the same format. It exists for tests with invented host hints. Its version carries that table's hash.
- `DataError` is a table that does not parse or breaks a rule below. Its `Display` names the rule, never mail content.

Guarantees, on top of the `InsightExtractor` contract:

- **Only `InsightKind::Parcel`, at most 4 items**, highest score first, then in text order. At most one item per carrier and number.
- **Only the message's own content.**
  - Candidates come from the HTML segments when the message has HTML, else from the plain text.
  - Nothing below an inline-forward marker (`forward_at`, `below_forward`) and nothing at or after the footer marker (`footer_at`) is a candidate.
  - Anchors are context only. An anchor whose `href` holds the number and whose text or `aria-label` holds a keyword counts as keyword evidence. A number found only in an `href` gives nothing.
- **Exact shapes.**
  - Text is NFKC-normalised, so fullwidth digits read as ASCII.
  - A number matches a carrier pattern exactly in compact uppercase form. Single spaces inside a grouping are dropped: `1Z 999 AA1 01 2345 6784` reads as `1Z999AA10123456784`, `ra 123 456 785 ru` as `RA123456785RU`.
  - The stored `number` is that compact form, never longer than 64 chars.
  - A number is no candidate when it is part of a longer run: a single word or a grouping with another digit group joined by a single space before or after it (so `55 1234567891` gives nothing), digits across `-`, `/`, `.`, `,` or `+`, a leading `+`, or a bracket next to it. This is what keeps card numbers, IBANs, phone numbers and dates out.
- **Check digit required.** Every pattern of the six carriers has a check digit. A number whose check digit is wrong gives nothing. Every item has `check_digit: true`. The `no_check_digit` score (500, `Possible`) applies only to a pattern without a check digit, and the embedded table has none.
- **Evidence required.** Scores come from `[scores]`:

  | Evidence | Score | Tier |
  |---|---|---|
  | a keyword within `keyword_window` (64) chars, or a matching anchor | 850 | Likely |
  | a `From` domain equal to or under a host hint of the carrier | 900 | Likely |
  | both | 950 | Likely |
  | neither | nothing | |

  - Keywords are the shared multilingual `[keywords]` plus the carrier's own `keywords` (brand names). They match on word boundaries, without boundaries next to CJK text, and never inside forwarded content.
  - Host hints compare the `From` domain by suffix on a label boundary: `parcels.example.com` matches `parcels.example.com` and `eu.parcels.example.com`, never `otherparcels.example.com`.
  - A word from `[negative]` (order, invoice, account, card, phone, fax and their translations) ending within `negative_window` (24) chars before the number drops it, unless a tracking keyword is closer before it.
  - Items at or above `likely` (700) are `Likely`, others `Possible`.
- **Unambiguous.** When one number at one place fits several carriers, the one with a host hint is kept, else the one with its own keyword. Otherwise none is reported.
- **`track_url` only from the table.** It is built from the carrier's https template, so the host never comes from mail. S10 numbers pick the template by their two-letter country suffix. A country without a template gives `None`.
- Provenance `Heuristic`, origin `Html` or `Text`, `forwarded: false`.
- `version()` is `ALGORITHM + "+" + blake3(carriers.toml)[..12]`, for example `1+3f9a…`. It is `1+unreadable` if the embedded table ever failed to parse, and then nothing is extracted.

### `explain`

`explain` gives one step per candidate and decision. Each step has a rule name, the part, the char offset, the candidate's `Skeleton` and keywords exactly as written in `carriers.toml`. It never contains a number, host or URL.

| Rule | Meaning |
|---|---|
| `pattern` | a candidate fits a carrier pattern |
| `boundary` | dropped: part of a longer digit run |
| `check_digit` | score 1 when the check digit holds, 0 when it does not (dropped) |
| `forwarded`, `footer` | dropped: below the forward marker, in the footer |
| `negative` | dropped: an order, invoice, phone or similar word is closer |
| `keyword` | the nearest keyword; the score is its distance in chars |
| `anchor` | an anchor with the number and a keyword |
| `host_hint` | the sender domain matches the carrier |
| `no_evidence` | dropped: no keyword, anchor or host |
| `ambiguous` | dropped: several carriers, none with stronger evidence |
| `chosen` | an item, with its score |
| `candidates_capped` | more than 64 candidates; the rest are not looked at |

### `check_digit(scheme: &str, number: &str) -> Option<bool>`

The check-digit test, public for property tests and the README examples. `number` is the compact uppercase form. The result is `None` for an unknown scheme or a number without the scheme's shape, else whether the check digit matches. Pure and never panics.

| Scheme | Shape | Check | Invented valid example |
|---|---|---|---|
| `s10` (UPU S10) | 2 letters, 8 digits, check digit, 2-letter country | weights 8,6,4,2,3,5,9,7 over the 8 digits; `11 - sum % 11`, where 10 gives 0 and 11 gives 5 | `RA123456785RU` |
| `ups` | `1Z`, 15 letters or digits, check digit | letters become `(code - 63) % 10`; weights 1,2,1,2… from the left; `(10 - sum % 10) % 10` | `1Z4E8752A038192654` |
| `usps` (IMpb) | 20–22 digits | weights 3,1,3… from the right of the body; `(10 - sum % 10) % 10` | `9400111899223197428497` |
| `fedex` | 12 digits | weights 3,1,7,3,1,7… from the left over the first 11; `sum % 11 % 10` | `798530184721` |
| `fedex` (Ground) | 15 digits | weights 3,1,3… from the right of the body; `(10 - sum % 10) % 10` | `612900348571028` |
| `dhl_express` | 10 digits | the first 9 digits as a number, `% 7` | `4738291051` |
| `russian_post` | 14 digits | weights 3,1,3… from the left over the first 13; `(10 - sum % 10) % 10` | `14102081078442` |

Changes a scheme cannot catch:

- `s10`: two sums map to check digit 5 (`11 - 0` and `11 - 6`), so a changed digit that moves the sum between them passes.
- `ups`: a digit on a weight-2 position changed by 5.
- `fedex` 12 digits: two sums map to check digit 0 (remainders 0 and 10).
- `dhl_express`: a digit changed by 7.
- The 3/1 schemes (`usps`, `fedex` 15, `russian_post`) catch every single changed digit.

### `ALGORITHM: u32`, `ID: &str`

`ALGORITHM` (`1`) versions the matching and scoring code, and the table hash is appended to it. `ID` is `"parcel"`.

## Data file (`data/carriers.toml`)

- `[scores]`: `keyword`, `host`, `keyword_and_host`, `no_check_digit`, `likely`. All must be below 1000.
- `[context]`:
  - `keyword_window` is the char distance for keywords;
  - `negative_window` is the char distance for negative words;
  - `max_group_words` (1–16) is how many space-separated groups one number may span.
- `[keywords]`: tracking words per language (en, de, fr, es, pt, it, ru, zh, ja, ko, tr, pl, nl), shared by every carrier.
- `[negative] words`: order, invoice, account, card and phone words.
- `[carriers.<id>]`: the id is the contract's `CarrierId` (`[a-z0-9_]`).
  - `check` is one of the schemes above.
  - `patterns` use `A` for a letter, `D` for a digit, `X` for either, and other ASCII letters and digits literally; each is at most 32 chars.
  - `keywords` are the carrier's own words.
  - `hosts` are registrable domains for sender hints.
  - `track_url` is an https template with exactly one `{number}`. S10 has `[carriers.s10.track_urls]` keyed by country (`RU`, `US`, `DE`, `CN`, `GB`) instead.

Carrier ids: `s10`, `ups`, `usps`, `fedex`, `dhl_express`, `russian_post`. A table that breaks any rule is refused: an unknown scheme, a bad id, a non-https template or one without `{number}`, a bad pattern char, or scores of 1000 or more.

## Limits

- Text and segments are read up to `MAX_TEXT_BYTES`. At most 64 shape-matching candidates are examined per message, and the keyword search looks only at a window around each one, so time is linear in the input.
- The tracking URL templates and S10 country hosts are taken from the carriers' public sites and have not been checked against live tracking pages (VERIFY).
- Host hints use the `From` domain only. Until trust lands (Phase 38), a forged `From` on a carrier domain gives 900 to a number that also has a valid check digit.

## Tests

- `src/check.rs`: a valid and an off-by-one number for every scheme, wrong shapes and unknown schemes.
- `src/data.rs`: the embedded table parses, templates build, and broken tables are refused.
- `src/extract.rs`: groupings, longer digit runs, keyword distance, negative words, forward and footer, anchors as context, and the 4-item cap.
- `tests/check_digits.rs` (proptest):
  - exactly one check digit completes a random body, for every scheme;
  - a single changed digit invalidates a valid number, except in the blind spots listed above;
  - `check_digit` never panics on arbitrary strings.
- `tests/conformance.rs`:
  - positives for every carrier and language;
  - hard negatives: a bad check digit, no keyword, order and invoice numbers, phone numbers, an IBAN, card numbers, dates, digit runs, the footer, forwarded content and a number only in an `href`;
  - host hints on a label boundary, with an invented table through `with_data`;
  - ambiguity between two carriers;
  - the conformance suite with `personal_mail()` and `hostile_corpus()`.
- `tests/fixtures.rs` over `tests/fixtures/*.eml`, read through `holtmail_mime` and `Runner::input`:
  - `parcel-s10-valid`, `parcel-s10-bad-check`, `parcel-ups-valid` (HTML), `parcel-no-context`;
  - `parcel-usps-valid`, `parcel-fedex-12-valid`, `parcel-fedex-15-valid`, `parcel-dhl-express-valid`, `parcel-russian-post-valid`;
  - `parcel-forwarded`, `parcel-phone-number`;
  - tracking URLs, explanations without values, and the conformance suite.
