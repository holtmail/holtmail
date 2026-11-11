# Deploying Holtmail

How to put an instance on the internet so browsers, mail apps, calendar apps and contacts apps reach it: ports, TLS, proxies, the firewall, DNS records and backups. The configuration keys are explained in the [server docs](../apps/server/docs/README.md); the design behind serving is in [SERVE.md](SERVE.md).

## Ports

| Port | What | Config | Who connects |
| --- | --- | --- | --- |
| 8080 (behind a proxy on 443) | Web app, API, JMAP (`/jmap`, `/.well-known/jmap`), CalDAV and CardDAV (`/dav`, `/.well-known/caldav`, `/.well-known/carddav`), autoconfig, profile links | `listen`, `public_url`, `[serve.jmap]`, `[serve.dav]` | Browsers, JMAP clients, calendar and contacts apps |
| 993 | IMAP with implicit TLS | `[serve.imap] listen_tls` | Mail apps |
| 143 | IMAP with STARTTLS | `[serve.imap] listen_starttls` | Mail apps that cannot use 993 |
| 465 | SMTP submission with implicit TLS | `[serve.submit] listen_tls` | Mail apps sending |
| 587 | SMTP submission with STARTTLS | `[serve.submit] listen_starttls` | Mail apps sending |
| 2525 (any) | Inbound SMTP for journal and BCC copies into the archive | `[smtp] listen` | Your own mail servers only |

Every protocol listener is off until its section exists. Prefer the implicit TLS ports (RFC 8314); offer the STARTTLS ports only for clients that need them. Holtmail never delivers to recipients' MX hosts, so port 25 is not needed for sending.

A minimal configuration for an instance at `mail.example.com` serving `@example.com` addresses:

```toml
listen = "127.0.0.1:8080"
public_url = "https://mail.example.com"

[serve]
tls_cert = "/etc/holtmail/fullchain.pem"
tls_key = "/etc/holtmail/privkey.pem"
mail_domains = ["example.com"]

[serve.imap]
listen_tls = ["0.0.0.0:993"]

[serve.submit]
listen_tls = ["0.0.0.0:465"]
listen_starttls = ["0.0.0.0:587"]

[serve.jmap]

[serve.dav]
```

## TLS

- **HTTP** (web app, JMAP, DAV) is plain HTTP on `listen`. Terminate TLS in a reverse proxy, pass `Host` unchanged, set `X-Forwarded-Proto`, and set `public_url` to the https address. JMAP and DAV clients may sign in only over https per `public_url`, or from loopback.
- **IMAP and submission** terminate TLS in Holtmail with `[serve] tls_cert` and `tls_key` (TLS 1.2 and 1.3). Holtmail does not run ACME: let certbot, Caddy or acme.sh write the files, and Holtmail picks up a renewed certificate within a minute without a restart (open connections keep the old one). The certificate must name the host clients use: `[serve.imap] hostname` and `[serve.submit] hostname`, by default the host of `public_url`.
- A listener without a certificate is refused at start, unless it only listens with STARTTLS on loopback.

## Behind a TCP proxy with PROXY v2

The IMAP and submission ports can sit behind a TCP proxy, which is useful when one machine's 993 and 465 are shared or when Holtmail runs in a private network. Without the PROXY protocol every client would appear to come from the proxy, so the sign-in limits would count all clients together. With `proxy_protocol = true` Holtmail reads a PROXY v2 header from every connection whose peer is in `trusted_proxies`, and closes any connection from anyone else that starts with one.

```toml
[serve]
proxy_protocol = true
trusted_proxies = ["10.0.0.2"]  # the proxy; also believed for X-Forwarded-For on HTTP
```

Two ways to run it:

- **Pass TLS through** (recommended): the proxy forwards the raw TLS bytes after the PROXY header and Holtmail terminates TLS with its own certificate. Works on implicit TLS and STARTTLS ports.
- **Terminate TLS in the proxy**: the proxy decrypts and sends a PROXY v2 header with the SSL TLV saying the client used TLS. Holtmail counts such a connection as TLS and offers no STARTTLS on it. Use this only on the implicit TLS ports.

### Caddy with the layer4 module

