use crate::lex::{Cursor, is_ws, split_outside_quotes, strip_comments};

/// One parsed `Authentication-Results` field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthResults {
    /// The authserv-id, lowercase, unquoted. Never empty.
    pub authserv_id: String,
    /// The `authres-version` after the authserv-id, when present.
    pub version: Option<u32>,
    /// Every method result in field order. Empty for `none` and for a field with no
    /// readable result.
    pub results: Vec<MethodResult>,
}

/// One `method=result` entry with its reason and properties.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MethodResult {
    /// The method, lowercase (`dkim`, `spf`, `dmarc`, ...).
    pub method: String,
    /// The method version after `/`, when present.
    pub method_version: Option<u32>,
    /// The result, lowercase (`pass`, `fail`, `none`, ...).
    pub result: String,
    /// The `reason=` value, unquoted, when present.
    pub reason: Option<String>,
    /// Every `ptype.property=value` in field order.
    pub properties: Vec<Property>,
}

/// One `ptype.property=value` entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Property {
    /// The property type, lowercase (`header`, `smtp`, `policy`, ...).
    pub ptype: String,
    /// The property name, lowercase (`d`, `i`, `s`, `b`, `from`, ...).
    pub name: String,
    /// The value as written, unquoted.
    pub value: String,
}

/// The DKIM view of one `dkim` method result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DkimResult {
    /// The result, lowercase.
    pub result: String,
    /// `header.d`, lowercase.
    pub d: Option<String>,
    /// `header.i` as written.
    pub i: Option<String>,
    /// `header.s`, lowercase.
    pub s: Option<String>,
    /// `header.b` as written (a prefix of the signature).
    pub b: Option<String>,
}

impl MethodResult {
    /// The value of the first property `ptype.name`, compared without regard to letter case.
    pub fn property(&self, ptype: &str, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|p| p.ptype.eq_ignore_ascii_case(ptype) && p.name.eq_ignore_ascii_case(name))
            .map(|p| p.value.as_str())
    }

    /// The DKIM view of this result, `None` when the method is not `dkim`.
    pub fn dkim(&self) -> Option<DkimResult> {
        if self.method != "dkim" {
            return None;
        }
        let get = |name| self.property("header", name).map(str::to_owned);
        Some(DkimResult {
            result: self.result.clone(),
            d: get("d").map(|d| d.to_ascii_lowercase()),
            i: get("i"),
            s: get("s").map(|s| s.to_ascii_lowercase()),
            b: get("b"),
        })
    }
}

impl AuthResults {
    /// Every `dkim` result in field order.
    pub fn dkim(&self) -> Vec<DkimResult> {
        self.results.iter().filter_map(MethodResult::dkim).collect()
    }
}

impl DkimResult {
    /// Whether the result is `pass`.
    pub fn is_pass(&self) -> bool {
        self.result == "pass"
    }

    /// The domain part of `header.i` (after its last `@`), lowercase; `None` when it has none.
    pub fn i_domain(&self) -> Option<String> {
        let (_, domain) = self.i.as_deref()?.rsplit_once('@')?;
        (!domain.is_empty()).then(|| domain.to_ascii_lowercase())
    }

    /// The signing domain: `header.d`, else the domain part of `header.i`.
    pub fn signing_domain(&self) -> Option<String> {
        self.d.clone().or_else(|| self.i_domain())
    }

    /// At most the first [`B_PREFIX_LEN`](crate::B_PREFIX_LEN) characters of `header.b`.
    pub fn b_prefix(&self) -> Option<&str> {
        self.b.as_deref().map(crate::b_prefix)
    }
}

/// Parses one `Authentication-Results` value. Comments in parentheses are ignored anywhere,
/// quoted strings are unquoted, and entries that do not fit the grammar are skipped while the
/// rest are kept. `None` only when the value has no authserv-id. Never panics.
pub fn parse_auth_results(value: &str) -> Option<AuthResults> {
    let clean = strip_comments(value);
    let mut segments = split_outside_quotes(&clean, ';').into_iter();
    let mut head = Cursor::new(segments.next()?);
    head.skip_ws();
    let authserv_id = head.value().to_ascii_lowercase();
    if authserv_id.is_empty() {
        return None;
    }
    head.skip_ws();
    let version = number(&head.take_while(|c| !is_ws(c)));
    let results = segments.filter_map(method_result).collect();
    Some(AuthResults {
        authserv_id,
        version,
        results,
    })
}

