# holtmail-aliases

Alias services connected to archives: their keys sealed in the store, their aliases listed into the store, and aliases created, changed and deleted at the service. A library over the `holtmail-alias`, `holtmail-store` and `holtmail-secret` contracts; which services exist and how a directory is built for one is given by the caller, so only the composition root knows the implementations.

## API

```rust
let manager = AliasManager::new(store, secrets, providers, connect);
```

- `Connect`: `Fn(provider, base_url, key) -> Result<Arc<dyn AliasDirectory>, AliasError>`. `ProviderInfo { id, name, default_url, capabilities, key_label, key_help, detects }`; a provider without `capabilities` has no API.
- `add_service(archive, provider, name, base_url, key)`: checks the key with `options()` first and stores nothing when that fails; seals the key for `alias-service:<id>`; an empty name becomes the provider's; lists the aliases once (a failure there is kept as `sync_error`).
- `update_service(id, name, base_url, key)`: a new address or key is checked before it is kept, then listed again.
- `sync_service(id)`: lists the aliases into the store (`replace_service_aliases`); a failure is kept as the service's `sync_error` and returned. `sync_all()` does every service and returns the archives that changed and how many failed.
- `options(id)`: capabilities and choices for new aliases.
- `create_alias(id, new)`, `update_alias(archive, address, change)`, `delete_alias(archive, address, at_service)`: done at the service first, then stored. An alias added by hand keeps only a note and a website; deleting at the service accepts an alias the service no longer has.
- `ManageError`: `Invalid(words for the owner)` from `explain(&AliasError)`, or `Store`. Keys never appear in errors or logs.

## Tests

Against SQLite and `FakeDirectory`: a refused key stores nothing, a provider without an API is refused, listing, creating with a prefix, switching off and moving mailboxes at the service, deleting at the service, a key refused later kept as `sync_error`, aliases added by hand, and a new key checked before it is kept.
