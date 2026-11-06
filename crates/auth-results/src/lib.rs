//! Grammar of `Authentication-Results` (RFC 8601) and `DKIM-Signature` (RFC 6376) header
//! fields, without verification.
//!
//! Both parsers take one field value (folded or unfolded), never panic and return owned data.
//! Names are compared without regard to letter case where the RFCs say so and are returned
//! lowercase; values that carry signatures keep their letter case.

mod ar;
mod dkim;
mod lex;

pub use ar::{AuthResults, DkimResult, MethodResult, Property, parse_auth_results};
pub use dkim::{DkimSignature, parse_dkim_signature};

/// The version of this grammar. It changes whenever a parse of the same input could give a
/// different answer, so stored facts derived from it can be recomputed.
pub const VERSION: &str = "1";

/// How many leading characters of a signature (`b=` or `header.b=`) identify it, as RFC 6008
/// recommends for `header.b`.
pub const B_PREFIX_LEN: usize = 8;

fn b_prefix(b: &str) -> &str {
    match b.char_indices().nth(B_PREFIX_LEN) {
        Some((i, _)) => &b[..i],
        None => b,
    }
}
