use std::collections::BTreeMap;

use holtmail_auth_results::{DkimResult, parse_auth_results, parse_dkim_signature};
use holtmail_insight::InsightInput;
use serde::Serialize;

pub(crate) const AUTH_VERSION: u32 = 2;
pub(crate) const MAX_RESULTS: usize = 8;
pub(crate) const MAX_DKIM_PER_RESULT: usize = 8;
pub(crate) const MAX_SIGNATURES: usize = 8;
const MAX_NAME_BYTES: usize = 255;
const COUNTED_FIELDS: [&str; 3] = ["from", "list-unsubscribe", "list-unsubscribe-post"];

#[derive(Serialize)]
struct AuthFacts {
    v: u32,
    results: Vec<ResultFact>,
    signatures: Vec<SignatureFact>,
    fields: BTreeMap<&'static str, usize>,
    sig_cap_hit: bool,
    from_domain: Option<String>,
    from_addresses: usize,
}

#[derive(Serialize)]
struct ResultFact {
    pos: usize,
    authserv: String,
    dkim: Vec<DkimFact>,
}

#[derive(Serialize)]
struct DkimFact {
    d: Option<String>,
    i: Option<String>,
    s: Option<String>,
    b: Option<String>,
}

#[derive(Serialize)]
struct SignatureFact {
    d: Option<String>,
    s: Option<String>,
    b: Option<String>,
    h: BTreeMap<&'static str, usize>,
    l: bool,
}

pub(crate) fn auth_facts(input: &InsightInput) -> String {
    let mut results = Vec::new();
    let mut pass_domains: Vec<String> = Vec::new();
    for (pos, field) in input.headers.iter().enumerate() {
        if results.len() == MAX_RESULTS {
            break;
        }
        if !field.name.eq_ignore_ascii_case("authentication-results") {
            continue;
        }
        let Some(parsed) = parse_auth_results(&field.value) else {
            continue;
        };
        if parsed.authserv_id.len() > MAX_NAME_BYTES {
            continue;
        }
        let dkim: Vec<DkimFact> = parsed
            .dkim()
            .into_iter()
            .filter(DkimResult::is_pass)
            .take(MAX_DKIM_PER_RESULT)
            .map(|result| {
                pass_domains.extend(result.d.clone().filter(|d| fits(d)));
                pass_domains.extend(result.i_domain().filter(|d| fits(d)));
                DkimFact {
                    d: result.d.clone().filter(|d| fits(d)),
                    i: result.i.clone().filter(|i| fits(i)),
                    s: result.s.clone().filter(|s| fits(s)),
                    b: result.b_prefix().map(str::to_owned),
                }
            })
            .collect();
        results.push(ResultFact {
            pos,
            authserv: parsed.authserv_id,
            dkim,
        });
    }

    let signatures: Vec<(bool, SignatureFact)> = input
        .header_values("dkim-signature")
        .filter_map(parse_dkim_signature)
        .map(|signature| {
            let d = signature.d.clone().filter(|d| fits(d));
            let matched = d.as_ref().is_some_and(|d| pass_domains.contains(d));
            let fact = SignatureFact {
                s: signature.s.clone().filter(|s| fits(s)),
                b: signature.b_prefix().map(str::to_owned),
                h: COUNTED_FIELDS
                    .iter()
                    .map(|name| (*name, signature.signed_count(name)))
                    .filter(|(_, count)| *count > 0)
                    .collect(),
                l: signature.l,
                d,
            };
            (matched, fact)
        })
        .collect();
    let matched = signatures.iter().filter(|(m, _)| *m).count();
    let sig_cap_hit = matched > MAX_SIGNATURES;
    let mut room_for_others = MAX_SIGNATURES.saturating_sub(matched);
    let mut matched_kept = 0;
    let signatures = signatures
        .into_iter()
        .filter_map(|(is_match, fact)| {
            let keep = if is_match {
                matched_kept += 1;
                matched_kept <= MAX_SIGNATURES
            } else if room_for_others > 0 {
                room_for_others -= 1;
                true
            } else {
                false
            };
            keep.then_some(fact)
        })
        .collect();

    let fields = COUNTED_FIELDS
        .iter()
        .map(|name| (*name, input.header_values(name).count()))
        .collect();

    let facts = AuthFacts {
        v: AUTH_VERSION,
        results,
        signatures,
        fields,
        sig_cap_hit,
        from_domain: from_address(input).map(|(_, domain)| domain),
        from_addresses: input.from.len(),
    };
    serde_json::to_string(&facts).unwrap_or_default()
}

fn fits(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME_BYTES
}

/// The first `From` address as `(address, domain)`: lowercase, the domain as an A-label.
pub(crate) fn from_address(input: &InsightInput) -> Option<(String, String)> {
    let email = input.from.first()?.email.trim();
    let (local, domain) = email.rsplit_once('@')?;
    if local.is_empty() {
        return None;
    }
    let domain = domain.trim_end_matches('.');
    let domain = match url::Host::parse(domain).ok()? {
        url::Host::Domain(domain) => domain,
        _ => return None,
    };
    if domain.is_empty() || domain.len() > MAX_NAME_BYTES {
        return None;
    }
    Some((format!("{}@{domain}", local.to_lowercase()), domain))
}

