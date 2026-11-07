//! Properties of the check-digit schemes.

use holtmail_insight_parcel::check_digit;
use proptest::prelude::*;

/// Every way to finish `body` with one digit at `at`, and which of them validate.
fn completions(scheme: &str, body: &str, at: usize) -> Vec<String> {
    (0..10)
        .map(|d| {
            let mut number = body.to_owned();
            number.insert(at, char::from(b'0' + d));
            number
        })
        .filter(|n| check_digit(scheme, n) == Some(true))
        .collect()
}

fn replace(number: &str, at: usize, digit: u8) -> String {
    number
        .char_indices()
        .map(|(i, c)| if i == at { char::from(b'0' + digit) } else { c })
        .collect()
}

fn digits(len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(0u8..10, len)
        .prop_map(|d| d.into_iter().map(|d| char::from(b'0' + d)).collect())
}

fn upper(len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(b'A'..=b'Z', len).prop_map(|b| String::from_utf8(b).unwrap())
}

fn alnum(len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(prop_oneof![b'0'..=b'9', b'A'..=b'Z'], len)
        .prop_map(|b| String::from_utf8(b).unwrap())
}

/// A body (without check digit), the scheme, and where the check digit goes.
fn bodies() -> impl Strategy<Value = (&'static str, String, usize)> {
    prop_oneof![
        (upper(2), digits(8), upper(2)).prop_map(|(a, d, c)| ("s10", format!("{a}{d}{c}"), 10)),
        alnum(15).prop_map(|b| ("ups", format!("1Z{b}"), 17)),
        (19usize..=21).prop_flat_map(digits).prop_map(|b| {
            let at = b.len();
            ("usps", b, at)
        }),
        digits(11).prop_map(|b| ("fedex", b, 11)),
        digits(14).prop_map(|b| ("fedex", b, 14)),
        digits(9).prop_map(|b| ("dhl_express", b, 9)),
        digits(13).prop_map(|b| ("russian_post", b, 13)),
    ]
}

/// Whether a single-digit change from `old` to `new` at `at` may keep `number` valid under
/// `scheme`: the blind spots of each scheme, documented in the crate README.
fn blind_spot(scheme: &str, number: &str, at: usize, old: u8, new: u8) -> bool {
    let delta = old.abs_diff(new);
    match scheme {
        "s10" => number.as_bytes().get(10) == Some(&b'5'),
        "ups" => at >= 2 && (at - 2) % 2 == 1 && delta == 5,
        "fedex" if number.len() == 12 => number.as_bytes().get(11) == Some(&b'0'),
        "dhl_express" => delta == 7,
        _ => false,
    }
}

proptest! {
    #[test]
    fn exactly_one_check_digit_completes_a_body((scheme, body, at) in bodies()) {
        let valid = completions(scheme, &body, at);
        prop_assert_eq!(valid.len(), 1, "{} {}", scheme, body);
    }

    #[test]
    fn a_single_changed_digit_is_caught((scheme, body, at) in bodies(), pick in any::<usize>(), new in 0u8..10) {
        let number = completions(scheme, &body, at).pop().unwrap();
        let positions: Vec<usize> = number
            .bytes()
            .enumerate()
            .filter(|(i, b)| b.is_ascii_digit() && !(scheme == "ups" && *i < 2))
            .map(|(i, _)| i)
            .collect();
        let position = positions[pick % positions.len()];
        let old = number.as_bytes()[position] - b'0';
        prop_assume!(old != new);
        let changed = replace(&number, position, new);
        if check_digit(scheme, &changed) == Some(true) {
            prop_assert!(
                blind_spot(scheme, &number, position, old, new),
                "{} {} -> {}", scheme, number, changed
            );
        }
    }

    #[test]
    fn check_digit_never_panics(scheme in "[a-z_0-9]{0,14}", number in "\\PC{0,40}") {
        for scheme in [scheme.as_str(), "s10", "ups", "usps", "fedex", "dhl_express", "russian_post"] {
            let _ = check_digit(scheme, &number);
        }
    }
}
