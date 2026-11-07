/// Why a categorizer could not provide its data.
///
/// The message names only the failed rule or state, never message content, senders or table
/// rows, so an error can be logged as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CategoryError {
    /// The categorizer's data is not available right now, such as a table not yet loaded.
    #[error("category data unavailable")]
    Unavailable,
    /// The data broke the named rule, such as a table that does not parse.
    #[error("invalid category data: {0}")]
    Data(&'static str),
}
