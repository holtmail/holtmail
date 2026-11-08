//! Parses PROXY protocol v2 headers: the client and server addresses a TCP proxy reports, and
//! its SSL TLV. No I/O; [`parse`] works on bytes the caller has read.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// The twelve bytes every PROXY v2 header starts with.
pub const SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

const FIXED_LENGTH: usize = 16;
const UNIX_ADDRESSES: usize = 216;

const TLV_SSL: u8 = 0x20;
const SSL_VERSION: u8 = 0x21;
const SSL_CIPHER: u8 = 0x23;

const CLIENT_SSL: u8 = 0x01;
const CLIENT_CERT_CONN: u8 = 0x02;
const CLIENT_CERT_SESS: u8 = 0x04;

/// What the proxy says the connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// The proxy opened this connection for itself, for example as a health check. A header
    /// with this command never carries addresses or an SSL block.
    Local,
    /// The connection is relayed for a client.
    Proxy,
}

/// The two ends of the relayed connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Addresses {
    /// The client's address.
    pub source: SocketAddr,
    /// The address the client connected to.
    pub destination: SocketAddr,
}

/// The proxy's SSL TLV: how the client reached the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ssl {
    /// The client connected to the proxy over TLS.
    pub client_tls: bool,
    /// The client presented a certificate on this connection.
    pub cert_in_connection: bool,
    /// The client presented a certificate at some point in this TLS session.
    pub cert_in_session: bool,
    /// The result of checking the client's certificate. Zero means verified; it means nothing
    /// when the client presented none.
    pub verify: u32,
    /// The TLS version, such as `TLSv1.3`, when the proxy reports it as UTF-8.
    pub version: Option<String>,
    /// The cipher name, when the proxy reports it as UTF-8.
    pub cipher: Option<String>,
}

impl Ssl {
    /// Whether the client's connection to the proxy was TLS and nothing about it failed
    /// verification: `false` without TLS, and `false` when the client presented a certificate
    /// that did not verify. A client that presented no certificate passes.
    pub fn verified_tls(&self) -> bool {
        self.client_tls && (!(self.cert_in_connection || self.cert_in_session) || self.verify == 0)
    }
}

/// A complete, valid PROXY v2 header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyHeader {
    /// Local or relayed.
    pub command: Command,
    /// `Some` only for a relayed TCP connection over IPv4 or IPv6. `None` when the proxy does
    /// not know the client (the `UNSPEC` family) or the client came over a Unix socket.
    pub addresses: Option<Addresses>,
    /// The SSL TLV, when the proxy sent one.
    pub ssl: Option<Ssl>,
}

/// What [`parse`] made of the bytes so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// The bytes are a valid start; at least `needed` bytes in total are required to go on.
    /// `needed` is always more than the bytes given.
    Incomplete {
        /// Total bytes required, counted from the start of the header.
        needed: usize,
    },
    /// A whole header.
    Complete {
        /// The header.
        header: ProxyHeader,
        /// How many leading bytes it took. What follows belongs to the relayed connection.
        length: usize,
    },
}

/// Why bytes are not a usable PROXY v2 header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The bytes do not start with [`SIGNATURE`].
    #[error("not a PROXY v2 header")]
    BadSignature,
    /// The version is not 2.
    #[error("unsupported PROXY version {0}")]
    BadVersion(u8),
    /// The command is neither `LOCAL` nor `PROXY`.
    #[error("unknown PROXY command {0}")]
    BadCommand(u8),
    /// The address family is not one of `UNSPEC`, `INET`, `INET6`, `UNIX`.
    #[error("unknown PROXY address family {0}")]
    BadFamily(u8),
    /// The transport is not a stream.
    #[error("unsupported PROXY transport {0}")]
    BadTransport(u8),
    /// The header is too short for its address family.
    #[error("the PROXY address block is too short")]
    ShortAddresses,
    /// A TLV runs past the end of the header.
    #[error("a PROXY TLV is truncated")]
    TruncatedTlv,
    /// The SSL TLV is too short.
    #[error("the PROXY SSL TLV is too short")]
    ShortSsl,
    /// More than one SSL TLV.
    #[error("more than one PROXY SSL TLV")]
    DuplicateSsl,
}

