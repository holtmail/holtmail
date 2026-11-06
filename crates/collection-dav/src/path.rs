use url::Url;

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An item's or collection's id: its path, percent-decoded, so that servers that encode the
/// same path differently from one answer to the next give one id.
pub(crate) fn id(url: &Url) -> String {
    decode(url.path())
}

/// A decoded path encoded again for a request: everything but unreserved characters,
/// sub-delimiters, `:`, `@` and `/` is percent-encoded.
pub(crate) fn encode(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn last_segment(id: &str) -> String {
    id.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_survive_different_encodings() {
        let a = Url::parse("https://dav.example.com/books/ada%40example.com/x%20y.vcf").unwrap();
        let b = Url::parse("https://dav.example.com/books/ada@example.com/x%20y.vcf").unwrap();
        assert_eq!(id(&a), id(&b));
        assert_eq!(id(&a), "/books/ada@example.com/x y.vcf");
        assert_eq!(encode(&id(&a)), "/books/ada@example.com/x%20y.vcf");
        assert_eq!(encode("/a/Ü%.ics"), "/a/%C3%9C%25.ics");
        assert_eq!(decode("%zz%4"), "%zz%4");
        assert_eq!(last_segment("/books/main/"), "main");
    }
}
