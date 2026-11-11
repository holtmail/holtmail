use holtmail_core::{AccountId, Address, MessageId, ThreadId, Timestamp};
use holtmail_index::{Clause, DocumentKind, SearchQuery};

use crate::QueryError;
use crate::words::{Word, words};

/// What the store holds about one message, as a [`Matcher`] reads it.
///
/// A field of type `Option` is `None` when that fact is not known yet (text not extracted,
/// insights not derived); `Some` of an empty value means it is known to be empty. The matcher
/// never guesses a missing fact: a clause that needs one is [`Verdict::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFacts {
    /// The message.
    pub message_id: MessageId,
    /// Its thread.
    pub thread_id: ThreadId,
    /// The accounts whose folders hold it.
    pub accounts: Vec<AccountId>,
    /// Subject, empty when there is none.
    pub subject: String,
    /// `From`, plus the real sender when it came through an alias.
    pub from: Vec<Address>,
    /// `To`.
    pub to: Vec<Address>,
    /// `Cc`.
    pub cc: Vec<Address>,
    /// `Bcc`.
    pub bcc: Vec<Address>,
    /// Header fields in message order as decoded name and value pairs.
    pub headers: Vec<(String, String)>,
    /// Plain text of the body.
    pub body: Option<String>,
    /// Attachments, inline parts included.
    pub attachments: Vec<AttachmentFacts>,
    /// Text extracted from the attachments.
    pub attachment_text: Option<String>,
    /// Paths of the folders the message is in, and their roles (`inbox`, `sent`, ...).
    pub folders: Vec<String>,
    /// Flags from the source, e.g. `\Seen`, `\Flagged`.
    pub flags: Vec<String>,
    /// Labels, e.g. `Projects/2025`.
    pub labels: Vec<String>,
    /// The date lists sort by.
    pub date: Timestamp,
    /// Size of the raw message in bytes.
    pub size: u64,
    /// Names of the insight kinds found, as the index holds them (`code`, `invoice`, ...).
    pub insights: Option<Vec<String>>,
    /// Effective categories as `kind:<name>` and `topic:<name>`.
    pub categories: Option<Vec<String>>,
}

impl MessageFacts {
    /// The values of every header field called `name`, compared case-insensitively, in message
    /// order.
    pub fn header<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn recipients(&self) -> impl Iterator<Item = &Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }

    fn files(&self) -> impl Iterator<Item = &AttachmentFacts> {
        self.attachments.iter().filter(|a| !a.inline)
    }
}

/// One attachment of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentFacts {
    /// File name, when the part has one.
    pub filename: Option<String>,
    /// MIME type, lowercase.
    pub content_type: String,
    /// Decoded size in bytes.
    pub size: u64,
    /// Shown inside the body rather than listed as a file.
    pub inline: bool,
}

/// Whether a message matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// It matches.
    Match,
    /// It does not match.
    NoMatch,
    /// It cannot be decided because a fact the query needs is missing from [`MessageFacts`].
    Unknown,
}

impl Verdict {
    /// True only for [`Verdict::Match`].
    pub fn is_match(self) -> bool {
        self == Verdict::Match
    }

    fn of(value: bool) -> Self {
        if value { Self::Match } else { Self::NoMatch }
    }

    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::NoMatch, _) | (_, Self::NoMatch) => Self::NoMatch,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            _ => Self::Match,
        }
    }

    fn not(self) -> Self {
        match self {
            Self::Match => Self::NoMatch,
            Self::NoMatch => Self::Match,
            Self::Unknown => Self::Unknown,
        }
    }
}

/// A parsed query that tests one message at a time, without the search index.
///
/// For every message whose [`MessageFacts`] are complete it decides exactly what
/// [`holtmail_index::SearchIndex::search`] finds for the same query over the same facts: the
/// same word splitting, case folding, accent removal, prefixes of three letters or more and
/// English and Russian word forms. Clauses combine with Kleene logic: a clause that needs a
/// missing fact is [`Verdict::Unknown`], `Not` keeps it unknown, and the query is unknown unless
/// another clause already rules the message out.
#[derive(Debug, Clone)]
pub struct Matcher {
    query: SearchQuery,
    clauses: Vec<Test>,
}

#[derive(Debug, Clone, Copy)]
enum Field {
    All,
    From,
    To,
    With,
    Subject,
    Body,
    Filename,
    Attachment,
}

#[derive(Debug, Clone)]
enum Test {
    Always(bool),
    Words(Field, Vec<Word>),
    Phrase(Field, Vec<String>),
    HasAttachment,
    In(String),
    Flag(String),
    Label(String),
    Insight(String),
    Category(String),
    Before(i64),
    After(i64),
    Not(Box<Test>),
}

impl Matcher {
    /// A matcher for `query`.
    pub fn new(query: SearchQuery) -> Self {
        let clauses = query.clauses.iter().map(prepare).collect();
        Self { query, clauses }
    }

