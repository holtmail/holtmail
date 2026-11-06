/// Why a value could not be read into a contract type.
///
/// The message names only the rule that failed, never the offending content, so an error can be
/// logged as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InsightError {
    /// The value is not in the expected format at all, such as a stored row that is not JSON.
    #[error("unparsable insight data")]
    Unparsable,
    /// The value broke the named rule.
    #[error("rejected insight value: {0}")]
    Decode(&'static str),
}
