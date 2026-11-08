# holtmail-serve-net

The TCP listener every protocol front end (`serve-imap`, `serve-submit`) stands on: it accepts sockets, believes PROXY v2 headers only from trusted proxies, enforces connection limits, does implicit TLS and STARTTLS with rustls, and picks up renewed certificates. It speaks no protocol; a front end implements `ConnectionHandler` and gets a finished `Connection`. A library, not a contract: `apps/server` builds the options from `[serve]`. Design in [SERVE.md](../../../docs/SERVE.md#listeners-and-tls).

## API

- `serve(TcpListener, ListenerOptions, ConnectionLimiter, Arc<dyn ConnectionHandler>)` runs until the runtime stops.
- `ListenerOptions { tls, proxy_protocol, trusted_proxies, proxy_header_timeout, tls_handshake_timeout }`; `ListenerOptions::new(tls)` gives no PROXY protocol, 5 s for a header and 10 s for a handshake.
- `TlsMode::{Off, Implicit(TlsCertificates), Starttls(TlsCertificates)}`.
- `ConnectionHandler::handle(&self, Connection)`.
- `Connection` is `AsyncRead + AsyncWrite`, with `info()`, `can_start_tls()`, `start_tls()`, `authenticated(&UserId)` and `place()`. `place()` gives a `LimitPlace` (with its own `authenticated`), for a front end that hands the connection's bytes to another layer, as `serve-submit` does; the place is freed when the connection and every clone are gone.
- `ConnectionInfo { client, server, peer, source, transport }` with `encrypted()`, `loopback()` and `sign_in_allowed()`; `ClientSource::{Direct, Proxy, ProxyLocal}`; `Transport::{Plain, Tls, ProxyTls}`.
- `TrustedProxies::parse(&[entry])` takes IP addresses and CIDR ranges; `contains(ip)`. It is public so the HTTP side can apply the same list to `X-Forwarded-For`.
- `ConnectionLimiter::new(Limits { total, per_user, per_address })`, defaults 200, 20, 10; `open()` counts open connections. `LimitExceeded::{Total, PerUser, PerAddress}`.
- `TlsCertificates::load(cert, key)`, `reload()`, `watch(interval)`, `acceptor()` (the certificate in force now, for a protocol that runs its own `STARTTLS`); `DEFAULT_RELOAD_INTERVAL` is 60 s.
- `NetError::{InvalidProxy, Certificate, TlsUnavailable, Handshake, Limit}`.

## Guarantees

**Order of checks.** Total limit, then the PROXY header when one is due, then the per-address limit on the real client, then the implicit TLS handshake. A refused connection is closed with no reply, as this layer speaks no protocol, and the handler never runs. Time spent on a header or handshake holds a place in the limits.

**PROXY v2.**
- A header is read only with `proxy_protocol` on and from a peer in `trusted_proxies`. Such a peer must send one, within the timeout: a server-speaks-first protocol cannot wait to see whether one is coming, so "maybe a header" is not offered.
- Everyone else is served directly with no wait. If a client's bytes start with a PROXY signature, the read that completes it fails with `InvalidData`, also when `proxy_protocol` is off and also for a client relayed by a trusted proxy, and the front end's session ends. On TLS listeners the handshake fails first.
- A `LOCAL` header is the proxy's own connection: the socket's address stands, `source` is `ProxyLocal`, and `loopback()` is `false` even on a loopback socket.
- A relayed header with no IP client (`UNSPEC`, Unix socket) is closed, as neither the limits nor the loopback rule would have a real client to go by.
- TLS passed through a proxy (header, then a ClientHello) works on an implicit listener. A proxy that terminated TLS and reports it in a verified SSL TLV gives `Transport::ProxyTls`, and such a connection is not offered STARTTLS.

**Who may sign in.** `sign_in_allowed()` is `encrypted() || loopback()`, judged on the reported client, so a proxy on loopback never makes its clients look local.

**Limits.** Listeners sharing a limiter share counts; each protocol has its own limiter. A connection counts per address until `authenticated`, then per user instead. `authenticated` refused with `PerUser` changes nothing. The same user again counts once. IPv6 clients count per `/64`, since one client usually owns the whole block. Places are freed when the `Connection` is dropped, including by a panic.

**TLS.** TLS 1.2 and 1.3 only, `ring`, no client certificates. `start_tls` never leaves plaintext behind on this side: `Connection` does not read ahead, so a front end has only its own buffer to discard (and must). After `Handshake` the connection fails every read and write.

**Certificate reload.** `watch` polls both files and compares their BLAKE3 hashes, so certbot's symlink swaps and copies alike are seen, and no file-event dependency is needed. Connections that finished their handshake keep the old certificate. A pair that does not load (half written, key and certificate out of step, file missing) leaves the current certificate in place and is retried on the next tick; the failure is logged once per distinct message. `watch` ends when the last clone of the `TlsCertificates` is dropped.

**Logs** carry reasons and counts, never client addresses or anything the client sent.

## Limits and quirks

- Plain TCP only; PROXY v1 is not understood.
- `serve` has no drain: stopping the runtime ends everything, like `holtmail-smtp`.
- A proxy on a loopback address that does not send PROXY headers makes every client look local. Configure `proxy_protocol` or do not listen in the clear.

## Tests

`cargo test -p holtmail-serve-net`, loopback only: unit tests for limits, trusted ranges and the signature guard; `tests/listener.rs` drives real sockets through plain, implicit TLS at 1.2 and 1.3, STARTTLS, trusted and untrusted headers, `LOCAL`, the SSL TLV, every limit, a panicking handler, timeouts, and certificate reloads (new connections only, bad files, the watcher).
