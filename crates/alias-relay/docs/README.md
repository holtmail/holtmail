# holtmail-alias-relay

Firefox Relay. `RelayDetector` recognises mail Relay forwarded; `RelayDirectory` manages an account's masks through the Relay API.

## Detection

- Relay drops the original header fields and sets `From` to the mask with a name ending in `[via Relay]` (`"Ada <ada@example.com> [via Relay]" <mask@mozmail.com>`), and `Resent-From` to the sender.
- The alias is the `From` address, when it is on `mozmail.com`, `relay.firefox.com` (or below them) or `[aliases] relay_domains`. The sender is `Resent-From`, named from the display name when it repeats the same address.
- Replies go to one address for every sender (`replies@relay.firefox.com`), so there is no reverse alias.
- `signed`: DKIM of a mask domain.

## Directory

`RelayDirectory::new(base_url, api_key, roots)`, `DEFAULT_URL` = `https://relay.firefox.com`. Header `Authorization: Token <key>`. Lists are plain arrays, not paged.

| Call | API |
| --- | --- |
| `aliases` | `GET /api/v1/relayaddresses/` (ids `r:<id>`) and `GET /api/v1/domainaddresses/` (premium masks on the account's subdomain, ids `d:<id>`; refused or missing for free accounts and then left out) |
| `options` | `GET /api/v1/relayaddresses/` to check the key |
| `create` | `POST /api/v1/relayaddresses/` with `description`, `generated_for`, `enabled` |
| `update` | `PATCH …/{id}/` with `enabled`, `description`, and `generated_for` (random masks) or `used_on` (subdomain masks) |
| `delete` | `DELETE …/{id}/` |

Capabilities: `create_random`, `note`, `for_domain`, `switch`, `delete`. Masks with a chosen name need the account's subdomain and are not created from Holtmail.

## Tests

Invented fixtures: a named sender with a signature, a sender without a name on a subdomain mask, and the words `[via Relay]` on another domain. The directory runs the conformance suite against an in-process fake, plus both kinds of masks, their ids and a refused key.
