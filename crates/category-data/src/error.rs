/// Why a file could not be read or written.
///
/// The message names only the failed rule and a line number, never row text, so an error can
/// be logged as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DataError {
    /// The bytes are not a valid gzip stream, or it ends early.
    #[error("category data is not valid gzip")]
    Gzip,
    /// The decompressed file is larger than the limit.
    #[error("category data is larger than the limit")]
    TooLarge,
    /// The file has more data rows than the limit.
    #[error("category data has more rows than the limit")]
    TooManyRows,
    /// The header line is missing or broke the named rule.
    #[error("category data header: {0}")]
    Header(&'static str),
    /// A data line broke the named rule. `line` is 1-based and counts the header.
    #[error("category data line {line}: {rule}")]
    Line {
        /// The 1-based line number, the header being line 1.
        line: usize,
        /// The rule the line broke, such as `order` or `domain`.
        rule: &'static str,
    },
}
