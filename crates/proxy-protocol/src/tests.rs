use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use proptest::prelude::*;

use super::testing::encode;
use super::*;

fn v4(a: u8, port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::new(203, 0, 113, a), port))
}

fn complete(input: &[u8]) -> (ProxyHeader, usize) {
    match parse(input).unwrap() {
        Parsed::Complete { header, length } => (header, length),
        other => panic!("not complete: {other:?}"),
    }
}

fn raw(command: u8, family: u8, body: &[u8]) -> Vec<u8> {
    let mut out = SIGNATURE.to_vec();
    out.extend_from_slice(&[command, family]);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

const INET_BLOCK: [u8; 12] = [203, 0, 113, 7, 198, 51, 100, 1, 0xc3, 0x50, 0x03, 0xe1];

#[test]
fn reads_ipv4_bytes_exactly() {
    let input = raw(0x21, 0x11, &INET_BLOCK);
    let (header, length) = complete(&input);
    assert_eq!(length, 28);
    assert_eq!(header.command, Command::Proxy);
    let addresses = header.addresses.unwrap();
    assert_eq!(addresses.source, v4(7, 50000));
    assert_eq!(
        addresses.destination,
        SocketAddr::from((Ipv4Addr::new(198, 51, 100, 1), 993))
    );
    assert_eq!(header.ssl, None);
}

#[test]
fn reads_ipv6() {
    let header = ProxyHeader {
        command: Command::Proxy,
        addresses: Some(Addresses {
            source: SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7), 50000)),
            destination: SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 465)),
        }),
        ssl: None,
    };
    assert_eq!(complete(&encode(&header)).0, header);
}

#[test]
fn leaves_what_follows_alone() {
    let mut input = raw(0x21, 0x11, &INET_BLOCK);
    input.extend_from_slice(b"a1 NOOP\r\n");
    assert_eq!(complete(&input).1, 28);
}

#[test]
fn reads_ssl_tlv_and_skips_others() {
    let mut body = INET_BLOCK.to_vec();
    body.extend_from_slice(&[0x04, 0, 2, 0, 0]);
    let mut ssl = vec![0x01, 0, 0, 0, 0];
    ssl.extend_from_slice(&[0x21, 0, 7]);
    ssl.extend_from_slice(b"TLSv1.3");
    ssl.extend_from_slice(&[0x23, 0, 3]);
    ssl.extend_from_slice(b"AES");
    ssl.extend_from_slice(&[0x25, 0, 1, b'x']);
    body.push(0x20);
    body.extend_from_slice(&(ssl.len() as u16).to_be_bytes());
    body.extend_from_slice(&ssl);
    let found = complete(&raw(0x21, 0x11, &body)).0.ssl.unwrap();
    assert!(found.client_tls && found.verified_tls());
    assert_eq!(found.version.as_deref(), Some("TLSv1.3"));
    assert_eq!(found.cipher.as_deref(), Some("AES"));
}

#[test]
fn judges_the_ssl_block() {
    let base = Ssl {
        client_tls: true,
        cert_in_connection: false,
        cert_in_session: false,
        verify: 0,
        version: None,
        cipher: None,
    };
    assert!(base.verified_tls());
    assert!(
        Ssl {
            verify: 1,
            ..base.clone()
        }
        .verified_tls(),
        "no certificate presented, nothing to fail"
    );
    assert!(
        !Ssl {
            verify: 1,
            cert_in_session: true,
            ..base.clone()
        }
        .verified_tls()
    );
    assert!(
        !Ssl {
            verify: 1,
            cert_in_connection: true,
            ..base.clone()
        }
        .verified_tls()
    );
    assert!(
        Ssl {
            cert_in_connection: true,
            ..base.clone()
        }
        .verified_tls()
    );
    assert!(
        !Ssl {
            client_tls: false,
            ..base
        }
        .verified_tls()
    );
}

#[test]
fn local_ignores_the_rest() {
    let (header, length) = complete(&raw(0x20, 0xff, &[9, 9, 9]));
    assert_eq!(length, 19);
    assert_eq!(header.command, Command::Local);
    assert_eq!(header.addresses, None);
}

#[test]
fn unspec_and_unix_have_no_addresses() {
    assert_eq!(complete(&raw(0x21, 0x00, &[1, 2])).0.addresses, None);
    assert_eq!(complete(&raw(0x21, 0x31, &[0; 216])).0.addresses, None);
}

#[test]
fn asks_for_more() {
    assert_eq!(parse(b"").unwrap(), Parsed::Incomplete { needed: 16 });
    assert_eq!(parse(b"\r\n\r").unwrap(), Parsed::Incomplete { needed: 16 });
    let input = raw(0x21, 0x11, &INET_BLOCK);
    assert_eq!(
        parse(&input[..16]).unwrap(),
        Parsed::Incomplete { needed: 28 }
    );
}

