# holtmail-proxy-protocol

Parses [PROXY protocol v2](https://www.haproxy.org/download/2.9/doc/proxy-protocol.txt) headers, the binary preamble Caddy L4, HAProxy and load balancers put before a relayed TCP connection to say who the client really is. A library with no I/O and no dependencies beyond `thiserror`: `holtmail-serve-net` reads the bytes and decides whom to believe. Design in [SERVE.md](../../../docs/SERVE.md#listeners-and-tls). Version 1, the text form, is not understood.

## API

- `SIGNATURE`: the twelve bytes every header starts with.
- `parse(&[u8]) -> Result<Parsed, ParseError>`: `Parsed::Incomplete { needed }` when more bytes are needed (always more than given, counted from the start of the header), `Parsed::Complete { header, length }` when the first `length` bytes are a whole header.
- `ProxyHeader { command, addresses, ssl }`, `Command::{Local, Proxy}`, `Addresses { source, destination }`.
- `Ssl { client_tls, cert_in_connection, cert_in_session, verify, version, cipher }` is the SSL TLV. `Ssl::verified_tls()` is what "this client came over TLS" means to the rest of Holtmail.
- `ParseError`: `BadSignature`, `BadVersion`, `BadCommand`, `BadFamily`, `BadTransport`, `ShortAddresses`, `TruncatedTlv`, `ShortSsl`, `DuplicateSsl`.
- Feature `testing`: `testing::encode(&ProxyHeader) -> Vec<u8>`, for tests of code that reads headers.

## Guarantees

- `parse` never panics and never reads past the header, whatever the bytes. What follows `length` is the relayed connection's and is left alone.
- A wrong signature is reported from the first differing byte, so a caller need not wait for sixteen bytes to know.
- `LOCAL` headers and the `UNSPEC` family carry no addresses and no SSL block, even if the bytes contain some; the specification says to ignore them.
- `addresses` is `Some` only for TCP over IPv4 or IPv6. Unix-socket clients parse, with `addresses: None`. Datagram transports are refused: a TCP listener never carries them.
- Unknown TLVs are skipped. A truncated TLV anywhere, or a second SSL TLV, refuses the whole header. A `version` or `cipher` that is not UTF-8 reads as `None`. The optional CRC32c TLV is not checked: the sender is already trusted and TCP carries it.

`verified_tls` is `true` when the client bit says TLS and the client did not present a certificate that failed. Mail clients present no certificate, and proxies differ in what they put in `verify` when there is none (HAProxy zero, the specification allows non-zero), so `verify` counts only when a certificate was presented.

## Tests

`cargo test -p holtmail-proxy-protocol`: exact IPv4 bytes, IPv6, trailing data, SSL TLV with sub-TLVs, every refusal, and `proptest` runs (2000 cases each) feeding arbitrary bytes, arbitrary bytes behind the signature, structured bytes, encode-parse round trips and every prefix of a valid header.