    /// A matcher for what a person typed, parsed by [`crate::parse`].
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        crate::parse(input).map(Self::new)
    }

    /// The query this matcher tests.
    pub fn query(&self) -> &SearchQuery {
        &self.query
    }

    /// Tests one message. An empty query matches every message.
    pub fn test(&self, facts: &MessageFacts) -> Verdict {
        self.clauses.iter().fold(Verdict::Match, |verdict, test| {
            verdict.and(run(test, facts))
        })
    }
}

fn prepare(clause: &Clause) -> Test {
    let field = |field, text: &str, phrase: bool, stem: bool| {
        let mut found = words(text);
        match found.len() {
            0 => Test::Always(true),
            1 => Test::Words(field, vec![Word::new(found.remove(0), stem)]),
            _ if phrase => Test::Phrase(field, found),
            _ => Test::Words(
                field,
                found.into_iter().map(|w| Word::new(w, stem)).collect(),
            ),
        }
    };
    match clause {
        Clause::Text(text) => field(Field::All, text, false, true),
        Clause::Phrase(text) => field(Field::All, text, true, false),
        Clause::From(text) => field(Field::From, text, true, false),
        Clause::To(text) => field(Field::To, text, true, false),
        Clause::With(text) => field(Field::With, text, true, false),
        Clause::Subject(text) => field(Field::Subject, text, false, true),
        Clause::Body(text) => field(Field::Body, text, false, true),
        Clause::Filename(text) => field(Field::Filename, text, true, false),
        Clause::Attachment(text) => field(Field::Attachment, text, false, true),
        Clause::HasAttachment => Test::HasAttachment,
        Clause::In(folder) => Test::In(folder.to_lowercase()),
        Clause::Flag(flag) => Test::Flag(flag.clone()),
        Clause::Label(label) => Test::Label(label.to_lowercase()),
        Clause::Insight(name) => Test::Insight(name.to_lowercase()),
        Clause::Category(value) => Test::Category(value.to_lowercase()),
        Clause::Before(at) => Test::Before(at.as_millis()),
        Clause::After(at) => Test::After(at.as_millis()),
        Clause::Kind(kind) => Test::Always(*kind == DocumentKind::Mail),
        Clause::Not(inner) => Test::Not(Box::new(prepare(inner))),
    }
}

fn address_text(address: &Address) -> String {
    match &address.name {
        Some(name) => format!("{name} {}", address.email),
        None => address.email.clone(),
    }
}

struct Values {
    tokens: Vec<Vec<String>>,
    complete: bool,
}

fn values(field: Field, facts: &MessageFacts) -> Values {
    let mut texts: Vec<String> = Vec::new();
    let mut complete = true;
    let mut optional = |text: &Option<String>, texts: &mut Vec<String>| match text {
        Some(text) => texts.push(text.clone()),
        None => complete = false,
    };
    let names = || facts.files().filter_map(|a| a.filename.clone());
    match field {
        Field::All => {
            texts.push(facts.subject.clone());
            texts.extend(
                facts
                    .from
                    .iter()
                    .chain(facts.recipients())
                    .map(address_text),
            );
            optional(&facts.body, &mut texts);
            texts.extend(names());
            optional(&facts.attachment_text, &mut texts);
        }
        Field::From => texts.extend(facts.from.iter().map(address_text)),
        Field::To => texts.extend(facts.recipients().map(address_text)),
        Field::With => texts.extend(
            facts
                .from
                .iter()
                .chain(facts.recipients())
                .map(address_text),
        ),
        Field::Subject => texts.push(facts.subject.clone()),
        Field::Body => optional(&facts.body, &mut texts),
        Field::Filename => texts.extend(names()),
        Field::Attachment => optional(&facts.attachment_text, &mut texts),
    }
    Values {
        tokens: texts.iter().map(|t| words(t)).collect(),
        complete,
    }
}

fn found(hit: bool, complete: bool) -> Verdict {
    match (hit, complete) {
        (true, _) => Verdict::Match,
        (false, true) => Verdict::NoMatch,
        (false, false) => Verdict::Unknown,
    }
}

fn known(list: &Option<Vec<String>>, value: &str) -> Verdict {
    match list {
        Some(list) => Verdict::of(list.iter().any(|v| v.to_lowercase() == value)),
        None => Verdict::Unknown,
    }
}

