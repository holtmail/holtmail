use super::*;

fn identity(id: &str, address: &str, transport: &str) -> Identity {
    Identity {
        id: id.into(),
        address: address.into(),
        name: Some(format!("Name {id}")),
        transport: transport.into(),
        active: true,
        ..Identity::default()
    }
}

fn identities() -> Vec<Identity> {
    vec![
        Identity {
            account: Some("acc-1".into()),
            plus_addresses: true,
            ..identity("ada", "Ada@Example.com", "fast")
        },
        identity("bob", "bob@example.com", "fast"),
        Identity {
            account: Some("acc-2".into()),
            ..identity("work", "ada@work.example.org", "work")
        },
        Identity {
            alias: Some("Shop.xyz@alias.example.net".into()),
            ..identity("masked", "mailbox@example.com", "fast")
        },
        Identity {
            account: Some("acc-3".into()),
            active: false,
            ..identity("off", "off@example.com", "fast")
        },
        identity("carol", "carol@example.org", "relay"),
        identity("ghost", "ghost@example.com", "missing"),
        Identity {
            plus_addresses: true,
            ..identity("eve", "eve@example.net", "exact")
        },
        Identity {
            account: Some("acc-1".into()),
            ..identity("second", "second@example.com", "fast")
        },
    ]
}

fn transports() -> Vec<Transport> {
    let t = |id: &str, patterns: &[&str]| Transport {
        id: id.into(),
        allowed_from: patterns.iter().map(|p| p.to_string()).collect(),
    };
    vec![
        t("fast", &["*@example.com"]),
        t("work", &["*@*.example.org"]),
        t("relay", &["dave@example.org"]),
        t("exact", &["eve@example.net"]),
    ]
}

fn reply(recipients: &[&str], account: Option<&str>, alias: Option<&str>) -> Context {
    Context::Reply(Original {
        recipients: recipients.iter().map(|r| r.to_string()).collect(),
        account: account.map(Into::into),
        alias: alias.map(|a| AliasHit {
            provider: "simplelogin".into(),
            alias: a.into(),
            reverse_alias: Some("reply+abc@relay.example.net".into()),
            sender: Some("zed@example.net".into()),
        }),
    })
}

type Expected = Result<(&'static str, &'static str, Vec<Reason>), RouteError>;

type Case = (
    &'static str,
    Context,
    Option<&'static str>,
    Option<&'static str>,
    Expected,
);

fn ok(identity: &'static str, from: &'static str, reasons: Vec<Reason>) -> Expected {
    Ok((identity, from, reasons))
}

fn recipient(a: &str) -> Reason {
    Reason::OriginalRecipient { address: a.into() }
}

fn plus(a: &str) -> Reason {
    Reason::PlusAddress { address: a.into() }
}

fn dropped(a: &str) -> Reason {
    Reason::PlusAddressDropped { address: a.into() }
}

