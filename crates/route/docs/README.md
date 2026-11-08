# holtmail-route

Decides how one draft is sent: the identity it comes from, the exact `From` address, the transport that carries it and the strategy, with structured reasons the compose window shows ("From ada@example.com through Fastmail SMTP, because the original was sent to that address"). A pure library over its own input types: no store, no I/O. The caller loads identities and transports and passes them in. Design: [WRITE.md](../../../docs/WRITE.md#identities-transports-routes).

## Public API

- `route(Request) -> Result<Route, RouteError>`: deterministic, the same request always gives the same answer.
- `Request { identities, transports, context, default_identity, chosen }`.
- `Identity { id, address, name, transport, account, alias, active, plus_addresses }`: `account` and `alias` link the identity to a synced account or an alias address; inactive identities are never picked, not even when chosen.
- `Transport { id, allowed_from }` and `Transport::allows(address)`: any pattern covers the address; an empty list allows nothing.
- `Context::New`, `Context::Reply(Original)`, `Context::Forward(Original)`; a forward is routed like a reply.
- `Original { recipients, account, alias }`: `recipients` are the addresses the original was delivered to, in order `To`, `Cc`, `Delivered-To`, `X-Original-To`; `alias` is an `AliasHit { provider, alias, reverse_alias, sender }` mirroring `holtmail_alias::AliasHit`.
- `Route { identity, from, name, transport, strategy, reasons }`.
- `Strategy::Direct`, `ReverseAlias`, `ReplyHeader`, serialized `snake_case`.
- `Reason` serialized as `{"code": ...}`: `chosen`, `original_recipient {address}`, `plus_address {address}`, `alias {alias}`, `account {account}`, `default`, `plus_address_dropped {address}`, and for alias routes `alias_mailbox {address}`, `no_alias_mailbox {alias}`, `not_alias_mailbox {address}`, `alias_from {alias}`, `reverse_alias {address}`, `computed_reverse_alias {address}`, `unsigned_reverse_alias {address}`, `reply_header {address}`. The first reason is the rule that picked the identity; notes follow.
- `RouteError` serialized as `{"code": ...}`: `no_identity`, `unknown_identity {id}`, `unknown_transport {transport}`, `not_allowed_from {address, transport}`, `needs_reverse_alias {alias, contact}`, `no_reply_header {alias}`, `no_alias_identity {alias}`.
- `allows(pattern, address)` and `valid_pattern(pattern)`.
- `route_relay(Request, Option<&Relay>) -> Result<Routed, RouteError>`: alias strategies, see [Replies through alias services](#replies-through-alias-services).
- `Relay { hit, signed, strategy, mailboxes, reply_to, reverse_alias }`: the original's alias hit, whether the service's DKIM passed, the effective strategy (the detector's default overridden per alias, domain or service: the caller resolves it), the addresses the alias forwards to, the `Reply-To` the service set and a reverse alias the caller computed or created.
- `Routed { route, to, alias }`: `to` replaces the original's sender as the reply's `To` (the reverse alias or the service's `Reply-To`), `alias` is the alias, lowercase.
- `attendee_address(&Routed)`: the alias when there is one, else `From`; the attendee address of an iMIP reply, so an invitation received through an alias is answered as the alias.
- `leaks(&Routed, &Context, Recipients, &Thread) -> Vec<Leak>`: the leak guard, see [Leak guard](#leak-guard).
- `Thread { aliases, reverse_aliases, can_reverse }`, `Recipients { to, cc, bcc }`, `Leak { recipient, field, reason, fix }`; `Field`, `LeakReason` and `Fix` serialize `snake_case`.

## Rules

Among active identities, the first of:

1. The chosen identity (`chosen`). An unknown or inactive choice is `unknown_identity`; it never falls back to the defaults.
2. For a reply or forward, the identity whose address is a recipient of the original (`original_recipient`).
3. The identity with a plus address among the recipients: `ada+shop@example.com` picks `ada@example.com` (`plus_address`). An empty tag (`ada+@`) is not a plus address.
4. The identity linked to the alias the original came through, or to an alias among its recipients (`alias`).
5. The identity linked to the account the original was synced from (`account`).
6. The default identity (`default`). Without one, `no_identity`.

Recipients are tried in their order, then identities in theirs, so an earlier recipient beats a later one and every exact match beats every plus match. Addresses compare lowercase as a whole: local parts are case-sensitive in theory and case-insensitive at every provider Holtmail talks to.

`From` is the identity's address as stored. In rule 3 it is the plus address (lowercase) when the identity has `plus_addresses` and its transport allows that address; otherwise the plain address with a `plus_address_dropped` note.

Then `From` is checked against the transport's `allowed_from`. Outside it the answer is `not_allowed_from`: the route never moves on to another identity to get around the check, and the chosen identity is checked like any other. A missing transport is `unknown_transport`.

`route` always answers `direct` from the identity's own address. Alias strategies come from `route_relay`.

## Replies through alias services

`route_relay` with no `Relay`, or for a new message, is `route` with `to` and `alias` empty. With one, the strategy is `Relay::strategy`:

| Strategy | Identity | `From` | `To` of a reply |
| --- | --- | --- | --- |
| `reverse_alias` | a forwarding mailbox | the identity's address | the reverse alias |
| `direct` | `route`'s rules | the alias | as composed |
| `reply_header` | a forwarding mailbox | the identity's address | the service's `Reply-To` |

- **Forwarding mailbox.** The first active identity whose address is one of `Relay::mailboxes`, in the service's order (`alias_mailbox`), because services such as SimpleLogin accept replies only from the alias's mailboxes. Without one: the first active real mailbox of the original's account (`account`), then the default identity if it is a real mailbox (`default`), each followed by the note `no_alias_mailbox`; with neither, `no_identity`. A real mailbox is an identity with no linked alias whose address is not the alias. A chosen identity wins, with `not_alias_mailbox` when it is not one of the mailboxes. The address is checked against the transport's `allowed_from`.
- **Direct.** The identity `route`'s rules pick (chosen, recipient, plus address, linked alias, account, default) among the active identities that send as the alias: whose address is the alias or that are linked to it. With none, or a chosen identity that is not one of them, the answer is `no_alias_identity {alias}`, so a shared transport whose `allowed_from` covers the alias never lets someone without an identity for it send as it (the identity grant stays the lock). `From` is the alias, lowercase, with `alias_from` when it differs from the identity's address. Only the alias is checked against `allowed_from`: an identity whose own address the transport may not use can still send as its alias.
- **Reverse alias.** The hit's `reverse_alias` when `signed` (`reverse_alias`), else `Relay::reverse_alias` (`computed_reverse_alias`). An unsigned hit's reverse alias may have been written by anyone, so it is never a recipient (`unsigned_reverse_alias`). With no usable reverse alias the answer is `needs_reverse_alias {alias, contact}`: the caller creates one through the alias directory or the detector's `encode_reverse` and asks again. `contact` is the hit's sender, or `None` when the service did not rewrite `From` and the original's `From` is the contact.
- **Reply header.** `Relay::reply_to` when `signed` (`reply_header`); missing or unsigned is `no_reply_header {alias}`.
- **Forward.** The identity and `From` follow the strategy, `to` stays `None`; the leak guard checks the recipients.

All addresses in reasons, errors, `to` and `alias` are lowercase.

## Leak guard

`leaks` lists the recipients that would see the real mailbox. Nothing leaks when `Thread::aliases` is empty and the route names no alias (`Routed::alias` is `None`), or when `From` is one of the thread's aliases or the routed alias, whatever the strategy. Otherwise `From` is a real mailbox, and every recipient that is neither a reverse alias (one of `Thread::reverse_aliases` or `Routed::to`) nor one of the thread's aliases leaks, once, in the first field it appears in (`To`, `Cc`, `Bcc`), in the recipients' order.

| Reason | When |
| --- | --- |
| `forward_from_real_address` | the draft is a forward |
| `cc_not_reverse_alias` / `bcc_not_reverse_alias` | the recipient is in `Cc` / `Bcc` |
| `mixed_recipients` | in `To`, next to a reverse alias (reply all) |
| `real_address_from` | in `To`, with no reverse alias at all |

The fix is `use_alias_identity` under `direct` (send as the alias), `use_reverse_alias` when `Thread::can_reverse` (the directory can create a reverse alias or the detector encodes one), and `remove_recipient` otherwise. Callers block the send while any leak remains and offer the fix.

## `allowed_from` patterns

| Pattern | Covers | Does not cover |
| --- | --- | --- |
| `ada@example.com` | `ada@example.com`, `ADA@Example.com` | `ada+shop@example.com`, `ada@mail.example.com` |
| `*@example.com` | every address on `example.com` | `ada@mail.example.com`, `ada@badexample.com` |
| `*@*.example.com` | every address on a subdomain at any depth | `ada@example.com` |

Matching ignores case and surrounding whitespace. Anything else (`*`, `*@*`, `a*@example.com`, an empty domain label) is not `valid_pattern` and covers nothing; stores and the API refuse such patterns with `valid_pattern`.
