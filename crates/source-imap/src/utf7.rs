//! IMAP's modified UTF-7 for mailbox names (RFC 3501, section 5.1.3).

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// Decodes a mailbox name; sequences that are not valid are kept as they are.
pub(crate) fn decode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else {
            match decode_utf16(encoded) {
                Some(text) => out.push_str(&text),
                None => out.push_str(&rest[start..start + end + 2]),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Encodes a mailbox name: printable ASCII stays, `&` becomes `&-`, anything else goes in
/// `&...-` as modified base64 of UTF-16.
pub(crate) fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut run: Vec<u16> = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut out, &mut run);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut units = [0u16; 2];
            run.extend_from_slice(c.encode_utf16(&mut units));
        }
    }
    flush(&mut out, &mut run);
    out
}

fn flush(out: &mut String, run: &mut Vec<u16>) {
    if run.is_empty() {
        return;
    }
    out.push('&');
    let mut bits = 0u32;
    let mut count = 0;
    for byte in run.drain(..).flat_map(u16::to_be_bytes) {
        bits = (bits << 8) | u32::from(byte);
        count += 8;
        while count >= 6 {
            count -= 6;
            out.push(char::from(ALPHABET[((bits >> count) & 63) as usize]));
        }
        bits &= (1 << count) - 1;
    }
    if count > 0 {
        out.push(char::from(ALPHABET[((bits << (6 - count)) & 63) as usize]));
    }
    out.push('-');
}

fn decode_utf16(encoded: &str) -> Option<String> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut bytes = Vec::new();
    for c in encoded.bytes() {
        let value = ALPHABET.iter().position(|&a| a == c)? as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .collect();
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn encodes_mailbox_names() {
        for name in [
            "INBOX",
            "Tom & Jerry",
            "Отправленные",
            "日本語",
            "Projects/Été",
            "😀 ok",
        ] {
            assert_eq!(decode(&encode(name)), name);
        }
        assert_eq!(encode("Отправленные"), "&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-");
        assert_eq!(encode("Tom & Jerry"), "Tom &- Jerry");
        assert_eq!(encode("Projects/Été"), "Projects/&AMk-t&AOk-");
    }

    #[test]
    fn decodes_mailbox_names() {
        assert_eq!(decode("INBOX"), "INBOX");
        assert_eq!(decode("Tom &- Jerry"), "Tom & Jerry");
        assert_eq!(decode("&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-"), "Отправленные");
        assert_eq!(decode("&ZeVnLIqe-"), "日本語");
        assert_eq!(decode("Projects/&AMk-t&AOk-"), "Projects/Été");
    }

    #[test]
    fn keeps_broken_sequences() {
        assert_eq!(decode("A&B"), "A&B");
        assert_eq!(decode("A&*-B"), "A&*-B");
    }
}
