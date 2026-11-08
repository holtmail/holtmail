# Email aliases

Alias services give each website its own address and forward its mail to a real mailbox, often rewriting `From` so replies go back through the service. Holtmail recognises such mail, shows who really sent it and to which alias, lists every alias in use with the accounts its mail arrives in, and manages aliases through the services that have an API.

## Services

| Service | Recognised by | Sender from | Reverse alias | API |
| --- | --- | --- | --- | --- |
| SimpleLogin, Proton Pass | `X-SimpleLogin-Type: Forward`, alias in `X-SimpleLogin-Envelope-To` | `X-SimpleLogin-Original-From`, else the rewritten name `Name - ada at example.com` | the rewritten `From` | list, create random or with a suffix, note, name, mailboxes, switch, delete |
| addy.io | `X-AnonAddy-Original-Sender` or `Feedback-ID: …:anonaddy` | `X-AnonAddy-Original-From-Header`, else the reverse alias | `alias+sender=example.com@domain` in `From` or `Reply-To` | list, create random or with a local part, description, name, recipients, switch, delete |
| Firefox Relay | `From` name ending in `[via Relay]` on a mask domain | `Resent-From` | none (one reply address) | list, create random, label, website, switch, delete |
| Fastmail Masked Email | delivered unchanged; matched by recipient | `From` | none | JMAP: list, create (optional prefix), description, website, switch, delete |
| DuckDuckGo | `From` `sender_at_example.com_user@duck.com` and a Duck recipient | the reply address | the reply address | none: add by hand |
| Apple Hide My Email, Sign in with Apple | `X-Icloud-Hme: p=…; s=…`, or a recipient on `privaterelay.appleid.com` | `s=` | Apple's rewritten `From` | none: add by hand |
| Own domains, other forwarders | `[[aliases.rules]]` in the config | a configured header | none | none |

## Modules

- `holtmail-alias`: the contract. `AliasDetector` (pure, never fails, `version()` for re-detection, a default `reply_strategy()`, `encode_reverse` and `decode_reverse` for services that write the sender into the address) and `AliasDirectory` (list, options, create, update, delete, with `Capabilities`, and the optional `reverse` capability with `reverse_alias`). Conformance suites for both and a `FakeDirectory`.
- One crate per service: `alias-simplelogin`, `alias-addy`, `alias-relay` (detector and directory), `alias-fastmail` (directory), `alias-duck`, `alias-apple` (detector), `alias-rules` (detectors from the config).
- `holtmail-json-http`: JSON over HTTPS for the directories.
- `holtmail-aliases`: services connected to archives (sealed keys, listing, changes at the service, reverse aliases for new contacts), over the contracts; the server gives it the providers, a `Connect` function and the detectors.
- `apps/server/src/aliases.rs`: the only place that names the implementations (`detectors`, `providers`, `connect`), the `[aliases]` config, the listing schedule and the start-up detection pass.

A new service is a new crate that passes the conformance suites and three lines in `apps/server/src/aliases.rs`.

## Data

- `message_aliases`: what detection found per message (provider, alias, sender, reverse alias, signed). Derived from the raw bytes; replaced by `Ingest::refresh_alias`, removed with the message.
- `aliases`: known aliases, listed by a service (`service_id`, `remote_id`, switch, note, name, website, mailboxes, the service's counts) or added by hand.
- `alias_services`: provider, name, address, the key sealed for `alias-service:<id>`, the last listing and its error.
- `resolved_addresses` (a view): the addresses of mail with each reverse alias replaced by the sender it stands for. People read it, so a reverse alias counts as its sender; aliases count as the owner's own addresses.

The alias list of an archive merges known aliases with aliases seen in mail, with the messages of each (forwarded to it, or addressed to a known alias), distinct senders, signed messages, first and last dates and the accounts the mail was archived from.

## Trust

Anyone can write `X-SimpleLogin-*` fields. A hit is `signed` only when the topmost `Authentication-Results` (added by the receiving server) records a passing DKIM signature of the provider's own domain. The reader marks a hit that is neither signed nor a known alias as not verified, and the alias list counts unsigned messages of aliases only seen in mail. Self-hosted services add their domains in `[aliases]`.

## Replies

How a reply to forwarded mail leaves is a `ReplyStrategy` ([WRITE.md](WRITE.md#replies-through-relay-mailboxes)); each detector has a default and the route may override it per alias, domain or service:

| Service | Strategy | Reverse alias for a new contact |
| --- | --- | --- |
| SimpleLogin, Proton Pass | `reverse_alias`, from one of the alias's mailboxes (SimpleLogin lists them) | the contacts API (`reverse_alias` on the directory) |
| addy.io | `reverse_alias`, from one of the alias's recipients | `encode_reverse`: `alias+contact=domain@alias-domain` |
| DuckDuckGo | `reverse_alias` | `encode_reverse`: `contact_at_domain_user@duck.com` |
| Firefox Relay, Apple | `reply_header` | none |
| Fastmail, own domains, rules, aliases added by hand | `direct` | not needed |

`AliasManager::reverse_alias(archive, alias, contact)` picks the directory when it has the capability, else the detector of the alias's provider. Results are not cached yet.

## Search and the reader

The hidden sender is indexed as `From` too, so `from:ada@example.com` finds forwarded mail; `to:<alias>` finds an alias's mail. The reader shows the real sender in place of the reverse alias, a line with the alias, the service, whether it is signed or known, its note and the reverse alias replies go through. Replies still go to `From`, the reverse alias, as the services expect.

## Testing

- Unit: every detector against hand-written `.eml` fixtures with invented addresses, including forged fields and look-alike domains; every directory against an in-process fake of the service's API with the conformance suite; the store conformance suite (aliases, people through reverse aliases, removal); the manager against SQLite and `FakeDirectory`; the API end to end with a forwarded message.
- Integration: `tests/integration/tests/aliases.rs` with `IT_PROFILES=aliases` runs a self-hosted SimpleLogin, a Postfix with OpenDKIM and a CoreDNS that publishes SimpleLogin's DKIM key; aliases are managed through Holtmail and checked in SimpleLogin's API, and forwards arrive at Holtmail's SMTP listener with a real, checked signature. addy.io could join the same way.
- By hand ([VERIFY](VERIFY.md)): one real forwarded message per service, compared with the fixtures; Apple's and DuckDuckGo's headers above all.
