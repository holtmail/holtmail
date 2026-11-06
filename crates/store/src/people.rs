use holtmail_core::{
    ArchiveId, CollectionId, ItemId, MessageId, PersonId, PersonOverrideId, ThreadId, Timestamp,
    UserId,
};
use serde::{Deserialize, Serialize};

use crate::{Attachment, PageCursor};

/// One address as the archive's mail uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailAddress {
    /// The address, trimmed and lowercased.
    pub email: String,
    /// The display name it was written with most often; ties go to the name that sorts first.
    pub name: Option<String>,
    /// Messages it appears in, in `From`, `To`, `Cc` or `Bcc`.
    pub messages: u64,
    /// Messages it sent: it is in `From`.
    pub from_them: u64,
    /// Messages the archive's owner sent to it: it is a recipient, and the `From` is one of
    /// [`MailAddresses::own`].
    pub from_me: u64,
    /// The date of the oldest of its messages.
    pub first_at: Timestamp,
    /// The date of the newest of its messages.
    pub last_at: Timestamp,
}

/// Every address of an archive's mail, and which of them are the owner's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailAddresses {
    /// The owner's addresses, sorted: the addresses of the archive's accounts, and every `From`
    /// of a message located in a folder with the role `sent` or carrying the label `Sent`.
    pub own: Vec<String>,
    /// Every address, sorted by address.
    pub addresses: Vec<MailAddress>,
}

/// A contact card that belongs to a person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonCard {
    /// The contact.
    pub item_id: ItemId,
    /// Its address book.
    pub collection_id: CollectionId,
    /// Whether the contact was marked removed from its source.
    pub removed: bool,
}

/// Why two cards are the same person: they share an email address or a phone number. For every
/// key several cards share, the first card with it (by item id) links to each other one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonLink {
    /// One card.
    pub from: ItemId,
    /// The other card.
    pub to: ItemId,
    /// What they share: `email:<address>` or `tel:<E.164>`.
    pub key: String,
}

/// A person's mail, summed over their addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonMail {
    /// Messages one of their addresses appears in; a message to two of them counts twice.
    pub messages: u64,
    /// Messages they sent.
    pub from_them: u64,
    /// Messages the archive's owner sent to them.
    pub from_me: u64,
    /// The oldest message's date.
    pub first_at: Timestamp,
    /// The newest message's date.
    pub last_at: Timestamp,
}

/// A person: contact cards of an archive grouped by a shared email address or phone number,
/// and the mail of their addresses, or an address of the mail alone. Derived data: rebuilt from
/// the contacts and the messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// Id, stable while the group keeps its smallest key.
    pub id: PersonId,
    /// The archive.
    pub archive_id: ArchiveId,
    /// The name to show.
    pub name: Option<String>,
    /// The email address to show first.
    pub email: Option<String>,
    /// The phone number to show first, in E.164 form when known.
    pub phone: Option<String>,
    /// Every email address, lowercased: the cards' and the mail's, sorted.
    pub emails: Vec<String>,
    /// The addresses their mail and meetings are found by: [`Person::emails`] without the
    /// archive owner's addresses, sorted.
    pub mail_emails: Vec<String>,
    /// Every phone number of the cards in E.164 form, sorted.
    pub phones: Vec<String>,
    /// The cards, by item id; empty for a person from mail alone.
    pub cards: Vec<PersonCard>,
    /// The shared keys that joined the cards.
    pub links: Vec<PersonLink>,
    /// The mail of their addresses; `None` when none appears in mail.
    pub mail: Option<PersonMail>,
    /// Whether the cards not marked removed disagree on a field.
    pub differences: bool,
    /// Whether every card was marked removed and no mail keeps the person.
    pub removed: bool,
}

/// Which people a list holds by where they come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonSource {
    /// People with at least one card.
    Contacts,
    /// People from mail alone, without a card.
    Mail,
    /// People with a card in this address book.
    Collection(CollectionId),
}

/// Which people to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonQuery {
    /// The archive.
    pub archive_id: ArchiveId,
    /// Only people whose name, an email address or a phone number contains this text, compared
    /// without regard to letter case.
    pub text: Option<String>,
    /// Only people from this source.
    pub source: Option<PersonSource>,
    /// Only people whose cards disagree.
    pub differences: bool,
    /// Continue after this point of a previous page.
    pub after: Option<PageCursor>,
    /// At most this many people; implementations cap it at 500.
    pub limit: u32,
}

/// Messages of a year.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct YearCount {
    /// The year, by the messages' dates in UTC.
    pub year: i32,
    /// How many.
    pub messages: u64,
}

/// A conversation with some addresses in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadActivity {
    /// The conversation.
    pub thread_id: ThreadId,
    /// Its messages with one of the addresses.
    pub messages: u64,
    /// The newest of those.
    pub latest: MessageId,
    /// Its date.
    pub last_at: Timestamp,
}

/// An attachment of a message with some addresses in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangedAttachment {
    /// The message.
    pub message_id: MessageId,
    /// Its date.
    pub date: Timestamp,
    /// Whether one of the addresses sent it.
    pub from_them: bool,
    /// The attachment.
    pub attachment: Attachment,
}

/// The mail of a set of addresses in an archive: every message one of them appears in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correspondence {
    /// Messages, each once.
    pub messages: u64,
    /// Those sent from one of the addresses.
    pub from_them: u64,
    /// Those with one of the addresses among the recipients.
    pub to_them: u64,
    /// The oldest message's date.
    pub first_at: Option<Timestamp>,
    /// The newest message's date.
    pub last_at: Option<Timestamp>,
    /// Messages per year, oldest year first.
    pub years: Vec<YearCount>,
    /// The conversations, the most recently active first.
    pub threads: Vec<ThreadActivity>,
    /// Attachments that are neither inline nor calendar parts (`text/calendar`,
    /// `application/ics`, which are meetings), the newest message first, then by ordinal.
    pub attachments: Vec<ExchangedAttachment>,
}

/// What a [`PersonOverride`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonOverrideKind {
    /// Its keys are one person, whether or not they share an address or a phone number.
    Join,
    /// Its keys are never the same person as its `from` keys.
    Split,
}

impl PersonOverrideKind {
    /// `join` or `split`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Split => "split",
        }
    }

    /// Parses [`PersonOverrideKind::as_str`] output.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "join" => Some(Self::Join),
            "split" => Some(Self::Split),
            _ => None,
        }
    }
}

/// A split or join of people made by hand: the only stored state about people. It names things
/// that survive a rebuild of the people, never a person id: cards as `item:<item id>`, and
/// addresses of people from mail as `email:<lowercase address>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonOverride {
    /// Id; later overrides sort after earlier ones.
    pub id: PersonOverrideId,
    /// The archive.
    pub archive_id: ArchiveId,
    /// Join or split.
    pub kind: PersonOverrideKind,
    /// A join: the keys that are one person, at least two. A split: the cards moved out.
    pub keys: Vec<String>,
    /// A split: the cards the moved ones were separated from. Empty for a join.
    pub from: Vec<String>,
    /// When it was made.
    pub created_at: Timestamp,
    /// Who made it; `None` from the command line or for a removed user.
    pub created_by: Option<UserId>,
}
