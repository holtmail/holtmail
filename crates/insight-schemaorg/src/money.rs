/// Digits a price may have in all, integer and fraction together.
pub(crate) const MAX_PRICE_DIGITS: usize = 18;

/// `text` as an amount in minor units of a currency with `exponent` fraction digits.
///
/// Accepts an optional leading `-`, ASCII digits, and optionally `.` followed by at least one
/// digit, with surrounding whitespace. Returns `None` for anything else: an exponent, a `+`,
/// thousands separators, more than [`MAX_PRICE_DIGITS`] digits, more fraction digits than
/// `exponent`, or an amount outside `i64`. Never panics and never uses a float.
pub(crate) fn parse_decimal(text: &str, exponent: u8) -> Option<i64> {
    let text = text.trim();
    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (rest, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || fraction.is_some_and(|f| !digits(f)) {
        return None;
    }
    let fraction = fraction.unwrap_or("");
    if whole.len() + fraction.len() > MAX_PRICE_DIGITS || fraction.len() > usize::from(exponent) {
        return None;
    }
    let mut minor: i64 = 0;
    for b in whole.bytes().chain(fraction.bytes()) {
        minor = minor.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
    }
    for _ in fraction.len()..usize::from(exponent) {
        minor = minor.checked_mul(10)?;
    }
    if negative {
        minor.checked_neg()
    } else {
        Some(minor)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn reads_plain_decimals() {
        assert_eq!(parse_decimal("12.50", 2), Some(1250));
        assert_eq!(parse_decimal("12.5", 2), Some(1250));
        assert_eq!(parse_decimal("12", 2), Some(1200));
        assert_eq!(parse_decimal(" 0.05 ", 2), Some(5));
        assert_eq!(parse_decimal("-3.25", 2), Some(-325));
        assert_eq!(parse_decimal("12800", 0), Some(12800));
        assert_eq!(parse_decimal("4.125", 3), Some(4125));
        assert_eq!(parse_decimal("1.5", 4), Some(15000));
    }

    #[test]
    fn refuses_everything_else() {
        for bad in [
            "", "-", ".", "12.", ".5", "1e3", "1E3", "1,000.00", "1 000", "+5", "12.345", "--1",
            "0x10", "12.5.0", "١٢", "NaN", "inf",
        ] {
            assert_eq!(parse_decimal(bad, 2), None, "{bad:?}");
        }
        assert_eq!(parse_decimal("12.5", 0), None);
        assert_eq!(parse_decimal(&"9".repeat(19), 0), None);
        assert_eq!(parse_decimal("999999999999999999", 2), None);
        assert_eq!(parse_decimal("99999999999999999.9", 2), None);
        assert_eq!(
            parse_decimal("999999999999999999", 0),
            Some(999_999_999_999_999_999)
        );
    }

    proptest! {
        #[test]
        fn never_panics(text in ".{0,40}", exponent in 0u8..=4) {
            let _ = parse_decimal(&text, exponent);
        }

        #[test]
        fn round_trips(minor in -999_999_999_999_999i64..=999_999_999_999_999, exponent in 0u8..=4) {
            let scale = 10i64.pow(u32::from(exponent));
            let whole = minor.unsigned_abs() / scale.unsigned_abs();
            let fraction = minor.unsigned_abs() % scale.unsigned_abs();
            let sign = if minor < 0 { "-" } else { "" };
            let text = if exponent == 0 {
                format!("{sign}{whole}")
            } else {
                format!("{sign}{whole}.{fraction:0width$}", width = usize::from(exponent))
            };
            prop_assert_eq!(parse_decimal(&text, exponent), Some(minor));
        }

        #[test]
        fn refuses_extra_fraction_digits(whole in 0u32..1_000_000, extra in "[0-9]{1,6}", exponent in 0u8..=4) {
            let fraction = format!("{}{extra}", "0".repeat(usize::from(exponent)));
            prop_assert_eq!(parse_decimal(&format!("{whole}.{fraction}"), exponent), None);
        }

        #[test]
        fn refuses_exponents(mantissa in 1u32..100_000, power in 0u8..20, letter in "[eE]") {
            prop_assert_eq!(parse_decimal(&format!("{mantissa}{letter}{power}"), 2), None);
        }
    }
}