fn number(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn name_char(c: char) -> bool {
    !is_ws(c) && !matches!(c, '=' | '"' | '.' | '/')
}

fn method_result(segment: &str) -> Option<MethodResult> {
    let mut cur = Cursor::new(segment);
    cur.skip_ws();
    let method = cur.take_while(name_char).to_ascii_lowercase();
    if method.is_empty() {
        return None;
    }
    cur.skip_ws();
    let mut method_version = None;
    if cur.peek() == Some('/') {
        cur.bump();
        cur.skip_ws();
        method_version = number(&cur.take_while(|c| c.is_ascii_digit()));
        cur.skip_ws();
    }
    if cur.bump() != Some('=') {
        return None;
    }
    cur.skip_ws();
    let result = cur.take_while(name_char).to_ascii_lowercase();
    if result.is_empty() {
        return None;
    }
    let mut out = MethodResult {
        method,
        method_version,
        result,
        ..MethodResult::default()
    };
    loop {
        cur.skip_ws();
        let Some(first) = cur.peek() else { break };
        let key = key(&mut cur);
        cur.skip_ws();
        if cur.peek() != Some('=') {
            if key.is_empty() && cur.peek() == Some(first) {
                cur.bump();
            }
            continue;
        }
        cur.bump();
        cur.skip_ws();
        let value = cur.value();
        let key = key.to_ascii_lowercase();
        if key == "reason" {
            out.reason.get_or_insert(value);
        } else if let Some((ptype, name)) = key.split_once('.')
            && !ptype.is_empty()
            && !name.is_empty()
        {
            out.properties.push(Property {
                ptype: ptype.to_owned(),
                name: name.to_owned(),
                value,
            });
        }
    }
    Some(out)
}

fn key(cur: &mut Cursor<'_>) -> String {
    let mut key = cur.take_while(name_char);
    if key.is_empty() {
        return key;
    }
    cur.skip_ws();
    if cur.peek() == Some('.') {
        cur.bump();
        key.push('.');
        cur.skip_ws();
        key.push_str(&cur.take_while(name_char));
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(ptype: &str, name: &str, value: &str) -> Property {
        Property {
            ptype: ptype.into(),
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn reads_a_full_field() {
        let ar = parse_auth_results(
            "MX.Example.ORG 1; dkim=pass (2048-bit key; unprotected) header.d=News.Example.com \
             header.i=@news.example.com header.s=S1 header.b=\"AbC1+/=xyz\"; \
             spf=pass (sender ok) smtp.mailfrom=bounce@example.com; dmarc=pass action=none \
             header.from=example.com; arc=none",
        )
        .unwrap();
        assert_eq!(ar.authserv_id, "mx.example.org");
        assert_eq!(ar.version, Some(1));
        assert_eq!(ar.results.len(), 4);
        assert_eq!(
            ar.results[0].properties,
            [
                prop("header", "d", "News.Example.com"),
                prop("header", "i", "@news.example.com"),
                prop("header", "s", "S1"),
                prop("header", "b", "AbC1+/=xyz"),
            ]
        );
        assert_eq!(
            ar.results[2].properties,
            [prop("header", "from", "example.com")]
        );
        let dkim = ar.dkim();
        assert_eq!(
            dkim,
            [DkimResult {
                result: "pass".into(),
                d: Some("news.example.com".into()),
                i: Some("@news.example.com".into()),
                s: Some("s1".into()),
                b: Some("AbC1+/=xyz".into()),
            }]
        );
        assert!(dkim[0].is_pass());
        assert_eq!(dkim[0].b_prefix(), Some("AbC1+/=x"));
        assert_eq!(dkim[0].i_domain().as_deref(), Some("news.example.com"));
    }

    #[test]
    fn tolerates_spacing_versions_reasons_and_none() {
        let ar = parse_auth_results(
            "\"mx.example.org\" ;dkim / 1 = PASS reason = \"good sig\" header . d = example.com",
        )
        .unwrap();
        assert_eq!(ar.authserv_id, "mx.example.org");
        assert_eq!(ar.version, None);
        let r = &ar.results[0];
        assert_eq!(
            (r.method.as_str(), r.method_version, r.result.as_str()),
            ("dkim", Some(1), "pass")
        );
        assert_eq!(r.reason.as_deref(), Some("good sig"));
        assert_eq!(r.property("HEADER", "D"), Some("example.com"));

        let none = parse_auth_results("mx.example.org; none").unwrap();
        assert!(none.results.is_empty());
        assert_eq!(parse_auth_results("  (only a comment) "), None);
        assert_eq!(parse_auth_results(""), None);
    }

    #[test]
    fn a_commented_pass_is_not_a_pass() {
        let ar = parse_auth_results(
            "mx.example.org (a (nested; comment)); dkim=fail (dkim=pass header.d=a.example.com) header.d=a.example.com",
        )
        .unwrap();
        assert_eq!(ar.results.len(), 1);
        assert_eq!(ar.dkim()[0].result, "fail");
    }

    #[test]
    fn skips_garbage_and_keeps_the_rest() {
        let ar = parse_auth_results(
            "mx.example.org; =pass; dkim; dkim=pass stray header.d=example.com =x .y=z",
        )
        .unwrap();
        assert_eq!(ar.results.len(), 1);
        assert_eq!(
            ar.results[0].properties,
            [prop("header", "d", "example.com")]
        );
    }

    #[test]
    fn signing_domain_falls_back_to_i() {
        let ar = parse_auth_results("mx.example.org; dkim=pass header.i=user@Example.COM").unwrap();
        assert_eq!(
            ar.dkim()[0].signing_domain().as_deref(),
            Some("example.com")
        );
    }
}