#[test]
fn rules() {
    let account = |a: &str| Reason::Account { account: a.into() };
    let alias = |a: &str| Reason::Alias { alias: a.into() };
    let cases: Vec<Case> = vec![
        (
            "new message uses the default",
            Context::New,
            Some("bob"),
            None,
            ok("bob", "bob@example.com", vec![Reason::Default]),
        ),
        (
            "new message without default",
            Context::New,
            None,
            None,
            Err(RouteError::NoIdentity),
        ),
        (
            "inactive default",
            Context::New,
            Some("off"),
            None,
            Err(RouteError::NoIdentity),
        ),
        (
            "unknown default",
            Context::New,
            Some("nobody"),
            None,
            Err(RouteError::NoIdentity),
        ),
        (
            "reply to an identity address, case-insensitive",
            reply(&["zed@example.net", "ADA@example.COM"], Some("acc-2"), None),
            Some("bob"),
            None,
            ok("ada", "Ada@Example.com", vec![recipient("ada@example.com")]),
        ),
        (
            "earlier recipient wins",
            reply(&["bob@example.com", "ada@example.com"], None, None),
            None,
            None,
            ok("bob", "bob@example.com", vec![recipient("bob@example.com")]),
        ),
        (
            "exact match before plus match",
            reply(&["ada+shop@example.com", "bob@example.com"], None, None),
            None,
            None,
            ok("bob", "bob@example.com", vec![recipient("bob@example.com")]),
        ),
        (
            "delivered-to after to and cc",
            reply(&["list@example.org", "bob@example.com"], None, None),
            None,
            None,
            ok("bob", "bob@example.com", vec![recipient("bob@example.com")]),
        ),
        (
            "plus address kept when the identity allows it",
            reply(&["Ada+Shop@example.com"], None, None),
            None,
            None,
            ok(
                "ada",
                "ada+shop@example.com",
                vec![plus("ada+shop@example.com")],
            ),
        ),
        (
            "plus address dropped when the identity does not keep them",
            reply(&["bob+news@example.com"], None, None),
            None,
            None,
            ok(
                "bob",
                "bob@example.com",
                vec![
                    plus("bob+news@example.com"),
                    dropped("bob+news@example.com"),
                ],
            ),
        ),
        (
            "plus address dropped when the transport may not send as it",
            reply(&["eve+x@example.net"], None, None),
            None,
            None,
            ok(
                "eve",
                "eve@example.net",
                vec![plus("eve+x@example.net"), dropped("eve+x@example.net")],
            ),
        ),
        (
            "empty plus tag is not a plus address",
            reply(&["ada+@example.com"], None, Some("x@alias.example.net")),
            Some("bob"),
            None,
            ok("bob", "bob@example.com", vec![Reason::Default]),
        ),
        (
            "alias hit picks the identity linked to the alias",
            reply(
                &["shop.xyz@alias.example.net"],
                Some("acc-1"),
                Some("SHOP.xyz@alias.example.net"),
            ),
            Some("bob"),
            None,
            ok(
                "masked",
                "mailbox@example.com",
                vec![alias("shop.xyz@alias.example.net")],
            ),
        ),
        (
            "alias among recipients without a hit",
            reply(&["shop.xyz@alias.example.net"], Some("acc-1"), None),
            None,
            None,
            ok(
                "masked",
                "mailbox@example.com",
                vec![alias("shop.xyz@alias.example.net")],
            ),
        ),
        (
            "identity address before alias",
            reply(
                &["shop.xyz@alias.example.net", "bob@example.com"],
                None,
                Some("shop.xyz@alias.example.net"),
            ),
            None,
            None,
            ok("bob", "bob@example.com", vec![recipient("bob@example.com")]),
        ),
        (
            "account of the original",
            reply(&["list@example.org"], Some("acc-2"), None),
            Some("bob"),
            None,
            ok("work", "ada@work.example.org", vec![account("acc-2")]),
        ),
        (
            "first identity of the account wins",
            reply(&["list@example.org"], Some("acc-1"), None),
            None,
            None,
            ok("ada", "Ada@Example.com", vec![account("acc-1")]),
        ),
        (
            "inactive identity of the account falls through to the default",
            reply(&["off@example.com"], Some("acc-3"), None),
            Some("bob"),
            None,
            ok("bob", "bob@example.com", vec![Reason::Default]),
        ),
        (
            "forward routes like a reply",
            Context::Forward(Original {
                recipients: vec!["bob@example.com".into()],
                ..Original::default()
            }),
            Some("ada"),
            None,
            ok("bob", "bob@example.com", vec![recipient("bob@example.com")]),
        ),
        (
            "reply without any match and no default",
            reply(&["list@example.org"], Some("acc-9"), None),
            None,
            None,
            Err(RouteError::NoIdentity),
        ),
        (
            "chosen identity wins over the original recipient",
            reply(&["ada@example.com"], Some("acc-1"), None),
            Some("ada"),
            Some("work"),
            ok("work", "ada@work.example.org", vec![Reason::Chosen]),
        ),
        (
            "chosen identity is still checked",
            Context::New,
            Some("bob"),
            Some("carol"),
            Err(RouteError::NotAllowedFrom {
                address: "carol@example.org".into(),
                transport: "relay".into(),
            }),
        ),
        (
            "chosen inactive identity",
            Context::New,
            Some("bob"),
            Some("off"),
            Err(RouteError::UnknownIdentity { id: "off".into() }),
        ),
        (
            "chosen unknown identity",
            Context::New,
            Some("bob"),
            Some("nobody"),
            Err(RouteError::UnknownIdentity {
                id: "nobody".into(),
            }),
        ),
        (
            "default outside allowed_from",
            Context::New,
            Some("carol"),
            None,
            Err(RouteError::NotAllowedFrom {
                address: "carol@example.org".into(),
                transport: "relay".into(),
            }),
        ),
        (
            "matched identity outside allowed_from does not fall through",
            reply(&["carol@example.org"], Some("acc-1"), None),
            Some("bob"),
            None,
            Err(RouteError::NotAllowedFrom {
                address: "carol@example.org".into(),
                transport: "relay".into(),
            }),
        ),
        (
            "missing transport",
            Context::New,
            Some("ghost"),
            None,
            Err(RouteError::UnknownTransport {
                transport: "missing".into(),
            }),
        ),
    ];

    let identities = identities();
    let transports = transports();
    for (name, context, default_identity, chosen, expected) in cases {
        let got = route(Request {
            identities: &identities,
            transports: &transports,
            context: &context,
            default_identity,
            chosen,
        });
        let expected = expected.map(|(identity, from, reasons)| Route {
            identity: identity.into(),
            from: from.into(),
            name: Some(format!("Name {identity}")),
            transport: identities
                .iter()
                .find(|i| i.id == identity)
                .unwrap()
                .transport
                .clone(),
            strategy: Strategy::Direct,
            reasons,
        });
        assert_eq!(got, expected, "{name}");
    }
}

