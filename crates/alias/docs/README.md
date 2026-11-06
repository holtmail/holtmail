# holtmail-alias

The contract for email alias services: SimpleLogin, addy.io, Firefox Relay, DuckDuckGo, Fastmail Masked Email, Apple Hide My Email and anything like them. An `AliasDetector` looks at one message and says whether a service forwarded it, to which alias, and who really sent it. An `AliasDirectory` is one account at a service: it lists the account's aliases and, as far as the service allows, creates, changes and deletes them. No I/O and no provider code live here.

## API

### Detection

```rust
pub trait AliasDetector: Send + Sync {
    fn provider(&self) -> &str;
    fn version(&self) -> String;
    fn detect(&self, mail: &Mail) -> Option<AliasHit>;
}
```

- `Mail { headers, from, to, cc, reply_to }`: every header field topmost first (`Header { name, value }`, unfolded, encoded words decoded) and the parsed address fields. Helpers: `header(name)` (topmost, any letter case), `headers_named(name)`, `recipients()` (`To`, `Cc`, `Delivered-To`, `X-Original-To`, `Envelope-To`, lowercase, each once), `recipient_at(domain)` (a recipient on the domain or below it) and `dkim_pass(domains)`.
- `dkim_pass` reads only the topmost `Authentication-Results`, the one the receiving server adds, and is true when it records `dkim=pass` with `header.d=` (or `header.i=@`) one of the domains or a domain below one of them. Comments in parentheses are ignored (OpenDKIM writes `dkim=pass (2048-bit key; unprotected) header.d=…`). A look-alike such as `evilrelay.example.net` for `relay.example.net` does not count.
- `AliasHit { provider, alias, sender, reverse_alias, signed }`: the alias lowercase; the sender when the service hid it; the reverse alias (lowercase) only when the service gives one per sender; `signed` when the provider's DKIM passed. Without `signed` the forwarding fields may have been written by anyone.
- Guarantees of every detector: deterministic, never panics on malformed fields, `None` for mail without the service's marks. `version()` changes whenever answers for stored mail may change (rules, trusted domains), so callers detect stored mail again.
- `parse_mailbox(value)` reads one mailbox from a header value (`Name <a@example.com>`, `"Name" <…>`, `<…>`, a bare address).

### Directories

```rust
#[async_trait]
pub trait AliasDirectory: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    async fn aliases(&self) -> Result<Vec<RemoteAlias>, AliasError>;
    async fn options(&self) -> Result<AliasOptions, AliasError>;
    async fn create(&self, alias: &NewAlias) -> Result<RemoteAlias, AliasError>;
    async fn update(&self, id: &str, change: &AliasChange) -> Result<RemoteAlias, AliasError>;
    async fn delete(&self, id: &str) -> Result<(), AliasError>;
}
```

- `Capabilities { create_random, create_custom, note, name, for_domain, switch, mailboxes, delete }`: what the directory can do besides listing; the same for its lifetime. The web app shows only the controls a service has.
- `aliases()` lists every alias once, deleted ones not at all. `options()` gives the domains or suffixes for a chosen prefix (default first), the mailboxes aliases can forward to (default first), a prefix hint, whether the account may create now, and checks the key (`Auth` when refused).
- `create` with `prefix: None` lets the service pick; a prefix needs `create_custom`, none needs `create_random`, else `Unsupported`.
- `AliasChange`: `None` leaves a field, an empty string clears it. A field the directory cannot change is `Unsupported` and nothing changes; an unknown id is `NotFound`.
- A call that returns `Ok` is done at the service; a failed call may or may not have reached it, so callers list again.
- `AliasError`: `Auth` (key refused), `Unsupported`, `NotFound`, `Invalid(words for the owner)` (taken prefix, quota), `Unavailable` (try later), `Protocol` (an answer not understood).

## Implementing a provider

1. Create `crates/alias-<name>` depending on `holtmail-alias`, `holtmail-core` and, for an API, `holtmail-json-http`.
2. A detector recognises only its own marks and sets `signed` only from `Mail::dkim_pass` with the service's own domains (configurable for self-hosted instances).
3. Add `holtmail-alias = { features = ["testing"] }` under `[dev-dependencies]` and run the suites:
   - `testing::conformance::detector(&detector, &cases)` with hand-written fixtures (`tests/fixtures/*.eml`, invented addresses), including a forged case;
   - `testing::conformance::directory(&directory).await` against an in-process fake of the service's API on `127.0.0.1`.
4. Register it in `apps/server/src/aliases.rs` (`detectors`, `providers`, `connect`). Nothing else changes.

`testing::FakeDirectory` is an in-memory account (domain `alias.example`, mailboxes `1` and `2`) with chosen capabilities, `seed` and `refuse_key`, for tests of code that uses directories. `testing::mail(&[(name, value)])` builds a `Mail` with its address fields parsed.