fn run(test: &Test, facts: &MessageFacts) -> Verdict {
    match test {
        Test::Always(value) => Verdict::of(*value),
        Test::Words(field, wanted) => {
            let values = values(*field, facts);
            wanted.iter().fold(Verdict::Match, |verdict, word| {
                let hit = values.tokens.iter().flatten().any(|t| word.finds(t));
                verdict.and(found(hit, values.complete))
            })
        }
        Test::Phrase(field, wanted) => {
            let values = values(*field, facts);
            let hit = values
                .tokens
                .iter()
                .any(|tokens| tokens.windows(wanted.len()).any(|w| w == wanted.as_slice()));
            found(hit, values.complete)
        }
        Test::HasAttachment => Verdict::of(facts.files().next().is_some()),
        Test::In(folder) => Verdict::of(facts.folders.iter().any(|f| f.to_lowercase() == *folder)),
        Test::Flag(flag) => Verdict::of(facts.flags.iter().any(|f| f == flag)),
        Test::Label(label) => Verdict::of(facts.labels.iter().any(|l| l.to_lowercase() == *label)),
        Test::Insight(name) => known(&facts.insights, name),
        Test::Category(value) => known(&facts.categories, value),
        Test::Before(at) => Verdict::of(facts.date.as_millis() < *at),
        Test::After(at) => Verdict::of(facts.date.as_millis() >= *at),
        Test::Not(inner) => run(inner, facts).not(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, email: &str) -> Address {
        Address {
            name: Some(name.into()),
            email: email.into(),
        }
    }

    fn facts() -> MessageFacts {
        MessageFacts {
            message_id: MessageId::new(),
            thread_id: ThreadId::new(),
            accounts: vec![AccountId::new()],
            subject: "Lunch on Friday".into(),
            from: vec![person("Ada Lovelace", "ada@example.com")],
            to: vec![person("Dana", "dana@example.net")],
            cc: Vec::new(),
            bcc: Vec::new(),
            headers: vec![
                ("Subject".into(), "Lunch on Friday".into()),
                ("List-Id".into(), "<lunch.example.com>".into()),
            ],
            body: None,
            attachments: Vec::new(),
            attachment_text: None,
            folders: vec!["INBOX".into(), "inbox".into()],
            flags: vec!["\\Seen".into()],
            labels: Vec::new(),
            date: Timestamp::from_secs(1_741_564_800),
            size: 2_048,
            insights: None,
            categories: None,
        }
    }

    fn verdict(input: &str, facts: &MessageFacts) -> Verdict {
        Matcher::parse(input).unwrap().test(facts)
    }

    #[test]
    fn decides_from_known_facts_even_when_text_is_missing() {
        let facts = facts();
        assert_eq!(verdict("lunch", &facts), Verdict::Match);
        assert_eq!(verdict("from:ada is:read in:inbox", &facts), Verdict::Match);
        assert_eq!(verdict("is:unread", &facts), Verdict::NoMatch);
        assert_eq!(verdict("", &facts), Verdict::Match);
        assert_eq!(verdict("is:mail -is:event", &facts), Verdict::Match);
    }

    #[test]
    fn is_unknown_only_when_a_needed_fact_is_missing() {
        let mut facts = facts();
        assert_eq!(verdict("harbour", &facts), Verdict::Unknown);
        assert_eq!(verdict("-harbour", &facts), Verdict::Unknown);
        assert_eq!(verdict("body:lunch", &facts), Verdict::Unknown);
        assert_eq!(verdict("attachment:menu", &facts), Verdict::Unknown);
        assert_eq!(verdict("has:code", &facts), Verdict::Unknown);
        assert_eq!(verdict("-category:bulk", &facts), Verdict::Unknown);
        assert_eq!(verdict("harbour is:unread", &facts), Verdict::NoMatch);
        assert!(!verdict("harbour", &facts).is_match());

        facts.body = Some("New café near the harbour.".into());
        facts.attachment_text = Some(String::new());
        facts.insights = Some(Vec::new());
        facts.categories = Some(vec!["kind:personal".into()]);
        assert_eq!(verdict("harbour", &facts), Verdict::Match);
        assert_eq!(verdict("-harbour", &facts), Verdict::NoMatch);
        assert_eq!(verdict("attachment:menu", &facts), Verdict::NoMatch);
        assert_eq!(verdict("has:code", &facts), Verdict::NoMatch);
        assert_eq!(
            verdict("-category:bulk is:personal", &facts),
            Verdict::Match
        );
    }

    #[test]
    fn reads_headers_by_name() {
        let facts = facts();
        assert_eq!(
            facts.header("list-id").collect::<Vec<_>>(),
            ["<lunch.example.com>"]
        );
        assert_eq!(facts.header("Precedence").count(), 0);
    }

    #[test]
    fn counts_only_attachments_that_are_not_inline() {
        let mut facts = facts();
        facts.attachments = vec![AttachmentFacts {
            filename: Some("logo.png".into()),
            content_type: "image/png".into(),
            size: 512,
            inline: true,
        }];
        assert_eq!(verdict("has:attachment", &facts), Verdict::NoMatch);
        assert_eq!(verdict("filename:logo", &facts), Verdict::NoMatch);
        facts.attachments[0].inline = false;
        assert_eq!(
            verdict("has:attachment filename:logo", &facts),
            Verdict::Match
        );
    }
}