#[test]
fn transport_allows_any_pattern() {
    let t = Transport {
        id: "t".into(),
        allowed_from: vec!["ada@example.com".into(), "*@*.example.org".into()],
    };
    assert!(t.allows("ada@example.com"));
    assert!(t.allows("x@mail.example.org"));
    assert!(!t.allows("x@example.org"));
    assert!(!Transport::default().allows("ada@example.com"));
}

#[test]
fn wire_codes() {
    let reason = serde_json::to_value(Reason::PlusAddress {
        address: "ada+x@example.com".into(),
    })
    .unwrap();
    assert_eq!(
        reason,
        serde_json::json!({"code": "plus_address", "address": "ada+x@example.com"})
    );
    let error = serde_json::to_value(RouteError::NoIdentity).unwrap();
    assert_eq!(error, serde_json::json!({"code": "no_identity"}));
    assert_eq!(
        serde_json::to_value(Strategy::ReverseAlias).unwrap(),
        serde_json::json!("reverse_alias")
    );
}

const ALIAS: &str = "shop@alias.example.net";
const REVERSE: &str = "reply+abc@alias.example.net";
const COMPUTED: &str = "shop+zed=example.net@alias.example.net";
const REPLY_TO: &str = "relay-zed@reply.example.net";

fn relay_identities() -> Vec<Identity> {
    vec![
        Identity {
            account: Some("acc-1".into()),
            alias: Some(ALIAS.into()),
            ..identity("alias-id", "Shop@alias.example.net", "aliases")
        },
        Identity {
            account: Some("acc-1".into()),
            ..identity("acct", "ada@example.com", "fast")
        },
        identity("box", "box@example.com", "fast"),
        identity("other", "other@example.com", "fast"),
        identity("strict", "strict@example.net", "exact"),
        Identity {
            alias: Some("m.1@masked.example.org".into()),
            ..identity("linked", "linked@example.com", "masked")
        },
        Identity {
            alias: Some("m.3@masked.example.org".into()),
            ..identity("solo", "solo@example.net", "masked")
        },
        Identity {
            alias: Some("m.2@masked.example.org".into()),
            ..identity("narrow", "narrow@example.com", "fast")
        },
    ]
}

fn relay_transports() -> Vec<Transport> {
    let mut t = transports();
    t.push(Transport {
        id: "aliases".into(),
        allowed_from: vec!["*@alias.example.net".into()],
    });
    t.push(Transport {
        id: "masked".into(),
        allowed_from: vec!["*@example.com".into(), "*@masked.example.org".into()],
    });
    t
}

