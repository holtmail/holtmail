pub(crate) fn decode_strict(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter();
    while let Some(&b) = iter.next() {
        if b == b'%' {
            let hi = hex(*iter.next()?)?;
            let lo = hex(*iter.next()?)?;
            out.push(hi << 4 | lo);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::decode_strict;

    #[test]
    fn decodes_and_refuses_bad_escapes() {
        assert_eq!(
            decode_strict("https%3A%2F%2Fexample.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(decode_strict("a+b").as_deref(), Some("a+b"));
        assert_eq!(decode_strict("%").as_deref(), None);
        assert_eq!(decode_strict("%4").as_deref(), None);
        assert_eq!(decode_strict("%zz").as_deref(), None);
        assert_eq!(decode_strict("%ff").as_deref(), None);
    }
}
