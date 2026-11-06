# holtmail-alias-addy

addy.io, formerly AnonAddy. `AddyDetector` recognises mail addy.io forwarded; `AddyDirectory` manages an account's aliases through the addy.io API.

## Detection

- Forwarded mail carries `X-AnonAddy-Original-Sender` or a `Feedback-ID` ending in `:anonaddy`.
- addy.io writes the reverse alias as `alias+sender=example.com@alias-domain` (the last `=` stands for `@`) into `From`, or into `Reply-To` when the account sends replies there. It gives the alias (`alias@alias-domain`) and the sender. There is no header with the alias itself.
- The sender's name comes from `X-AnonAddy-Original-From-Header`, the address from it or from `X-AnonAddy-Original-Sender`, else from the reverse alias.
- `signed`: DKIM of `addy.io`, `anonaddy.com`, `anonaddy.me` or `addymail.com`, plus `[aliases] addy_signing_domains` (self-hosted, or custom domains that sign as themselves).

## Directory

`AddyDirectory::new(base_url, api_key, roots)`, `DEFAULT_URL` = `https://app.addy.io`. Headers `Authorization: Bearer <key>` and `X-Requested-With: XMLHttpRequest`.

| Call | API |
| --- | --- |
| `aliases` | `GET /api/v1/aliases?page[number]=N&page[size]=100&with=recipients` until `meta.current_page >= meta.last_page`; trashed ones left out |
| `options` | `GET /api/v1/domain-options` (default domain first) and `GET /api/v1/recipients` (verified ones) |
| `create` | `POST /api/v1/aliases` with `domain` (the default when none), `description`, `format` `custom` + `local_part` or `random_characters`, `recipient_ids`; a name is set with a `PATCH` after |
| `update` | `PATCH /api/v1/aliases/{id}` (`description`, `from_name`), `POST /api/v1/alias-recipients` (`alias_id`, `recipient_ids`), `POST /api/v1/active-aliases` / `DELETE /api/v1/active-aliases/{id}` |
| `delete` | `DELETE /api/v1/aliases/{id}` (addy.io keeps it in its trash) |

Capabilities: everything except `for_domain`. `422` answers come back as `Invalid` with addy.io's own words; `429` (the hourly limit on new aliases) as `Unavailable`. Dates are `Y-m-d H:i:s` in UTC.

## Tests

Invented fixtures: a reverse alias in `From` with a signature, one in `Reply-To`, and a plus address without addy.io's fields. The directory runs the conformance suite against an in-process fake, plus paging over 230 aliases, the default domain first and a refused key.
