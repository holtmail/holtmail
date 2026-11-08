# holtmail-alias

The contract for email alias services: SimpleLogin, addy.io, Firefox Relay, DuckDuckGo, Fastmail Masked Email, Apple Hide My Email and anything like them. An `AliasDetector` looks at one message and says whether a service forwarded it, to which alias, and who really sent it. An `AliasDirectory` is one account at a service: it lists the account's aliases and, as far as the service allows, creates, changes and deletes them. No I/O and no provider code live here.

## API

### Detection

```rust
pub trait AliasDetector: Send + Sync {
    fn provider(&self) -> &str;
    fn version(&self) -> String;
    fn detect(&self, mail: &Mail) -> Option<AliasHit>;
    fn reply_strategy(&self) -> ReplyStrategy { ReplyStrategy::Direct }
    fn encode_reverse(&self, alias: &str, contact: &str) -> Option<String> { None }
    fn decode_reverse(&self, address: &str) -> Option<String> { None }
}
```

- `Mail { headers, from, to, cc, reply_to }`: every header field topmost first (`Header { name, value }`, unfolded, encoded words decoded) and the parsed address fields. Helpers: `header(name)` (topmost, any letter case), `headers_named(name)`, `recipients()` (`To`, `Cc`, `Delivered-To`, `X-Original-To`, `Envelope-To`, lowercase, each once), `recipient_at(domain)` (a recipient on the domain or below it), `dkim_pass(domains)` and `dkim_pass_domains()`.
- `dkim_pass` reads only the topmost `Authentication-Results`, the one the receiving server adds, and is true when it records `dkim=pass` with `header.d=` (or `header.i=@`) one of the domains or a domain below one of them. Comments in parentheses are ignored (OpenDKIM writes `dkim=pass (2048-bit key; unprotected) header.d=…`). A look-alike such as `evilrelay.example.net` for `relay.example.net` does not count. The field is parsed with [`holtmail-auth-results`](../../auth-results/docs/README.md) (RFC 8601 grammar, no verification).
- `dkim_pass_domains()` lists what `dkim_pass` compares against: every `header.d`, and every `header.i` written as `@domain`, of the `dkim=pass` results in the topmost field, lowercase, each once, in field order. Empty when there is no field or no pass.
- `AliasHit { provider, alias, sender, reverse_alias, signed }`: the alias lowercase; the sender when the service hid it; the reverse alias (lowercase) only when the service gives one per sender; `signed` when the provider's DKIM passed. Without `signed` the forwarding fields may have been written by anyone.
- Guarantees of every detector: deterministic, never panics on malformed fields, `None` for mail without the service's marks. `version()` changes whenever answers for stored mail may change (rules, trusted domains), so callers detect stored mail again.
- `ReplyStrategy` says how a reply to the service's mail leaves without showing the real mailbox ([WRITE.md](../../../docs/WRITE.md#replies-through-relay-mailboxes)): `ReverseAlias` (`To:` the reverse alias, `From:` a mailbox the alias forwards to), `Direct` (`From:` the alias itself, through a transport allowed to send as it), `ReplyHeader` (`To:` the service's `Reply-To`, `From:` the real mailbox). `as_str()` and `parse()` use `reverse_alias`, `direct`, `reply_header`. `reply_strategy()` is the detector's default, the same for its lifetime; without an override it is `Direct`, which never shows the real mailbox and which a transport not allowed to send as the alias refuses. Defaults: SimpleLogin, addy.io, DuckDuckGo, Apple `ReverseAlias`; Firefox Relay `ReplyHeader`; `alias-rules` and aliases without a detector (Fastmail Masked Email, added by hand) `Direct`. Overrides per alias, domain or service belong to the route.
- `encode_reverse(alias, contact)` gives the reverse alias through which `alias` reaches `contact`, lowercase, for services that write the contact into the address (addy.io, DuckDuckGo); `None` for other services, an alias not of the service, or addresses that cannot be encoded. Pure. `decode_reverse(address)` is its mirror: the contact a reverse alias stands for, lowercase, a display hint only (anyone can write an address of that shape). Decoding an encoded address gives the contact back.
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
    fn reverse(&self) -> bool { false }
    async fn reverse_alias(&self, id: &str, contact: &str) -> Result<String, AliasError> { Err(Unsupported) }
}
```

- `Capabilities { create_random, create_custom, note, name, for_domain, switch, mailboxes, delete }`: what the directory can do besides listing; the same for its lifetime. The web app shows only the controls a service has.
- `reverse()` is the `reverse` capability, kept as a method beside `Capabilities` so existing `Capabilities` literals keep compiling; the same for the directory's lifetime. With it, `reverse_alias(id, contact)` gives the reverse alias through which alias `id` reaches `contact` (an address, `Name <…>` accepted), lowercase, made at the service the first time and the same after (idempotent). `NotFound` for an unknown alias, `Invalid` for an address the service refuses or a plan that does not allow it; without the capability `Unsupported`.
- `RemoteAlias.mailboxes` are the mailboxes the alias forwards to, as the service lists them (SimpleLogin mailboxes, addy.io recipients; empty for Firefox Relay and Fastmail, whose APIs do not say). The route picks the sending mailbox for `ReverseAlias` from them.
- `aliases()` lists every alias once, deleted ones not at all. `options()` gives the domains or suffixes for a chosen prefix (default first), the mailboxes aliases can forward to (default first), a prefix hint, whether the account may create now, and checks the key (`Auth` when refused).
- `create` with `prefix: None` lets the service pick; a prefix needs `create_custom`, none needs `create_random`, else `Unsupported`.
- `AliasChange`: `None` leaves a field, an empty string clears it. A field the directory cannot change is `Unsupported` and nothing changes; an unknown id is `NotFound`.
- A call that returns `Ok` is done at the service; a failed call may or may not have reached it, so callers list again.
- `AliasError`: `Auth` (key refused), `Unsupported`, `NotFound`, `Invalid(words for the owner)` (taken prefix, quota), `Unavailable` (try later), `Protocol` (an answer not understood).

## Implementing a provider

1. Create `crates/alias-<name>` depending on `holtmail-alias`, `holtmail-core` and, for an API, `holtmail-json-http`.
2. A detector recognises only its own marks and sets `signed` only from `Mail::dkim_pass` with the service's own domains (configurable for self-hosted instances).
3. Add `holtmail-alias = { features = ["testing"] }` under `[dev-dependencies]` and run the suites:
   - `testing::conformance::detector(&detector, &cases)` with hand-written fixtures (`tests/fixtures/*.eml`, invented addresses), including a forged case; it also checks that `reply_strategy()` is stable, that empty addresses encode and decode to `None`, and that for every hit with a sender `decode_reverse(encode_reverse(alias, sender))` gives the sender back;
   - `testing::conformance::directory(&directory).await` against an in-process fake of the service's API on `127.0.0.1`; with `reverse()` it checks that `reverse_alias` is lowercase, the same for the same contact, different for another, and `NotFound` for an unknown alias, and without it `Unsupported`.
4. Register it in `apps/server/src/aliases.rs` (`detectors`, `providers`, `connect`). Nothing else changes.

`testing::FakeDirectory` is an in-memory account (domain `alias.example`, mailboxes `1` and `2`) with chosen capabilities (`new()` has all of them and `reverse`, `with_capabilities` has no `reverse`), reverse aliases `reverse<n>@alias.example` (`reverse_aliases_made()` counts them), `seed` and `refuse_key`, for tests of code that uses directories. `testing::mail(&[(name, value)])` builds a `Mail` with its address fields parsed.
