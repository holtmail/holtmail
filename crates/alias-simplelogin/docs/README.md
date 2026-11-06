# holtmail-alias-simplelogin

SimpleLogin, and Proton Pass aliases, which run on it. `SimpleLoginDetector` recognises mail SimpleLogin forwarded; `SimpleLoginDirectory` manages an account's aliases through the SimpleLogin API.

## Detection

- Forwarded mail carries `X-SimpleLogin-Type: Forward`; replies and bounces (other types) are not forwards. The alias is `X-SimpleLogin-Envelope-To`.
- The sender is `X-SimpleLogin-Original-From` when the account adds it (an account setting), else the name SimpleLogin gives the rewritten `From` (`Name - ada at example.com`, `ada at example.com`, `Name - ada(a)example.com`), else `X-SimpleLogin-Envelope-From`. With the account's `NAME_ONLY` or `NO_NAME` sender format and neither header, the address of the sender is unknown.
- The rewritten `From` address is the reverse alias (one per sender and alias).
- `signed`: DKIM of `simplelogin.co` or `simplelogin.com`, plus `[aliases] simplelogin_signing_domains` for a self-hosted instance.

## Directory

`SimpleLoginDirectory::new(base_url, api_key, roots)`, `DEFAULT_URL` = `https://app.simplelogin.io`. The key goes in the `Authentication` header (no `Bearer`).

| Call | API |
| --- | --- |
| `aliases` | `GET /api/v2/aliases?page_id=N` from 0, 20 per page, until a short page |
| `options` | `GET /api/v5/alias/options` (suffixes: `signed_suffix` is the domain id, `suffix` the label) and `GET /api/v2/mailboxes` (verified ones, default first) |
| `create`, random | `POST /api/alias/random/new` with the note; a name or mailboxes are set with a `PATCH` after |
| `create`, custom | `POST /api/v3/alias/custom/new` with `alias_prefix`, `signed_suffix`, `mailbox_ids` (the default mailbox when none), `note`, `name` |
| `update` | `PATCH /api/aliases/{id}` (`note`, `name`, `mailbox_ids`), `POST /api/aliases/{id}/toggle` when the switch differs |
| `delete` | `DELETE /api/aliases/{id}` |

Capabilities: everything except `for_domain`. A signed suffix expires: a `412` comes back as `Invalid`, and the web app reads the options again. `429` (50 requests a minute) is `Unavailable`.

## Tests

Invented fixtures in `tests/fixtures/`: with `Original-From` and a signature, with only the rewritten name (encoded words), forged fields signed by another domain, and a reply. The directory runs the conformance suite against an in-process fake of the API, plus paging over 45 aliases and a refused key.
