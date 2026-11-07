/// The check-digit schemes, by the name `carriers.toml` gives them in `check`.
pub(crate) const SCHEMES: [&str; 6] =
    ["s10", "ups", "usps", "fedex", "dhl_express", "russian_post"];

/// Whether the check digit of `number` is right under `scheme`.
///
/// `number` is the compact uppercase form, no spaces. Returns `None` when `scheme` is unknown or
/// `number` does not have the scheme's shape, `Some(true)` when the check digit matches and
/// `Some(false)` when it does not. Pure, never panics.
///
/// | Scheme | Shape | Check |
/// |---|---|---|
/// | `s10` | 2 letters, 8 digits, check digit, 2 letters | weights 8,6,4,2,3,5,9,7; `11 - sum % 11`, 10 gives 0 and 11 gives 5 |
/// | `ups` | `1Z`, 15 letters or digits, check digit | letters as `(code - 63) % 10`; weights 1,2 from the left; `(10 - sum % 10) % 10` |
/// | `usps` | 20–22 digits | weights 3,1 from the right of the body; `(10 - sum % 10) % 10` |
/// | `fedex` | 12 digits | weights 3,1,7 from the left over the first 11; `sum % 11 % 10` |
/// | `fedex` | 15 digits | weights 3,1 from the right of the body; `(10 - sum % 10) % 10` |
/// | `dhl_express` | 10 digits | the first 9 as a number, `% 7` |
/// | `russian_post` | 14 digits | weights 3,1 from the left over the first 13; `(10 - sum % 10) % 10` |
pub fn check_digit(scheme: &str, number: &str) -> Option<bool> {
    let bytes = number.as_bytes();
    let digits = || bytes.iter().all(u8::is_ascii_digit);
    let (last, body) = bytes.split_last()?;
    let mut check = *last;
    let expected = match scheme {
        "s10" => {
            check = *bytes.get(10)?;
            s10(bytes)?
        }
        "ups" => ups(bytes)?,
        "usps" if (20..=22).contains(&bytes.len()) && digits() => mod10_from_right(body),
        "fedex" if bytes.len() == 12 && digits() => fedex12(body),
        "fedex" if bytes.len() == 15 && digits() => mod10_from_right(body),
        "dhl_express" if bytes.len() == 10 && digits() => {
            let value = body.iter().fold(0u64, |n, d| n * 10 + u64::from(d - b'0'));
            u8::try_from(value % 7).unwrap_or(0)
        }
        "russian_post" if bytes.len() == 14 && digits() => russian_post(body),
        _ => return None,
    };
    Some(check.is_ascii_digit() && check - b'0' == expected)
}

fn digit(b: &u8) -> u32 {
    u32::from(b.saturating_sub(b'0'))
}

fn s10(bytes: &[u8]) -> Option<u8> {
    if bytes.len() != 13 {
        return None;
    }
    let letters = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_uppercase))
    };
    let serial = bytes.get(2..10)?;
    if !letters(0..2) || !letters(11..13) || !bytes.get(2..11)?.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let sum: u32 = serial
        .iter()
        .zip([8, 6, 4, 2, 3, 5, 9, 7])
        .map(|(d, w)| digit(d) * w)
        .sum();
    Some(match 11 - sum % 11 {
        10 => 0,
        11 => 5,
        r => u8::try_from(r).unwrap_or(0),
    })
}

fn ups(bytes: &[u8]) -> Option<u8> {
    if bytes.len() != 18 || !bytes.starts_with(b"1Z") {
        return None;
    }
    let body = bytes.get(2..17)?;
    if !bytes
        .get(2..)?
        .iter()
        .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
    {
        return None;
    }
    let sum: u32 = body
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let value = if b.is_ascii_digit() {
                digit(b)
            } else {
                (u32::from(*b) - 63) % 10
            };
            if i % 2 == 0 { value } else { value * 2 }
        })
        .sum();
    u8::try_from((10 - sum % 10) % 10).ok()
}

fn mod10_from_right(body: &[u8]) -> u8 {
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| digit(d) * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    u8::try_from((10 - sum % 10) % 10).unwrap_or(0)
}

