# holtmail-auth-results

The grammar of two header fields: `Authentication-Results` (RFC 8601) and `DKIM-Signature` (RFC 6376 tag-lists). It turns one field value into owned structs and nothing more: it verifies no signature, looks up no DNS, decides no trust and knows nothing about which field of a message to read. A plain library with no dependencies, so `holtmail-alias` can use it without pulling in a MIME parser. `holtmail-alias` reads the topmost field with it (`Mail::dkim_pass`, `Mail::dkim_pass_domains`); `holtmail-insights` builds the stored auth facts of [INSIGHTS.md](../../../docs/INSIGHTS.md) §4.4 from it.

## Public API

- `VERSION: &str` — changes whenever the same input could parse differently, so facts derived from it can be recomputed.
- `B_PREFIX_LEN: usize` (8) — how many leading characters of a signature identify it (RFC 6008 `header.b`).

### `parse_auth_results(value: &str) -> Option<AuthResults>`

- Never panics, on any input. Takes the value folded or unfolded (CR, LF, tab and space are all whitespace).
- Comments in parentheses are dropped anywhere, nested or not, except inside quoted strings; a comment that holds `dkim=pass header.d=…` is never read as a result.
- Quoted strings are unquoted (backslash escapes resolved). `;` inside quotes does not split.
- `None` only when there is no authserv-id. Results that do not fit the grammar (`dkim` without `=`, `=pass`) are skipped; the others are kept. `none` gives an empty list.
- Spaces are allowed around `=`, `/` and `.` (`dkim / 1 = pass`, `header . d = …`), as CFWS is in the RFC.

`AuthResults { authserv_id, version, results }`:

- `authserv_id`: lowercase, never empty. `version`: the `authres-version` number when present.
- `results: Vec<MethodResult>` in field order. `MethodResult { method, method_version, result, reason, properties }`: method and result lowercase, `reason` unquoted (the first one), `properties: Vec<Property { ptype, name, value }>` in field order with `ptype` and `name` lowercase and `value` as written. Keys without a dot other than `reason` (Microsoft's `action=none`) are not properties and are dropped.
- `MethodResult::property(ptype, name)`: the first matching value, any letter case.
- `MethodResult::dkim()` / `AuthResults::dkim()`: the DKIM view of `dkim` results, `DkimResult { result, d, i, s, b }`. `d` and `s` lowercase, `i` and `b` as written. Helpers: `is_pass()`, `i_domain()` (after the last `@`, lowercase), `signing_domain()` (`d`, else the domain of `i`), `b_prefix()` (at most 8 characters).

### `parse_dkim_signature(value: &str) -> Option<DkimSignature>`

- Never panics. Reads an RFC 6376 §3.2 tag-list: tag names are case-sensitive (`D=` is not `d=`), whitespace and folding are allowed around tags, a trailing `;` is allowed.
- `None` when the value is not a tag-list: an entry without `=`, a tag name that does not start with a letter or holds other than letters, digits and `_`, or a tag that occurs twice (which makes the whole list invalid).
- Required tags are not checked; a missing `d`, `s` or `b` is `None` in the struct.

`DkimSignature { d, s, b, h, l, tags }`:

- `d`, `s`: trimmed, lowercase. `b`: every whitespace character removed (folding inside the base64), letter case kept.
- `h`: the signed names in order, trimmed and lowercase, as often as listed, so oversigning shows (`from:list-unsubscribe:list-unsubscribe`). `h_counts()` gives a `BTreeMap` name → count; `signed_count(name)` one count, any letter case.
- `l`: whether an `l=` tag is present (anyone may then append to the body).
- `tags`: every tag in order with its value trimmed (`b` and `bh` without whitespace); `tag(name)` reads one.
- `b_prefix()`: at most the first 8 characters of `b`, comparable with `DkimResult::b_prefix()`.

## Tests

- Unit tests next to the code; `tests/fixtures.rs` reads the invented `.eml` files in `tests/fixtures/` (INSIGHTS.md §15) with a tiny local header splitter and checks each parse: a trusted topmost field, a forged field from an untrusted server, a forged field with the trusted id below the real one, comments everywhere, and an `l=` signature.
- `tests/properties.rs` (proptest): arbitrary strings and grammar-shaped noise never panic; generated `Authentication-Results` fields (with comments, quoted values and folding) and `DKIM-Signature` fields (with folding inside `b` and `h`) parse back to what was generated.
- Fuzzing: `fuzz/` is a cargo-fuzz crate outside the workspace (its own empty `[workspace]`, a dated nightly in `fuzz/rust-toolchain.toml`). Run `cargo fuzz run parse` from `crates/auth-results`. The target feeds every input to both parsers and their helpers.

## Limits

- Grammar only. Whether a field can be trusted (its authserv-id, its position) and whether a signature matches a result are the runner's decisions (INSIGHTS.md §4.3).
- Unquoted values end at whitespace, so an unquoted `header.b` that a server folded across lines is cut at the fold; a prefix is all `header.b` carries anyway.
- Version numbers above `u32::MAX` read as absent.