fn relay(strategy: Strategy) -> Relay {
    Relay {
        hit: AliasHit {
            provider: "simplelogin".into(),
            alias: ALIAS.into(),
            reverse_alias: Some(REVERSE.into()),
            sender: Some("Zed@example.net".into()),
        },
        signed: true,
        strategy,
        mailboxes: vec!["box@example.com".into()],
        reply_to: Some(REPLY_TO.into()),
        reverse_alias: None,
    }
}

fn alias_reply(recipients: &[&str], account: Option<&str>) -> Context {
    reply(recipients, account, None)
}

fn forward(recipients: &[&str]) -> Context {
    Context::Forward(Original {
        recipients: recipients.iter().map(|r| r.to_string()).collect(),
        account: Some("acc-1".into()),
        alias: None,
    })
}

type RelayExpected = Result<
    (
        &'static str,
        &'static str,
        Strategy,
        Option<&'static str>,
        Vec<Reason>,
    ),
    RouteError,
>;

struct RelayCase {
    name: &'static str,
    relay: Option<Relay>,
    context: Context,
    default_identity: Option<&'static str>,
    chosen: Option<&'static str>,
    expected: RelayExpected,
}

fn case(
    name: &'static str,
    relay: Option<Relay>,
    context: Context,
    default_identity: Option<&'static str>,
    chosen: Option<&'static str>,
    expected: RelayExpected,
) -> RelayCase {
    RelayCase {
        name,
        relay,
        context,
        default_identity,
        chosen,
        expected,
    }
}