Caddy needs the [`caddy-l4`](https://github.com/mholt/caddy-l4) plugin (`xcaddy build --with github.com/mholt/caddy-l4`). This passes TLS through to Holtmail on 10.0.0.5:

```caddyfile
{
	layer4 {
		:993 {
			route {
				proxy {
					proxy_protocol v2
					upstream 10.0.0.5:993
				}
			}
		}
		:465 {
			route {
				proxy {
					proxy_protocol v2
					upstream 10.0.0.5:465
				}
			}
		}
		:587 {
			route {
				proxy {
					proxy_protocol v2
					upstream 10.0.0.5:587
				}
			}
		}
	}
}

mail.example.com {
	reverse_proxy 10.0.0.5:8080
}
```

The site block terminates HTTPS for the web app, JMAP and DAV, and sets `X-Forwarded-For` and `X-Forwarded-Proto` itself.

### HAProxy

```haproxy
frontend imaps
    mode tcp
    bind :993
    default_backend holtmail_imaps

backend holtmail_imaps
    mode tcp
    server holtmail 10.0.0.5:993 send-proxy-v2

frontend submissions
    mode tcp
    bind :465
    default_backend holtmail_submissions

backend holtmail_submissions
    mode tcp
    server holtmail 10.0.0.5:465 send-proxy-v2

frontend submission
    mode tcp
    bind :587
    default_backend holtmail_submission

backend holtmail_submission
    mode tcp
    server holtmail 10.0.0.5:587 send-proxy-v2
```

To terminate TLS in HAProxy instead, bind with `ssl crt /etc/haproxy/mail.pem` and use `send-proxy-v2-ssl`, which adds the SSL TLV; keep that to the implicit TLS ports. For HTTPS, a usual `mode http` frontend with `option forwardfor` and `http-request set-header X-Forwarded-Proto https` in front of port 8080.

## Firewall

- Open 443 (and 80 for ACME redirects) to everyone who uses the web app, JMAP or DAV.
- Open 993, 465 and 587 (143 only if offered) to the networks your mail apps connect from. The new Outlook for Windows and Gmail's "other accounts" connect from Microsoft's and Google's servers, so they need these ports open to the internet.
- Keep 8080 closed to everything but the reverse proxy, and the protocol ports closed to everything but the TCP proxy when one is used: Holtmail believes `X-Forwarded-For` and PROXY headers only from `trusted_proxies`, but a closed port is simpler.
- Keep the inbound `[smtp]` port to your own mail servers (its `allow` list is the second fence).
- Holtmail itself connects out to your IMAP, JMAP, CalDAV and CardDAV servers and to the transports you configure for sending.

## DNS records and autoconfig

**These only help in a mail domain you control.** Mail apps look up records and autoconfig under the domain of the address being added: for `ada@example.com` that is `example.com`. If the addresses belong to a provider (Gmail, Fastmail, a company domain you do not run), you cannot publish them there; people enter the servers by hand from Settings → Devices, where every host, port and address is listed, or use a profile or the DAVx5 link.

For your own domains, list them in `[serve] mail_domains` and print the records:

```console
$ holtmail dns
; example.com
_imaps._tcp.example.com.       3600 IN SRV 0 1 993 mail.example.com.
_imap._tcp.example.com.        3600 IN SRV 0 0 0 .
_submissions._tcp.example.com. 3600 IN SRV 0 1 465 mail.example.com.
_submission._tcp.example.com.  3600 IN SRV 10 1 587 mail.example.com.
_pop3s._tcp.example.com.       3600 IN SRV 0 0 0 .
_pop3._tcp.example.com.        3600 IN SRV 0 0 0 .
_jmap._tcp.example.com.        3600 IN SRV 0 1 443 mail.example.com.
_caldavs._tcp.example.com.     3600 IN SRV 0 1 443 mail.example.com.
_caldavs._tcp.example.com.     3600 IN TXT "path=/dav/"
_carddavs._tcp.example.com.    3600 IN SRV 0 1 443 mail.example.com.
_carddavs._tcp.example.com.    3600 IN TXT "path=/dav/"
```

- RFC 6186 and RFC 8314 records for IMAP and submission (implicit TLS before STARTTLS; a service not offered points at `.`, and POP3 is never offered), RFC 8620 `_jmap._tcp` when JMAP is served over https, RFC 6764 records for CalDAV and CardDAV with the `/dav/` path.
- The ports are the ones Holtmail listens on. When a proxy maps ports, write the records by hand with the public ports.
- Also publish `/.well-known/caldav`, `/.well-known/carddav` and `/.well-known/jmap` on `https://example.com` if that is a different web server: a redirect to `https://mail.example.com/...` is enough.

**Thunderbird autoconfig.** Holtmail answers `/.well-known/autoconfig/mail/config-v1.1.xml` and `/mail/config-v1.1.xml` for the domains in `mail_domains` only, with the IMAP, submission and DAV servers; any other domain gets `404`. Thunderbird asks two places for `ada@example.com`:

- `https://autoconfig.example.com/mail/config-v1.1.xml`: point `autoconfig.example.com` at the reverse proxy (a CNAME to `mail.example.com` works), add the name to the proxy's certificate, and pass the request to Holtmail with its `Host`. In Caddy: `autoconfig.example.com { reverse_proxy 10.0.0.5:8080 }`.
- `https://example.com/.well-known/autoconfig/mail/config-v1.1.xml?emailaddress=...`: if `example.com` is served elsewhere, proxy or redirect that one path to Holtmail.

## Setting up clients

People set up their apps from **Settings → Devices** in the web app (also in the installed app): make a device, then copy the app password and the servers, download a **profile** for iPhone, iPad or Mac (a one-time link, valid for 10 minutes, that gives the device a fresh app password; iOS shows unsigned profiles as "Not Verified"), or open **DAVx5** on Android with the server filled in. The page also says what clients cannot do: iOS Mail polls accounts like this one every 15 minutes, so Web Push from the installed web app is the way to be notified; cloud-synced clients store the app password with Microsoft or Google; and a device should use Holtmail or the upstream account, not both.

## Backups

`holtmail backup` writes the database snapshot, every blob and optionally `secret.key` into one tar file while `serve` keeps running; `holtmail restore` reads it into an empty data directory. Details are in [the server docs](../apps/server/docs/README.md#back-up-and-move-an-instance).

The database carries more than derived data: devices and their app password hashes, the served tables (`served_mailboxes`, served UIDs and tombstones), subscriptions, local flags, held mail and item changes, raw drafts, the schedule inbox and the instance token. A restored instance answers mail apps with the same `UIDVALIDITY` and UIDs, so clients need no resync, and existing app passwords keep working. Losing the served tables never loses mail: clients resync once. Profile links are kept in memory only and do not survive a restart or a restore; ask for a new one.

Keep `secret.key` apart from the backups, or pass `--include-secret-key` and store the file like the key.
