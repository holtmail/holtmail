# holtmail-access

The one decision for every write: the instance switch, the account's capability, the user's grant on the archive, the scope of the credentials and, for mail on a server that asks for it, the consent of the account's sign-in. The web API calls it today; the mail view, submission, DAV and the scheduler call it as they land ([SERVE.md, Principles](../../../docs/SERVE.md#principles)), so a write cannot be allowed through one door and refused at another. A library over the store contract, not a contract: it does no I/O of its own and holds no state.

## API

- `Access::new(Arc<dyn Store>, WriteSwitches)`; cheap to make per request. `switches()` returns the switches in force.
- `Access::may_write(user, scopes, account, capability) -> Result<Account, AccessError>`: the check before anything changes on a server.
- `Access::writable(user, scopes, &Account) -> Vec<Capability>`: the mailbox capabilities that are usable now.
- `Access::may_send(user, scopes, archive, &Transport)` and `may_send_somehow(user, scopes, archive)`: the check on a send, with and without its transport.
- `Access::may_use_rules(user, scopes, archive)`: the check on rule endpoints.
- `Access::role_in(user, archive) -> Option<GrantRole>`: the owner has every right, an admin reads every archive and writes only with a grant, a member has what the grant says; `None` when the user may not read the archive.
- `decide(switches, role, &capabilities, &scopes, capability) -> Result<(), Denial>`: the decision on values already looked up.
- `WriteSwitches { enabled, send, rules }`, all on by default; `allow(capability)` says whether the switches leave a capability possible at all.
- `Scope` (`read`, `write:mail`, `send:mail`, `write:pim`, `rules`) and `Scopes`: `all()` for a cookie session, `only(..)`, and `of_device(&BTreeSet<DeviceScope>)` for device credentials (a device never has `rules`; `notify` opens no write).
- `write_consent_missing(&Account)`, `writes_mail(&Capabilities)`, `MAIL_WRITE_CAPABILITIES`. `holtmail-mail-writeback` re-exports them.

## Guarantees

- **Fail closed.** Every lock must be open. A store failure is `AccessError::Store` and allows nothing.
- **Order of `decide`.** Switch, capability, grant, scope; the first closed lock is the `Denial` reported. `mail.expunge` also needs `mail.delete`.
- **What a lock needs.** `mail.send`: the `send` switch, a `sender` grant, `send:mail`. `pim.*`: an `editor` grant, `write:pim`. Every other capability: an `editor` grant, `write:mail`.
- **Not told what you may not know.** `may_write` on an unknown account, or one in an archive the user may not read, is `NotFound("account")`; `may_send` and `may_use_rules` without any right on the archive are `NotFound("archive")`. `may_send_somehow` reports no grant as `Denial::Grant`, because it is asked before an archive is shown.
- **Consent.** `may_write` of a mailbox capability on a Microsoft Graph account signed in with OAuth without `Mail.ReadWrite` is `ConsentMissing`, once every lock is open; `mail.send` and `pim.*` do not need it. `writable` lists nothing for such an account and for import accounts.
- **Always current.** Nothing is cached; a grant or capability change applies to the next call.

Errors are `AccessError::{NotFound(&str), Denied(Denial), ConsentMissing, Store(StoreError)}`; `Denial::message()` has a sentence for people.
