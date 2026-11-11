# holtmail-client-setup

Renders client configuration for a Holtmail account from typed inputs: an Apple `.mobileconfig` profile with IMAP, SMTP submission, CalDAV and CardDAV ("People") accounts, Thunderbird autoconfig (`config-v1.1.xml`), the DAVx5 hand-off link, and the SRV and TXT records an operator puts in their own mail domain. A pure library: no I/O, no clock, no randomness, so the same input always renders the same bytes. `apps/server` fills the inputs from its config (`src/client_setup.rs`), the API serves the results (the one-time profile link `POST /api/devices/{id}/profile`, autoconfig at `/.well-known/autoconfig/mail/config-v1.1.xml` and `/mail/config-v1.1.xml`, the DAVx5 link in `/api/devices/setup`), and `holtmail dns` prints the records ([DEPLOY.md](../../../docs/DEPLOY.md)). The design is in [SERVE.md](../../../docs/SERVE.md#setting-up-a-client).

## Public API

| Item | Guarantees |
| --- | --- |
| `Server` | What the instance offers and under which names: `name`, `mail_host` (IMAP and submission), `web_host`, `web_port`, `web_tls` (JMAP and DAV), `imap` and `submission` `Ports`, `jmap`, `dav`. Hosts must be ASCII DNS names (letters, digits, hyphens, dots); IP addresses and IDN U-labels are refused as `Invalid` |
| `Server::dav_url`, `Server::jmap_url` | `https://<web_host>[:port]/dav/` and `.../.well-known/jmap`; the port is left out when it is the scheme's default |
| `Ports` | `tls` and `starttls` ports; `preferred()` is implicit TLS when offered (RFC 8314), otherwise STARTTLS, as `(port, Security)`; `offered()` when either is set |
| `Security` | `Tls` (implicit TLS) or `StartTls`, how a preferred port is secured |
| `DAV_PATH` | `/dav/`, where DAV discovery starts in profiles, autoconfig, the DAVx5 link and the TXT records |
| `Account` | `login` (the email address the device signs in with, an ASCII domain) and `display_name` |
| `Profile` | `instance` and `device` ids, the `account`, the device's `app_password`, and which of `mail`, `calendars`, `contacts` to set up |
| `mobileconfig(&Server, &Profile)` | An unsigned XML property list. Mail uses the preferred IMAP and submission ports with `UseSSL` true (Apple negotiates STARTTLS on a STARTTLS port) and the outgoing password the same as the incoming one; CalDAV and CardDAV point at `dav_url`, and the CardDAV account is named "People". Every `PayloadUUID` is a UUIDv8 from BLAKE3 over the instance id, device id and payload kind, and every `PayloadIdentifier` is `<reversed web host>.holtmail.<profile uuid>[.mail|.caldav|.carddav]`, so rendering again for the same device gives a profile that replaces the installed one, and two devices or two instances never collide. Names, passwords and addresses are XML-escaped; characters XML 1.0 cannot carry are dropped. The output holds the app password |
| `autoconfig(&Server, domain)` | Thunderbird `config-v1.1` for addresses at `domain`: every offered IMAP and submission port, implicit TLS first, `%EMAILADDRESS%` as the login and `password-cleartext`; `addressBook` (CardDAV) and `calendar` (CalDAV) with `dav_url` when DAV is offered. Holds no secret and nothing about a user |
| `davx5_link(&Server, login)` | `davx5://<percent-encoded login>@<web_host>[:port]/dav/`. DAVx5 opens `davx5://` as `https`, so the link is refused with `NotOffered("https")` when the web address is not https, and with `NotOffered("dav")` when DAV is not offered; a login that is not an address is `Invalid("login")`. Never carries the app password: links end up in browser history, logs and screenshots |
| `dns_records(&Server, domain)` | `DnsRecord`s in a fixed order: RFC 6186 and RFC 8314 `_imaps`, `_imap`, `_submissions`, `_submission` (implicit TLS priority 0, STARTTLS 10; a port that is not offered points at `.`), `_pop3s` and `_pop3` at `.`; RFC 8620 `_jmap._tcp` when JMAP is offered over `https`; RFC 6764 `_caldavs` and `_carddavs` (`_caldav`, `_carddav` over plain `http`) with a `path=/dav/` TXT record when DAV is offered. Every TTL is `DNS_TTL` (3600) |
| `zone_text(&[DnsRecord])` | Zone-file lines, names padded to one column; `DnsRecord` also displays as one line |
| `SetupError` | `Invalid(field)` for malformed input (a host that is not a DNS name, a login without `@`, port 0, control characters), `NotOffered(service)` when the output needs a service the server does not offer (`imap`, `submission`, `dav`, `https`), `Empty` when a profile asks for nothing |

## Decisions and limits

- **No signing.** Profiles are unsigned and show "Not Verified" on iOS; signing needs a certificate the operator holds and is left to `apps/server` if ever wanted.
- **Mail needs both IMAP and submission** in a profile, because Apple's mail payload has no account without an outgoing server. A device without `send:mail` still gets the submission server; sign-in there is refused.
- **DAV discovery starts at `/dav/`**, whose `PROPFIND` names the principal, so the profile and the link need no user segment.
- **No JMAP in autoconfig or profiles:** neither format has a JMAP entry clients read. JMAP clients use `jmap_url` or the `_jmap._tcp` record.
- **Autoconfig and SRV only help when the operator controls the mail domain.** Thunderbird asks `https://autoconfig.<domain>/mail/config-v1.1.xml` and `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`; the API answers both paths for the configured mail domains; pointing `autoconfig.<domain>` at Holtmail also needs a certificate for that name in the reverse proxy. That CNAME is not a record this crate emits.

## Tests

`cargo test -p holtmail-client-setup`: golden files in `tests/golden/` (a full profile, a STARTTLS profile with escaping, full and minimal autoconfig, three DNS sets) with invented hosts on `example.com` and `example.org`; `HOLTMAIL_BLESS=1` rewrites them. Unit tests cover stable and distinct payload UUIDs, well-formed XML, refusals and DAVx5 links.