/// Reads a PROXY v2 header from the start of `input`.
///
/// Never reads past the header and never panics, whatever the bytes. A wrong signature is
/// reported as soon as the first differing byte is seen. Unknown TLVs are skipped; the optional
/// CRC32c TLV is not checked. For a `LOCAL` command and for the `UNSPEC` family the address
/// block and TLVs are ignored, as the specification requires.
pub fn parse(input: &[u8]) -> Result<Parsed, ParseError> {
    let known = input.len().min(SIGNATURE.len());
    if input[..known] != SIGNATURE[..known] {
        return Err(ParseError::BadSignature);
    }
    if input.len() < FIXED_LENGTH {
        return Ok(Parsed::Incomplete {
            needed: FIXED_LENGTH,
        });
    }
    let version = input[12] >> 4;
    if version != 2 {
        return Err(ParseError::BadVersion(version));
    }
    let command = match input[12] & 0x0f {
        0 => Command::Local,
        1 => Command::Proxy,
        other => return Err(ParseError::BadCommand(other)),
    };
    let length = FIXED_LENGTH + usize::from(u16::from_be_bytes([input[14], input[15]]));
    if input.len() < length {
        return Ok(Parsed::Incomplete { needed: length });
    }
    let header = match command {
        Command::Local => ProxyHeader {
            command,
            addresses: None,
            ssl: None,
        },
        Command::Proxy => relayed(input[13], &input[FIXED_LENGTH..length])?,
    };
    Ok(Parsed::Complete { header, length })
}

fn relayed(family_transport: u8, body: &[u8]) -> Result<ProxyHeader, ParseError> {
    let family = family_transport >> 4;
    let transport = family_transport & 0x0f;
    if family == 0 {
        return Ok(ProxyHeader {
            command: Command::Proxy,
            addresses: None,
            ssl: None,
        });
    }
    if family > 3 {
        return Err(ParseError::BadFamily(family));
    }
    if transport != 1 {
        return Err(ParseError::BadTransport(transport));
    }
    let size = match family {
        1 => 12,
        2 => 36,
        _ => UNIX_ADDRESSES,
    };
    if body.len() < size {
        return Err(ParseError::ShortAddresses);
    }
    let (block, tlvs) = body.split_at(size);
    let addresses = match family {
        1 => Some(inet(block)),
        2 => Some(inet6(block)),
        _ => None,
    };
    Ok(ProxyHeader {
        command: Command::Proxy,
        addresses,
        ssl: ssl_block(tlvs)?,
    })
}

fn inet(block: &[u8]) -> Addresses {
    let ip = |at: usize| {
        IpAddr::V4(Ipv4Addr::new(
            block[at],
            block[at + 1],
            block[at + 2],
            block[at + 3],
        ))
    };
    let port = |at: usize| u16::from_be_bytes([block[at], block[at + 1]]);
    Addresses {
        source: SocketAddr::new(ip(0), port(8)),
        destination: SocketAddr::new(ip(4), port(10)),
    }
}

fn inet6(block: &[u8]) -> Addresses {
    let ip = |at: usize| {
        let mut octets = [0u8; 16];
        octets.copy_from_slice(&block[at..at + 16]);
        IpAddr::V6(Ipv6Addr::from(octets))
    };
    let port = |at: usize| u16::from_be_bytes([block[at], block[at + 1]]);
    Addresses {
        source: SocketAddr::new(ip(0), port(32)),
        destination: SocketAddr::new(ip(16), port(34)),
    }
}

fn next_tlv<'a>(input: &mut &'a [u8]) -> Result<Option<(u8, &'a [u8])>, ParseError> {
    let Some((head, rest)) = input.split_at_checked(3) else {
        return if input.is_empty() {
            Ok(None)
        } else {
            Err(ParseError::TruncatedTlv)
        };
    };
    let size = usize::from(u16::from_be_bytes([head[1], head[2]]));
    let (value, after) = rest
        .split_at_checked(size)
        .ok_or(ParseError::TruncatedTlv)?;
    *input = after;
    Ok(Some((head[0], value)))
}

