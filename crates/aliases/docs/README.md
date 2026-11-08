# holtmail-aliases

Alias services connected to archives: their keys sealed in the store, their aliases listed into the store, and aliases created, changed and deleted at the service. A library over the `holtmail-alias`, `holtmail-store` and `holtmail-secret` contracts; which services exist and how a directory is built for one is given by the caller, so only the composition root knows the implementations.

## API

```rust
let manager = AliasManager::new(store, secrets, providers, connect).with_detectors(detectors);
```

- `Connect`: `Fn(provider, base_url, key) -> Result<Arc<dyn AliasDirectory>, AliasError>`. `ProviderInfo { id, name, default_url, capabilities, key_label, key_help, detects }`; a provider without `capabilities` has no API.
- `add_service(archive, provider, name, base_url, key)`: checks the key with `options()` first and stores nothing when that fails; seals the key for `alias-service:<id>`; an empty name becomes the provider's; lists the aliases once (a failure there is kept as `sync_error`).
- `update_service(id, name, base_url, key)`: a new address or key is checked before it is kept, then listed again.
- `sync_service(id)`: lists the aliases into the store (`replace_service_aliases`); a failure is kept as the service's `sync_error` and returned. `sync_all()` does every service and returns the archives that changed and how many failed.
- `options(id)`: capabilities and choices for new aliases.
- `create_alias(id, new)`, `update_alias(archive, address, change)`, `delete_alias(archive, address, at_service)`: done at the service first, then stored. An alias added by hand keeps only a note and a website; deleting at the service accepts an alias the service no longer has.
- `with_detectors(detectors)`: the detectors used to encode reverse aliases and to give default reply strategies; without it only directories give reverse aliases and every default is `direct`.
- `effective_strategy(overrides, alias, provider, default) -> EffectiveStrategy { strategy, source }`: pure. The override for the alias address wins, then the one for its domain, then the one for the provider (`StrategyScope::Service`, keyed by provider id), then `default`. Addresses, domains and provider ids compare lowercase; an override naming an unknown strategy is skipped. `StrategySource` is `Alias`, `Domain`, `Service` or `Default` (`as_str`: `alias`, `domain`, `service`, `default`).
- `default_strategy(provider)`: the `reply_strategy` of the first detector of that provider, `direct` without one.
- `reply_strategy(archive, alias, provider)`: `effective_strategy` over the archive's stored overrides with `default_strategy` as the default.
- `can_reverse(archive, alias)`: whether `reverse_alias` can answer for the alias: its service has the `reverse` capability, or a detector of its provider encodes contacts into addresses. Asks no service and makes nothing.
- `reverse_alias(archive, alias, contact)`: the address through which the alias reaches the contact, lowercase, so a first message to someone the alias never heard from leaves through the service. A known alias whose service has the `reverse` capability asks the service (made there the first time, the same after); otherwise the detector of the alias's provider (from the known alias, else from the archive's alias list, which includes aliases only seen in mail) encodes it. `Invalid` when neither can, for an alias neither known nor seen, or when the service refuses. Every answer is recorded with `AliasStore::put_reverse_alias` (contact and address), so People and the reader count the reverse alias as the contact; nothing is cached for the next call, since SimpleLogin answers a known contact again without making a new one.
- `ManageError`: `Invalid(words for the owner)` from `explain(&AliasError)`, or `Store`. Keys never appear in errors or logs.

## Tests

Against SQLite and `FakeDirectory`: a refused key stores nothing, a provider without an API is refused, listing, creating with a prefix, switching off and moving mailboxes at the service, deleting at the service, a key refused later kept as `sync_error`, aliases added by hand, a new key checked before it is kept, and reverse aliases from the service (one per contact), from a detector for an alias added by hand, and refused for a provider without either and for an unknown alias.