#[test]
fn refuses_what_is_wrong() {
    assert_eq!(parse(b"a1 "), Err(ParseError::BadSignature));
    assert_eq!(
        parse(b"PROXY TCP4 203.0.113.7 198.51.100.1 1 2\r\n"),
        Err(ParseError::BadSignature)
    );
    assert_eq!(
        parse(&raw(0x11, 0x11, &INET_BLOCK)),
        Err(ParseError::BadVersion(1))
    );
    assert_eq!(
        parse(&raw(0x22, 0x11, &INET_BLOCK)),
        Err(ParseError::BadCommand(2))
    );
    assert_eq!(
        parse(&raw(0x21, 0x41, &INET_BLOCK)),
        Err(ParseError::BadFamily(4))
    );
    assert_eq!(
        parse(&raw(0x21, 0x12, &INET_BLOCK)),
        Err(ParseError::BadTransport(2))
    );
    assert_eq!(
        parse(&raw(0x21, 0x11, &INET_BLOCK[..11])),
        Err(ParseError::ShortAddresses)
    );
    assert_eq!(
        parse(&raw(0x21, 0x21, &INET_BLOCK)),
        Err(ParseError::ShortAddresses)
    );
}

#[test]
fn refuses_bad_tlvs() {
    let with = |tlvs: &[u8]| {
        let mut body = INET_BLOCK.to_vec();
        body.extend_from_slice(tlvs);
        raw(0x21, 0x11, &body)
    };
    assert_eq!(parse(&with(&[0x20, 0])), Err(ParseError::TruncatedTlv));
    assert_eq!(
        parse(&with(&[0x04, 0, 9, 1])),
        Err(ParseError::TruncatedTlv)
    );
    assert_eq!(
        parse(&with(&[0x20, 0, 4, 1, 0, 0, 0])),
        Err(ParseError::ShortSsl)
    );
    assert_eq!(
        parse(&with(&[0x20, 0, 8, 1, 0, 0, 0, 0, 0x21, 0, 9])),
        Err(ParseError::TruncatedTlv)
    );
    let one = [0x20, 0, 5, 1, 0, 0, 0, 0];
    let mut two = one.to_vec();
    two.extend_from_slice(&one);
    assert_eq!(parse(&with(&two)), Err(ParseError::DuplicateSsl));
}

#[test]
fn a_version_that_is_not_utf8_is_dropped() {
    let mut body = INET_BLOCK.to_vec();
    body.extend_from_slice(&[0x20, 0, 9, 1, 0, 0, 0, 0, 0x21, 0, 1, 0xff]);
    let ssl = complete(&raw(0x21, 0x11, &body)).0.ssl.unwrap();
    assert_eq!(ssl.version, None);
}

fn address() -> impl Strategy<Value = (SocketAddr, SocketAddr)> {
    prop_oneof![
        (any::<[u8; 4]>(), any::<[u8; 4]>(), any::<(u16, u16)>())
            .prop_map(|(a, b, (p, q))| (SocketAddr::from((a, p)), SocketAddr::from((b, q)))),
        (any::<[u8; 16]>(), any::<[u8; 16]>(), any::<(u16, u16)>())
            .prop_map(|(a, b, (p, q))| (SocketAddr::from((a, p)), SocketAddr::from((b, q)))),
    ]
}

fn ssl_info() -> impl Strategy<Value = Option<Ssl>> {
    proptest::option::of(
        (
            any::<(bool, bool, bool)>(),
            any::<u32>(),
            proptest::option::of("[ -~]{0,32}"),
            proptest::option::of("[ -~]{0,32}"),
        )
            .prop_map(|((client_tls, conn, sess), verify, version, cipher)| Ssl {
                client_tls,
                cert_in_connection: conn,
                cert_in_session: sess,
                verify,
                version,
                cipher,
            }),
    )
}

fn proxy_header() -> impl Strategy<Value = ProxyHeader> {
    (address(), ssl_info()).prop_map(|((source, destination), ssl)| ProxyHeader {
        command: Command::Proxy,
        addresses: Some(Addresses {
            source,
            destination,
        }),
        ssl,
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn arbitrary_bytes_never_panic(input in proptest::collection::vec(any::<u8>(), 0..400)) {
        let _ = parse(&input);
    }

    #[test]
    fn arbitrary_bytes_behind_the_signature_never_panic(
        tail in proptest::collection::vec(any::<u8>(), 0..400),
    ) {
        let mut input = SIGNATURE.to_vec();
        input.extend_from_slice(&tail);
        match parse(&input) {
            Ok(Parsed::Incomplete { needed }) => prop_assert!(needed > input.len()),
            Ok(Parsed::Complete { length, .. }) => prop_assert!(length <= input.len()),
            Err(_) => {}
        }
    }

    #[test]
    fn structured_bytes_never_panic(
        command in prop_oneof![Just(0x20u8), Just(0x21u8), any::<u8>()],
        family in prop_oneof![Just(0x11u8), Just(0x21u8), Just(0x31u8), Just(0u8), any::<u8>()],
        body in proptest::collection::vec(any::<u8>(), 0..400),
    ) {
        let input = raw(command, family, &body);
        let _ = parse(&input);
    }

    #[test]
    fn what_is_encoded_comes_back(header in proxy_header()) {
        let bytes = encode(&header);
        let (parsed, length) = complete(&bytes);
        prop_assert_eq!(parsed, header);
        prop_assert_eq!(length, bytes.len());
    }

    #[test]
    fn every_prefix_asks_for_more(header in proxy_header()) {
        let bytes = encode(&header);
        for end in 0..bytes.len() {
            match parse(&bytes[..end]) {
                Ok(Parsed::Incomplete { needed }) => prop_assert!(needed > end),
                other => prop_assert!(false, "prefix {} gave {:?}", end, other),
            }
        }
    }
}
