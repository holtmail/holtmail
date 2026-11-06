//! The grammars never panic, and well-formed fields come back as they were written.

use holtmail_auth_results::{parse_auth_results, parse_dkim_signature};
use proptest::prelude::*;

fn render_value(value: &str, quoted: bool) -> String {
    if quoted {
        let escaped: String = value
            .chars()
            .flat_map(|c| match c {
                '"' | '\\' => vec!['\\', c],
                _ => vec![c],
            })
            .collect();
        format!("\"{escaped}\"")
    } else {
        value.to_owned()
    }
}

#[derive(Debug, Clone)]
struct Prop {
    ptype: String,
    name: String,
    value: String,
    quoted: bool,
}

#[derive(Debug, Clone)]
struct Res {
    method: String,
    result: String,
    props: Vec<Prop>,
    comment: bool,
}

fn prop_strategy() -> impl Strategy<Value = Prop> {
    (
        "[a-z]{1,6}",
        "[a-z]{1,3}",
        prop_oneof![
            "[A-Za-z0-9+/=@.]{1,12}".prop_map(|v| (v, false)),
            "[ -~]{0,12}".prop_map(|v| (v, true)),
        ],
    )
        .prop_map(|(ptype, name, (value, quoted))| Prop {
            ptype,
            name,
            value,
            quoted,
        })
}

fn res_strategy() -> impl Strategy<Value = Res> {
    (
        "[a-z][a-z0-9-]{0,5}",
        "[a-z]{1,8}",
        proptest::collection::vec(prop_strategy(), 0..5),
        any::<bool>(),
    )
        .prop_map(|(method, result, props, comment)| Res {
            method,
            result,
            props,
            comment,
        })
}

proptest! {
    #[test]
    fn arbitrary_strings_never_panic(s in any::<String>()) {
        let _ = parse_auth_results(&s);
        let _ = parse_dkim_signature(&s);
    }

    #[test]
    fn grammar_shaped_noise_never_panics(s in "[ ;=.()\"\\\\/a-z0-9@\t\r\n]{0,200}") {
        if let Some(ar) = parse_auth_results(&s) {
            for d in ar.dkim() {
                let _ = (d.signing_domain(), d.b_prefix());
            }
        }
        if let Some(sig) = parse_dkim_signature(&s) {
            let _ = (sig.h_counts(), sig.b_prefix());
        }
    }

    #[test]
    fn authentication_results_round_trip(
        id in "[a-z]{1,8}(\\.[a-z]{1,8}){0,2}",
        version in proptest::option::of(0u32..10),
        results in proptest::collection::vec(res_strategy(), 0..5),
    ) {
        let mut field = id.clone();
        if let Some(v) = version {
            field.push_str(&format!(" {v}"));
        }
        if results.is_empty() {
            field.push_str("; none");
        }
        for r in &results {
            field.push_str(&format!(";\r\n\t{}={}", r.method, r.result));
            if r.comment {
                field.push_str(" (a \"quoted\" (nested; comment) d=x)");
            }
            for p in &r.props {
                field.push_str(&format!(" {}.{}={}", p.ptype, p.name, render_value(&p.value, p.quoted)));
            }
        }
        let parsed = parse_auth_results(&field).expect("authserv-id");
        prop_assert_eq!(parsed.authserv_id, id);
        prop_assert_eq!(parsed.version, version);
        prop_assert_eq!(parsed.results.len(), results.len());
        for (got, want) in parsed.results.iter().zip(&results) {
            prop_assert_eq!(&got.method, &want.method);
            prop_assert_eq!(&got.result, &want.result);
            let props: Vec<_> = got.properties.iter().map(|p| (p.ptype.clone(), p.name.clone(), p.value.clone())).collect();
            let expected: Vec<_> = want.props.iter().map(|p| (p.ptype.clone(), p.name.clone(), p.value.clone())).collect();
            prop_assert_eq!(props, expected);
        }
    }

    #[test]
    fn dkim_signature_round_trip(
        d in "[A-Za-z0-9]{1,8}\\.[a-z]{2,4}",
        s in "[A-Za-z0-9]{1,8}",
        h in proptest::collection::vec("[A-Za-z][A-Za-z-]{0,9}", 0..8),
        b in proptest::collection::vec("[A-Za-z0-9+/]{1,12}", 1..6),
        pad in "={0,2}",
        fws in proptest::collection::vec(prop_oneof![Just(" "), Just("\r\n\t"), Just("\r\n "), Just("")], 8),
        l in any::<bool>(),
    ) {
        let h_field = h.join(&format!("{}:{}", fws[0], fws[1]));
        let b_field = format!("{}{pad}", b.join(fws[2]));
        let mut field = format!("v=1;{}d={d};{}s={s};{}h={h_field};", fws[3], fws[4], fws[5]);
        if l {
            field.push_str(" l=100;");
        }
        field.push_str(&format!("{}b={}{b_field}{}", fws[6], fws[7], fws[2]));
        let sig = parse_dkim_signature(&field).expect("tag-list");
        prop_assert_eq!(sig.d, Some(d.to_ascii_lowercase()));
        prop_assert_eq!(sig.s, Some(s.to_ascii_lowercase()));
        prop_assert_eq!(sig.b, Some(format!("{}{pad}", b.concat())));
        prop_assert_eq!(sig.h, h.iter().map(|n| n.to_ascii_lowercase()).collect::<Vec<_>>());
        prop_assert_eq!(sig.l, l);
    }
}
