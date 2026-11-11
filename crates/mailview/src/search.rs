use holtmail_core::Timestamp;

/// Where a text condition looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextField {
    /// Headers and body (IMAP `TEXT`, JMAP `text`).
    Any,
    /// The body.
    Body,
    /// `Subject`.
    Subject,
    /// `From`, names and addresses.
    From,
    /// `To`.
    To,
    /// `Cc`.
    Cc,
    /// `Bcc`.
    Bcc,
    /// A header by name, ignoring case; with an empty value, whether the header is there.
    Header(String),
}

/// Which members a search keeps. Dates compare at whole days in UTC for `*Before` (earlier
/// day) and `*Since` (that day or later), as IMAP does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Every member.
    All,
    /// Members every filter keeps; every member when empty.
    And(Vec<Filter>),
    /// Members any filter keeps; none when empty.
    Or(Vec<Filter>),
    /// Members the filter drops.
    Not(Box<Filter>),
    /// Members whose field holds every word of the value, ignoring case.
    Text(TextField, String),
    /// Members whose effective flags in the mailbox hold this flag, ignoring ASCII case.
    Flag(String),
    /// Received on an earlier day.
    ReceivedBefore(Timestamp),
    /// Received that day or later.
    ReceivedSince(Timestamp),
    /// `Date` header on an earlier day.
    SentBefore(Timestamp),
    /// `Date` header that day or later.
    SentSince(Timestamp),
    /// Raw bytes more than this many.
    Larger(u64),
    /// Raw bytes fewer than this many.
    Smaller(u64),
    /// Members whose UID is in one of these inclusive ranges.
    Uids(Vec<(u32, u32)>),
    /// Members whose `MODSEQ` is at least this.
    ModseqSince(u64),
    /// Members with an attachment that is not inline.
    HasAttachment,
}