#[test]
fn relay_strategies() {
    use Strategy::{Direct, ReplyHeader, ReverseAlias};
    let mailbox = |a: &str| Reason::AliasMailbox { address: a.into() };
    let reverse = |a: &str| Reason::ReverseAlias { address: a.into() };
    let computed = |a: &str| Reason::ComputedReverseAlias { address: a.into() };
    let missing = Reason::NoAliasMailbox {
        alias: ALIAS.into(),
    };
    let needs = |contact: Option<&str>| RouteError::NeedsReverseAlias {
        alias: ALIAS.into(),
        contact: contact.map(Into::into),
    };
    let plain = alias_reply(&["list@example.org"], Some("acc-9"));
    let cases = vec![
        case(
            "no relay routes as before",
            None,
            alias_reply(&["box@example.com"], None),
            None,
            None,
            Ok((
                "box",
                "box@example.com",
                Direct,
                None,
                vec![recipient("box@example.com")],
            )),
        ),
        case(
            "a new message ignores the relay",
            Some(relay(ReverseAlias)),
            Context::New,
            Some("other"),
            None,
            Ok((
                "other",
                "other@example.com",
                Direct,
                None,
                vec![Reason::Default],
            )),
        ),
        case(
            "reverse alias from the forwarding mailbox",
            Some(relay(ReverseAlias)),
            alias_reply(&[ALIAS], Some("acc-1")),
            None,
            None,
            Ok((
                "box",
                "box@example.com",
                ReverseAlias,
                Some(REVERSE),
                vec![mailbox("box@example.com"), reverse(REVERSE)],
            )),
        ),
        case(
            "mailboxes are tried in the service's order",
            Some(Relay {
                mailboxes: vec![
                    "nobody@example.com".into(),
                    "OTHER@example.com".into(),
                    "box@example.com".into(),
                ],
                ..relay(ReverseAlias)
            }),
            alias_reply(&[ALIAS], Some("acc-1")),
            None,
            None,
            Ok((
                "other",
                "other@example.com",
                ReverseAlias,
                Some(REVERSE),
                vec![mailbox("other@example.com"), reverse(REVERSE)],
            )),
        ),
        case(
            "no mailbox identity falls back to a real mailbox of the account",
            Some(Relay {
                mailboxes: vec!["nobody@example.com".into()],
                ..relay(ReverseAlias)
            }),
            alias_reply(&[ALIAS], Some("acc-1")),
            None,
            None,
            Ok((
                "acct",
                "ada@example.com",
                ReverseAlias,
                Some(REVERSE),
                vec![
                    Reason::Account {
                        account: "acc-1".into(),
                    },
                    missing.clone(),
                    reverse(REVERSE),
                ],
            )),
        ),
        case(
            "then to a real default identity",
            Some(Relay {
                mailboxes: vec![],
                ..relay(ReverseAlias)
            }),
            plain.clone(),
            Some("other"),
            None,
            Ok((
                "other",
                "other@example.com",
                ReverseAlias,
                Some(REVERSE),
                vec![Reason::Default, missing.clone(), reverse(REVERSE)],
            )),
        ),
        case(
            "an alias identity is never the real mailbox",
            Some(Relay {
                mailboxes: vec![],
                ..relay(ReverseAlias)
            }),
            plain.clone(),
            Some("alias-id"),
            None,
            Err(RouteError::NoIdentity),
        ),
        case(
            "an unsigned reverse alias is replaced by the computed one",
            Some(Relay {
                signed: false,
                reverse_alias: Some(COMPUTED.into()),
                ..relay(ReverseAlias)
            }),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Ok((
                "box",
                "box@example.com",
                ReverseAlias,
                Some(COMPUTED),
                vec![
                    mailbox("box@example.com"),
                    Reason::UnsignedReverseAlias {
                        address: REVERSE.into(),
                    },
                    computed(COMPUTED),
                ],
            )),
        ),
        case(
            "an unsigned reverse alias alone needs a new one",
            Some(Relay {
                signed: false,
                ..relay(ReverseAlias)
            }),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Err(needs(Some("zed@example.net"))),
        ),
        case(
            "no reverse alias and no rewritten sender",
            Some(Relay {
                hit: AliasHit {
                    reverse_alias: None,
                    sender: None,
                    ..relay(ReverseAlias).hit
                },
                ..relay(ReverseAlias)
            }),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Err(needs(None)),
        ),
        case(
            "the computed reverse alias when the hit has none",
            Some(Relay {
                hit: AliasHit {
                    reverse_alias: None,
                    ..relay(ReverseAlias).hit
                },
                reverse_alias: Some(COMPUTED.into()),
                ..relay(ReverseAlias)
            }),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Ok((
                "box",
                "box@example.com",
                ReverseAlias,
                Some(COMPUTED),
                vec![mailbox("box@example.com"), computed(COMPUTED)],
            )),
        ),
        case(
            "a chosen identity outside the mailboxes gets a note",
            Some(relay(ReverseAlias)),
            alias_reply(&[ALIAS], None),
            None,
            Some("other"),
            Ok((
                "other",
                "other@example.com",
                ReverseAlias,
                Some(REVERSE),
                vec![
                    Reason::Chosen,
                    Reason::NotAliasMailbox {
                        address: "other@example.com".into(),
                    },
                    reverse(REVERSE),
                ],
            )),
        ),
        case(
            "a chosen mailbox has no note",
            Some(relay(ReverseAlias)),
            alias_reply(&[ALIAS], None),
            None,
            Some("box"),
            Ok((
                "box",
                "box@example.com",
                ReverseAlias,
                Some(REVERSE),
                vec![Reason::Chosen, reverse(REVERSE)],
            )),
        ),
        case(
            "a chosen inactive identity",
            Some(relay(ReverseAlias)),
            alias_reply(&[ALIAS], None),
            None,
            Some("nobody"),
            Err(RouteError::UnknownIdentity {
                id: "nobody".into(),
            }),
        ),
        case(
            "a forward keeps its recipients",
            Some(relay(ReverseAlias)),
            forward(&[ALIAS]),
            None,
            None,
            Ok((
                "box",
                "box@example.com",
                ReverseAlias,
                None,
                vec![mailbox("box@example.com")],
            )),
        ),
        case(
            "the mailbox is still checked against allowed_from",
            Some(Relay {
                mailboxes: vec!["strict@example.net".into()],
                ..relay(ReverseAlias)
            }),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Err(RouteError::NotAllowedFrom {
                address: "strict@example.net".into(),
                transport: "exact".into(),
            }),
        ),
        case(
            "direct from the alias identity",
            Some(relay(Direct)),
            alias_reply(&[ALIAS], Some("acc-1")),
            None,
            None,
            Ok(("alias-id", ALIAS, Direct, None, vec![recipient(ALIAS)])),
        ),
        case(
            "direct from the alias through a linked identity",
            Some(Relay {
                hit: AliasHit {
                    alias: "M.1@masked.example.org".into(),
                    ..relay(Direct).hit
                },
                ..relay(Direct)
            }),
            reply(&["m.1@masked.example.org"], None, None),
            None,
            None,
            Ok((
                "linked",
                "m.1@masked.example.org",
                Direct,
                None,
                vec![
                    Reason::Alias {
                        alias: "m.1@masked.example.org".into(),
                    },
                    Reason::AliasFrom {
                        alias: "m.1@masked.example.org".into(),
                    },
                ],
            )),
        ),
        case(
            "direct checks only the alias against allowed_from",
            Some(Relay {
                hit: AliasHit {
                    alias: "m.3@masked.example.org".into(),
                    ..relay(Direct).hit
                },
                ..relay(Direct)
            }),
            reply(&["m.3@masked.example.org"], None, None),
            None,
            None,
            Ok((
                "solo",
                "m.3@masked.example.org",
                Direct,
                None,
                vec![
                    Reason::Alias {
                        alias: "m.3@masked.example.org".into(),
                    },
                    Reason::AliasFrom {
                        alias: "m.3@masked.example.org".into(),
                    },
                ],
            )),
        ),
        case(
            "direct is refused when the transport may not send as the alias",
            Some(Relay {
                hit: AliasHit {
                    alias: "m.2@masked.example.org".into(),
                    ..relay(Direct).hit
                },
                ..relay(Direct)
            }),
            reply(&["m.2@masked.example.org"], None, None),
            None,
            None,
            Err(RouteError::NotAllowedFrom {
                address: "m.2@masked.example.org".into(),
                transport: "fast".into(),
            }),
        ),
        case(
            "direct needs an identity that sends as the alias",
            Some(Relay {
                hit: AliasHit {
                    alias: "nobody@masked.example.org".into(),
                    ..relay(Direct).hit
                },
                ..relay(Direct)
            }),
            reply(&["nobody@masked.example.org"], None, None),
            Some("box"),
            None,
            Err(RouteError::NoAliasIdentity {
                alias: "nobody@masked.example.org".into(),
            }),
        ),
        case(
            "direct refuses a chosen identity that does not send as the alias",
            Some(relay(Direct)),
            alias_reply(&[ALIAS], Some("acc-1")),
            None,
            Some("box"),
            Err(RouteError::NoAliasIdentity {
                alias: ALIAS.into(),
            }),
        ),
        case(
            "reply header from the forwarding mailbox",
            Some(relay(ReplyHeader)),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Ok((
                "box",
                "box@example.com",
                ReplyHeader,
                Some(REPLY_TO),
                vec![
                    mailbox("box@example.com"),
                    Reason::ReplyHeader {
                        address: REPLY_TO.into(),
                    },
                ],
            )),
        ),
        case(
            "reply header without the service's signature",
            Some(Relay {
                signed: false,
                ..relay(ReplyHeader)
            }),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Err(RouteError::NoReplyHeader {
                alias: ALIAS.into(),
            }),
        ),
        case(
            "reply header without a Reply-To",
            Some(Relay {
                reply_to: None,
                ..relay(ReplyHeader)
            }),
            alias_reply(&[ALIAS], None),
            None,
            None,
            Err(RouteError::NoReplyHeader {
                alias: ALIAS.into(),
            }),
        ),
    ];

    let identities = relay_identities();
    let transports = relay_transports();
    for c in cases {
        let got = route_relay(
            Request {
                identities: &identities,
                transports: &transports,
                context: &c.context,
                default_identity: c.default_identity,
                chosen: c.chosen,
            },
            c.relay.as_ref(),
        );
        let relayed = c.relay.is_some() && !matches!(c.context, Context::New);
        let expected = c
            .expected
            .map(|(identity, from, strategy, to, reasons)| Routed {
                route: Route {
                    identity: identity.into(),
                    from: from.into(),
                    name: Some(format!("Name {identity}")),
                    transport: identities
                        .iter()
                        .find(|i| i.id == identity)
                        .unwrap()
                        .transport
                        .clone(),
                    strategy,
                    reasons,
                },
                to: to.map(Into::into),
                alias: relayed.then(|| norm(&c.relay.as_ref().unwrap().hit.alias)),
            });
        assert_eq!(got, expected, "{}", c.name);
    }
}