fn fedex12(body: &[u8]) -> u8 {
    let sum: u32 = body
        .iter()
        .zip([3, 1, 7].iter().cycle())
        .map(|(d, w)| digit(d) * w)
        .sum();
    u8::try_from(sum % 11 % 10).unwrap_or(0)
}

fn russian_post(body: &[u8]) -> u8 {
    let sum: u32 = body
        .iter()
        .enumerate()
        .map(|(i, d)| digit(d) * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    u8::try_from((10 - sum % 10) % 10).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn off_by_one(number: &str, at: usize) -> String {
        number
            .char_indices()
            .map(|(i, c)| match c.to_digit(10) {
                Some(d) if i == at => char::from_digit((d + 1) % 10, 10).unwrap_or(c),
                _ => c,
            })
            .collect()
    }

    fn valid_and_not_off_by_one(scheme: &str, number: &str, check_at: usize) {
        assert_eq!(check_digit(scheme, number), Some(true), "{scheme} {number}");
        let changed = off_by_one(number, check_at);
        assert_eq!(
            check_digit(scheme, &changed),
            Some(false),
            "{scheme} {changed}"
        );
    }

    #[test]
    fn s10() {
        valid_and_not_off_by_one("s10", "RA123456785RU", 10);
        valid_and_not_off_by_one("s10", "CP473829103DE", 10);
        valid_and_not_off_by_one("s10", "RA123456785RU", 4);
        assert_eq!(check_digit("s10", "RA12345678RU"), None);
        assert_eq!(check_digit("s10", "Ra123456785RU"), None);
        assert_eq!(check_digit("s10", "RA1234567X5RU"), None);
    }

    #[test]
    fn ups() {
        valid_and_not_off_by_one("ups", "1Z999AA10123456784", 17);
        valid_and_not_off_by_one("ups", "1Z4E8752A038192654", 17);
        valid_and_not_off_by_one("ups", "1Z999AA10123456784", 2);
        assert_eq!(check_digit("ups", "1Z999AA1012345678A"), Some(false));
        assert_eq!(check_digit("ups", "1Y999AA10123456784"), None);
        assert_eq!(check_digit("ups", "1Z999AA1012345678"), None);
    }

    #[test]
    fn usps() {
        valid_and_not_off_by_one("usps", "9400111899223197428497", 21);
        valid_and_not_off_by_one("usps", "9205590164917312751089", 21);
        valid_and_not_off_by_one("usps", "03070200000112345677", 19);
        assert_eq!(check_digit("usps", "0307020000011234567"), None);
    }

    #[test]
    fn fedex() {
        valid_and_not_off_by_one("fedex", "798530184721", 11);
        valid_and_not_off_by_one("fedex", "612900348578", 11);
        valid_and_not_off_by_one("fedex", "041441760228964", 14);
        valid_and_not_off_by_one("fedex", "123456789012343", 14);
        assert_eq!(check_digit("fedex", "1234567890123"), None);
    }

    #[test]
    fn dhl_express() {
        valid_and_not_off_by_one("dhl_express", "4738291051", 9);
        valid_and_not_off_by_one("dhl_express", "1283904753", 9);
        assert_eq!(check_digit("dhl_express", "4738291058"), Some(false));
        assert_eq!(check_digit("dhl_express", "473829105"), None);
    }

    #[test]
    fn russian_post() {
        valid_and_not_off_by_one("russian_post", "14102081078442", 13);
        valid_and_not_off_by_one("russian_post", "63001204567893", 13);
        assert_eq!(check_digit("russian_post", "1410208107844"), None);
    }

    #[test]
    fn unknown_schemes_and_odd_input() {
        assert_eq!(check_digit("royal_mail", "RA123456785RU"), None);
        assert_eq!(check_digit("ups", ""), None);
        assert_eq!(check_digit("fedex", "７９８５３０１８４７２１"), None);
        assert!(SCHEMES.iter().all(|s| check_digit(s, "").is_none()));
    }
}