fn ssl_block(mut tlvs: &[u8]) -> Result<Option<Ssl>, ParseError> {
    let mut found = None;
    while let Some((kind, value)) = next_tlv(&mut tlvs)? {
        if kind == TLV_SSL {
            if found.is_some() {
                return Err(ParseError::DuplicateSsl);
            }
            found = Some(ssl(value)?);
        }
    }
    Ok(found)
}

fn ssl(value: &[u8]) -> Result<Ssl, ParseError> {
    let (fixed, mut sub) = value.split_at_checked(5).ok_or(ParseError::ShortSsl)?;
    let client = fixed[0];
    let mut out = Ssl {
        client_tls: client & CLIENT_SSL != 0,
        cert_in_connection: client & CLIENT_CERT_CONN != 0,
        cert_in_session: client & CLIENT_CERT_SESS != 0,
        verify: u32::from_be_bytes([fixed[1], fixed[2], fixed[3], fixed[4]]),
        version: None,
        cipher: None,
    };
    while let Some((kind, text)) = next_tlv(&mut sub)? {
        let text = std::str::from_utf8(text).ok().map(str::to_owned);
        match kind {
            SSL_VERSION => out.version = text,
            SSL_CIPHER => out.cipher = text,
            _ => {}
        }
    }
    Ok(out)
}

/// Builds headers for tests of code that reads them.
#[cfg(feature = "testing")]
pub mod testing {
    use std::net::IpAddr;

    use super::{Command, FIXED_LENGTH, ProxyHeader, SIGNATURE, Ssl};

    /// The bytes of `header`. Panics when the two addresses are of different families.
    pub fn encode(header: &ProxyHeader) -> Vec<u8> {
        let command = match header.command {
            Command::Local => 0x20,
            Command::Proxy => 0x21,
        };
        let mut body = Vec::new();
        let family = match header.addresses {
            None => 0x00,
            Some(addresses) => {
                let family = match (addresses.source.ip(), addresses.destination.ip()) {
                    (IpAddr::V4(source), IpAddr::V4(destination)) => {
                        body.extend_from_slice(&source.octets());
                        body.extend_from_slice(&destination.octets());
                        0x11
                    }
                    (IpAddr::V6(source), IpAddr::V6(destination)) => {
                        body.extend_from_slice(&source.octets());
                        body.extend_from_slice(&destination.octets());
                        0x21
                    }
                    _ => panic!("both addresses must be of one family"),
                };
                body.extend_from_slice(&addresses.source.port().to_be_bytes());
                body.extend_from_slice(&addresses.destination.port().to_be_bytes());
                family
            }
        };
        if let Some(ssl) = &header.ssl {
            encode_tlv(&mut body, super::TLV_SSL, &ssl_value(ssl));
        }
        let mut out = Vec::with_capacity(FIXED_LENGTH + body.len());
        out.extend_from_slice(&SIGNATURE);
        out.push(command);
        out.push(family);
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn ssl_value(ssl: &Ssl) -> Vec<u8> {
        let mut client = 0;
        if ssl.client_tls {
            client |= super::CLIENT_SSL;
        }
        if ssl.cert_in_connection {
            client |= super::CLIENT_CERT_CONN;
        }
        if ssl.cert_in_session {
            client |= super::CLIENT_CERT_SESS;
        }
        let mut value = vec![client];
        value.extend_from_slice(&ssl.verify.to_be_bytes());
        if let Some(version) = &ssl.version {
            encode_tlv(&mut value, super::SSL_VERSION, version.as_bytes());
        }
        if let Some(cipher) = &ssl.cipher {
            encode_tlv(&mut value, super::SSL_CIPHER, cipher.as_bytes());
        }
        value
    }

    fn encode_tlv(out: &mut Vec<u8>, kind: u8, value: &[u8]) {
        out.push(kind);
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value);
    }
}

#[cfg(test)]
mod tests;