fn routed(from: &str, strategy: Strategy, to: Option<&str>, alias: Option<&str>) -> Routed {
    Routed {
        route: Route {
            identity: "i".into(),
            from: from.into(),
            name: None,
            transport: "t".into(),
            strategy,
            reasons: vec![],
        },
        to: to.map(Into::into),
        alias: alias.map(Into::into),
    }
}

fn list(addresses: &[&str]) -> Vec<String> {
    addresses.iter().map(|a| a.to_string()).collect()
}

#[test]
fn leak_guard() {
    use Field::{Bcc, Cc, To};
    use Fix::{RemoveRecipient, UseAliasIdentity, UseReverseAlias};
    use LeakReason::*;
    let thread = Thread {
        aliases: vec![ALIAS.into()],
        reverse_aliases: vec!["reply+other@alias.example.net".into()],
        can_reverse: true,
    };
    let reverse = routed(
        "box@example.com",
        Strategy::ReverseAlias,
        Some(REVERSE),
        Some(ALIAS),
    );
    let header = routed(
        "box@example.com",
        Strategy::ReplyHeader,
        Some(REPLY_TO),
        Some(ALIAS),
    );
    let direct_alias = routed(ALIAS, Strategy::Direct, None, Some(ALIAS));
    let direct_real = routed("box@example.com", Strategy::Direct, None, None);
    let other_alias = routed("Old@alias.example.net", Strategy::Direct, None, None);
    let from_alias = routed(ALIAS, Strategy::ReverseAlias, Some(REVERSE), Some(ALIAS));
    let r = || reply(&[], None, None);
    let leak = |recipient: &str, field, reason, fix| Leak {
        recipient: recipient.into(),
        field,
        reason,
        fix,
    };
    type LeakCase<'a> = (
        &'a str,
        &'a Routed,
        Context,
        [&'a [&'a str]; 3],
        Thread,
        Vec<Leak>,
    );
    let cases: Vec<LeakCase> = vec![
        (
            "no alias in the thread",
            &direct_real,
            r(),
            [&["bob@example.org"], &[], &[]],
            Thread::default(),
            vec![],
        ),
        (
            "direct from the alias",
            &direct_alias,
            r(),
            [&["bob@example.org"], &["carol@example.org"], &[]],
            thread.clone(),
            vec![],
        ),
        (
            "direct from another alias of the thread",
            &other_alias,
            r(),
            [&["bob@example.org"], &[], &[]],
            Thread {
                aliases: vec![ALIAS.into(), "old@alias.example.net".into()],
                ..thread.clone()
            },
            vec![],
        ),
        (
            "the routed alias counts when the thread lists none",
            &reverse,
            r(),
            [&[REVERSE], &["carol@example.org"], &[]],
            Thread::default(),
            vec![leak(
                "carol@example.org",
                Cc,
                CcNotReverseAlias,
                RemoveRecipient,
            )],
        ),
        (
            "from the alias under any strategy",
            &from_alias,
            r(),
            [&[REVERSE], &["carol@example.org"], &[]],
            thread.clone(),
            vec![],
        ),
        (
            "reply from the real address",
            &direct_real,
            r(),
            [&["bob@example.org"], &[], &[]],
            thread.clone(),
            vec![leak(
                "bob@example.org",
                To,
                RealAddressFrom,
                UseAliasIdentity,
            )],
        ),
        (
            "forward from the real address",
            &direct_real,
            forward(&[]),
            [&["Bob@example.org"], &["carol@example.org"], &[]],
            thread.clone(),
            vec![
                leak(
                    "bob@example.org",
                    To,
                    ForwardFromRealAddress,
                    UseAliasIdentity,
                ),
                leak(
                    "carol@example.org",
                    Cc,
                    ForwardFromRealAddress,
                    UseAliasIdentity,
                ),
            ],
        ),
        (
            "reply to the reverse alias only",
            &reverse,
            r(),
            [&[REVERSE], &[], &[]],
            thread.clone(),
            vec![],
        ),
        (
            "reply all mixing reverse aliases and plain addresses",
            &reverse,
            r(),
            [
                &[REVERSE, "bob@example.org"],
                &["carol@example.org", "reply+other@alias.example.net"],
                &["dave@example.org"],
            ],
            thread.clone(),
            vec![
                leak("bob@example.org", To, MixedRecipients, UseReverseAlias),
                leak("carol@example.org", Cc, CcNotReverseAlias, UseReverseAlias),
                leak("dave@example.org", Bcc, BccNotReverseAlias, UseReverseAlias),
            ],
        ),
        (
            "no reverse alias can be made",
            &reverse,
            r(),
            [&[REVERSE, "bob@example.org"], &[], &[]],
            Thread {
                can_reverse: false,
                ..thread.clone()
            },
            vec![leak(
                "bob@example.org",
                To,
                MixedRecipients,
                RemoveRecipient,
            )],
        ),
        (
            "plain address without any reverse alias",
            &reverse,
            r(),
            [&["bob@example.org"], &[], &[]],
            thread.clone(),
            vec![leak(
                "bob@example.org",
                To,
                RealAddressFrom,
                UseReverseAlias,
            )],
        ),
        (
            "a recipient leaks once, in its first field",
            &reverse,
            r(),
            [
                &["BOB@example.org"],
                &["bob@example.org"],
                &["bob@example.org"],
            ],
            thread.clone(),
            vec![leak(
                "bob@example.org",
                To,
                RealAddressFrom,
                UseReverseAlias,
            )],
        ),
        (
            "the alias itself is not a leak",
            &reverse,
            r(),
            [&[REVERSE], &[], &["Shop@alias.example.net"]],
            thread.clone(),
            vec![],
        ),
        (
            "a Cc next to the service's Reply-To",
            &header,
            r(),
            [&[REPLY_TO], &["carol@example.org"], &[]],
            Thread {
                can_reverse: false,
                ..thread.clone()
            },
            vec![leak(
                "carol@example.org",
                Cc,
                CcNotReverseAlias,
                RemoveRecipient,
            )],
        ),
        (
            "forward through the mailbox to a plain address",
            &reverse,
            forward(&[]),
            [&["bob@example.org"], &[], &[]],
            thread.clone(),
            vec![leak(
                "bob@example.org",
                To,
                ForwardFromRealAddress,
                UseReverseAlias,
            )],
        ),
    ];
    for (name, routed, context, [to, cc, bcc], thread, expected) in cases {
        let (to, cc, bcc) = (list(to), list(cc), list(bcc));
        let got = leaks(
            routed,
            &context,
            Recipients {
                to: &to,
                cc: &cc,
                bcc: &bcc,
            },
            &thread,
        );
        assert_eq!(got, expected, "{name}");
    }
}