#[cfg(test)]
mod tests {
    use holtmail_core::Address;
    use holtmail_insight::HeaderField;
    use serde_json::{Value, json};

    use super::*;

    fn input(headers: Vec<(&str, String)>) -> InsightInput {
        InsightInput {
            headers: headers
                .into_iter()
                .map(|(n, v)| HeaderField::new(n, v))
                .collect(),
            from: vec![Address {
                name: None,
                email: "News@Bücher.Example".into(),
            }],
            ..InsightInput::default()
        }
    }

    fn facts(input: &InsightInput) -> Value {
        serde_json::from_str(&auth_facts(input)).unwrap()
    }

    fn signature(d: &str, b: &str) -> String {
        format!(
            "v=1; a=rsa-sha256; d={d}; s=s1; h=From:List-Unsubscribe:List-Unsubscribe:Subject; bh=x; b={b}"
        )
    }

    #[test]
    fn records_the_documented_shape() {
        let message = input(vec![
            ("Received", "from a by b".into()),
            ("Received", "from c by d".into()),
            ("Received", "from e by f".into()),
            (
                "Authentication-Results",
                "mx.example.org; dkim=pass header.d=xn--bcher-kva.example header.s=s1 header.b=AbC12345xyz; dkim=fail header.d=other.example.com".into(),
            ),
            ("DKIM-Signature", signature("xn--bcher-kva.example", "AbC12345 xyz")),
            ("From", "News@Bücher.Example".into()),
            ("List-Unsubscribe", "<https://example.com/u>".into()),
        ]);
        assert_eq!(
            facts(&message),
            json!({
                "v": 2,
                "results": [{"pos": 3, "authserv": "mx.example.org", "dkim": [
                    {"d": "xn--bcher-kva.example", "i": null, "s": "s1", "b": "AbC12345"}
                ]}],
                "signatures": [{"d": "xn--bcher-kva.example", "s": "s1", "b": "AbC12345",
                    "h": {"from": 1, "list-unsubscribe": 2}, "l": false}],
                "fields": {"from": 1, "list-unsubscribe": 1, "list-unsubscribe-post": 0},
                "sig_cap_hit": false,
                "from_domain": "xn--bcher-kva.example",
                "from_addresses": 1
            })
        );
        assert_eq!(
            from_address(&message),
            Some((
                "news@xn--bcher-kva.example".into(),
                "xn--bcher-kva.example".into()
            ))
        );
    }

    #[test]
    fn counts_from_addresses() {
        let mut message = input(vec![(
            "From",
            "support@signer.example, billing@bank.example".into(),
        )]);
        message.from.push(Address {
            name: None,
            email: "billing@bank.example".into(),
        });
        let counted = facts(&message);
        assert_eq!(counted["fields"]["from"], 1);
        assert_eq!(counted["from_addresses"], 2);
        message.from.clear();
        assert_eq!(facts(&message)["from_addresses"], 0);
    }

    #[test]
    fn keeps_eight_results_topmost_first() {
        let headers = (0..12)
            .map(|n| {
                (
                    "Authentication-Results",
                    format!("mx{n}.example.org; dkim=none"),
                )
            })
            .collect();
        let value = facts(&input(headers));
        let results = value["results"].as_array().unwrap();
        assert_eq!(results.len(), MAX_RESULTS);
        assert_eq!(results[0]["authserv"], "mx0.example.org");
        assert_eq!(results[7]["pos"], 7);
    }

    #[test]
    fn junk_signatures_cannot_push_out_the_matching_one() {
        let mut headers: Vec<(&str, String)> = (0..20)
            .map(|n| {
                (
                    "DKIM-Signature",
                    signature(&format!("junk{n}.example"), "Junk"),
                )
            })
            .collect();
        headers.push(("DKIM-Signature", signature("news.example.com", "Real1234")));
        headers.push((
            "Authentication-Results",
            "mx.example.org; dkim=pass header.i=@news.example.com".into(),
        ));
        let value = facts(&input(headers));
        let signatures = value["signatures"].as_array().unwrap();
        assert_eq!(signatures.len(), MAX_SIGNATURES);
        assert_eq!(signatures[7]["d"], "news.example.com");
        assert_eq!(signatures[0]["d"], "junk0.example");
        assert_eq!(value["sig_cap_hit"], false);
    }

    #[test]
    fn too_many_matching_signatures_hit_the_cap() {
        let mut headers: Vec<(&str, String)> = (0..9)
            .map(|n| {
                (
                    "DKIM-Signature",
                    signature("news.example.com", &format!("Sig{n}")),
                )
            })
            .collect();
        headers.push((
            "Authentication-Results",
            "mx.example.org; dkim=pass header.d=news.example.com".into(),
        ));
        let value = facts(&input(headers));
        assert_eq!(
            value["signatures"].as_array().unwrap().len(),
            MAX_SIGNATURES
        );
        assert_eq!(value["sig_cap_hit"], true);
    }

    #[test]
    fn records_l_and_skips_unreadable_fields() {
        let value = facts(&input(vec![
            ("Authentication-Results", "".into()),
            ("DKIM-Signature", "not a tag list".into()),
            (
                "DKIM-Signature",
                "v=1; d=example.com; s=s1; l=100; h=from; b=abc".into(),
            ),
        ]));
        assert_eq!(value["results"], json!([]));
        assert_eq!(value["signatures"][0]["l"], true);
    }
}