#[test]
fn attendee_is_the_alias() {
    let through = routed(
        "box@example.com",
        Strategy::ReverseAlias,
        Some(REVERSE),
        Some(ALIAS),
    );
    assert_eq!(attendee_address(&through), ALIAS);
    let plain = routed("ada@example.com", Strategy::Direct, None, None);
    assert_eq!(attendee_address(&plain), "ada@example.com");
}

#[test]
fn relay_wire_codes() {
    let error = serde_json::to_value(RouteError::NeedsReverseAlias {
        alias: ALIAS.into(),
        contact: Some("zed@example.net".into()),
    })
    .unwrap();
    assert_eq!(
        error,
        serde_json::json!({"code": "needs_reverse_alias", "alias": ALIAS, "contact": "zed@example.net"})
    );
    assert_eq!(
        serde_json::to_value(Reason::NoAliasMailbox {
            alias: ALIAS.into()
        })
        .unwrap(),
        serde_json::json!({"code": "no_alias_mailbox", "alias": ALIAS})
    );
    let leak = Leak {
        recipient: "bob@example.org".into(),
        field: Field::Cc,
        reason: LeakReason::CcNotReverseAlias,
        fix: Fix::UseReverseAlias,
    };
    assert_eq!(
        serde_json::to_value(leak).unwrap(),
        serde_json::json!({"recipient": "bob@example.org", "field": "cc", "reason": "cc_not_reverse_alias", "fix": "use_reverse_alias"})
    );
}
