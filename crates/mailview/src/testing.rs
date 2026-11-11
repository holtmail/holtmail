//! Conformance suite every [`MailView`] and [`MailActions`] implementation runs, and an
//! in-memory fake.

mod fake;
mod mime;
mod suite;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use holtmail_core::{FolderId, MessageId, Timestamp};
use holtmail_store::FolderRole;

pub use fake::{FAKE_APPEND_LIMIT, FakeHarness, FakeMailView};
pub use suite::conformance;

use crate::{EmailRef, MailActions, MailView, Viewer};

/// A write an account allows, as its capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Write {
    /// `mail.flags`.
    Flags,
    /// `mail.move`.
    Move,
    /// `mail.delete`.
    Delete,
    /// `mail.expunge`.
    Expunge,
    /// `mail.append`.
    Append,
}

/// An account of a [`Scenario`].
#[derive(Debug, Clone)]
pub struct ScenarioAccount {
    /// Its name.
    pub name: &'static str,
    /// The capabilities it has.
    pub writes: &'static [Write],
    /// Its folders: path with `/` between levels, and role.
    pub folders: &'static [(&'static str, Option<FolderRole>)],
}

/// Where a message of a [`Scenario`] was seen.
#[derive(Debug, Clone)]
pub struct ScenarioPlace {
    /// The account's name.
    pub account: &'static str,
    /// The folder's path.
    pub folder: &'static str,
    /// Its synced flags there.
    pub flags: &'static [&'static str],
    /// Whether it is still there upstream; `false` for a removed place.
    pub live: bool,
}

/// A message of a [`Scenario`].
#[derive(Debug, Clone)]
pub struct ScenarioMessage {
    /// The name the suite knows it by.
    pub name: &'static str,
    /// Its raw bytes, with CRLF line ends.
    pub raw: String,
    /// When the server received it.
    pub received_at: Timestamp,
    /// Its places.
    pub places: Vec<ScenarioPlace>,
}

/// What a fresh archive holds. A harness creates the accounts with their folders and
/// capabilities, then archives the messages in order (so earlier messages were archived
/// earlier) with their places and synced flags.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// The accounts.
    pub accounts: Vec<ScenarioAccount>,
    /// The messages, in archive order.
    pub messages: Vec<ScenarioMessage>,
}

const WORK_FOLDERS: &[(&str, Option<FolderRole>)] = &[
    ("INBOX", Some(FolderRole::Inbox)),
    ("Sent", Some(FolderRole::Sent)),
    ("Drafts", Some(FolderRole::Drafts)),
    ("Trash", Some(FolderRole::Trash)),
    ("Projects/2026", None),
];

const HOME_FOLDERS: &[(&str, Option<FolderRole>)] = &[
    ("INBOX", Some(FolderRole::Inbox)),
    ("Archive", Some(FolderRole::Archive)),
    ("Junk", Some(FolderRole::Junk)),
];

const ALL_WRITES: &[Write] = &[
    Write::Flags,
    Write::Move,
    Write::Delete,
    Write::Expunge,
    Write::Append,
];

/// Invented raw bytes for a test message.
pub fn mail(
    from: &str,
    to: &str,
    subject: &str,
    id: &str,
    date: &str,
    extra: &str,
    body: &str,
) -> String {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: {date}\r\n\
         Message-ID: <{id}@example.com>\r\n{extra}MIME-Version: 1.0\r\n\r\n{body}"
    )
}

fn place(
    account: &'static str,
    folder: &'static str,
    flags: &'static [&'static str],
    live: bool,
) -> ScenarioPlace {
    ScenarioPlace {
        account,
        folder,
        flags,
        live,
    }
}

const ADA: &str = "Ada Lovelace <ada@example.com>";
const BOB: &str = "Bob Builder <bob@example.org>";

impl Scenario {
    /// Two accounts. `Work` allows every write and has `INBOX`, `Sent`, `Drafts`, `Trash` and
    /// `Projects/2026`; `Home` allows none and has `INBOX`, `Archive` and `Junk`. Messages:
    /// `welcome` (Work INBOX, unread), `shared` (Work INBOX seen, Home INBOX seen and flagged),
    /// `report` (multipart with an attachment, Work Projects/2026), `old` (Home Archive),
    /// `gone` (removed from Work INBOX), `reply` (answers `welcome`, Work INBOX, unread) and
    /// `sent` (Work Sent).
    pub fn two_accounts() -> Self {
        let report_body = "--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             Here are the numbers.\r\n--b1\r\nContent-Type: text/csv; name=\"report.csv\"\r\n\
             Content-Disposition: attachment; filename=\"report.csv\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nYSxiLGMKMSwyLDMK\r\n--b1--\r\n";
        let messages = vec![
            ScenarioMessage {
                name: "welcome",
                raw: mail(
                    ADA,
                    BOB,
                    "Welcome aboard",
                    "welcome",
                    "Mon, 02 Nov 2026 09:00:00 +0000",
                    "",
                    "Glad to have you here.\r\n",
                ),
                received_at: Timestamp::from_secs(1_793_610_000),
                places: vec![place("Work", "INBOX", &[], true)],
            },
            ScenarioMessage {
                name: "shared",
                raw: mail(
                    "Carol Jones <carol@example.org>",
                    BOB,
                    "Quarterly numbers",
                    "shared",
                    "Tue, 03 Nov 2026 09:00:00 +0000",
                    "",
                    "The numbers are in.\r\n",
                ),
                received_at: Timestamp::from_secs(1_793_696_400),
                places: vec![
                    place("Work", "INBOX", &["\\Seen"], true),
                    place("Home", "INBOX", &["\\Seen", "\\Flagged"], true),
                ],
            },
            ScenarioMessage {
                name: "report",
                raw: mail(
                    "Dan Smith <dan@example.org>",
                    BOB,
                    "Report attached",
                    "report",
                    "Wed, 04 Nov 2026 09:00:00 +0000",
                    "Content-Type: multipart/mixed; boundary=\"b1\"\r\n",
                    report_body,
                ),
                received_at: Timestamp::from_secs(1_793_782_800),
                places: vec![place("Work", "Projects/2026", &["\\Seen"], true)],
            },
            ScenarioMessage {
                name: "old",
                raw: mail(
                    "Erin Wood <erin@example.org>",
                    BOB,
                    "Old times",
                    "old",
                    "Sun, 01 Mar 2026 09:00:00 +0000",
                    "",
                    "Remember this?\r\n",
                ),
                received_at: Timestamp::from_secs(1_772_355_600),
                places: vec![place("Home", "Archive", &["\\Seen"], true)],
            },
            ScenarioMessage {
                name: "gone",
                raw: mail(
                    "Frank Hill <frank@example.org>",
                    BOB,
                    "Removed upstream",
                    "gone",
                    "Thu, 05 Nov 2026 09:00:00 +0000",
                    "",
                    "Gone from the server.\r\n",
                ),
                received_at: Timestamp::from_secs(1_793_869_200),
                places: vec![place("Work", "INBOX", &["\\Seen"], false)],
            },
            ScenarioMessage {
                name: "reply",
                raw: mail(
                    BOB,
                    ADA,
                    "Re: Welcome aboard",
                    "reply",
                    "Fri, 06 Nov 2026 09:00:00 +0000",
                    "In-Reply-To: <welcome@example.com>\r\n",
                    "Thanks, happy to be here.\r\n",
                ),
                received_at: Timestamp::from_secs(1_793_955_600),
                places: vec![place("Work", "INBOX", &[], true)],
            },
            ScenarioMessage {
                name: "sent",
                raw: mail(
                    ADA,
                    "Carol Jones <carol@example.org>",
                    "Numbers look fine",
                    "sent",
                    "Fri, 06 Nov 2026 12:00:00 +0000",
                    "",
                    "Looks right to me.\r\n",
                ),
                received_at: Timestamp::from_secs(1_793_966_400),
                places: vec![place("Work", "Sent", &["\\Seen"], true)],
            },
        ];
        Self {
            accounts: vec![
                ScenarioAccount {
                    name: "Work",
                    writes: ALL_WRITES,
                    folders: WORK_FOLDERS,
                },
                ScenarioAccount {
                    name: "Home",
                    writes: &[],
                    folders: HOME_FOLDERS,
                },
            ],
            messages,
        }
    }

    /// [`Scenario::two_accounts`] with only `Work` and the places in it; `old` is left out.
    pub fn one_account() -> Self {
        let mut scenario = Self::two_accounts();
        scenario.accounts.retain(|a| a.name == "Work");
        for message in &mut scenario.messages {
            message.places.retain(|p| p.account == "Work");
        }
        scenario.messages.retain(|m| !m.places.is_empty());
        scenario
    }

    /// The raw bytes of the message named `name`.
    pub fn raw(&self, name: &str) -> &str {
        &self
            .messages
            .iter()
            .find(|m| m.name == name)
            .expect("scenario message")
            .raw
    }
}

/// How the instance under test is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    /// `[serve] local_flags`.
    pub local_flags: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self { local_flags: true }
    }
}

/// Plays the upstream servers for the suite.
#[async_trait]
pub trait Driver: Send + Sync {
    /// Settles every open mail change: with `applied`, the server took it and a sync saw the
    /// result; without, the server shows its old state (`superseded_by_server` or refused).
    async fn settle(&self, applied: bool);

    /// A new message (raw bytes with CRLF) arrives in a folder of an account and is archived.
    async fn deliver(&self, account: &str, folder: &str, raw: &str) -> MessageId;
}

/// A fresh instance for one group of checks.
pub struct World {
    /// The view under test.
    pub view: Arc<dyn MailView>,
    /// The actions under test, over the same archive.
    pub actions: Arc<dyn MailActions>,
    /// Plays the upstream servers.
    pub driver: Arc<dyn Driver>,
    /// The archive's owner (or an editor) on a device with `read` and `write:mail`.
    pub editor: Viewer,
    /// The same person on another device with `read` only.
    pub reader: Viewer,
    /// Another person with a reader grant, on a device with `read` and `write:mail`.
    pub member: Viewer,
    /// A person with no grant on the archive, on a device with every scope.
    pub stranger: Viewer,
    /// Folders by account name and path.
    pub folders: HashMap<(String, String), FolderId>,
    /// Messages by scenario name.
    pub messages: HashMap<String, MessageId>,
}

impl World {
    /// The folder at `path` of the account named `account`.
    pub fn folder(&self, account: &str, path: &str) -> FolderId {
        self.folders[&(account.to_string(), path.to_string())]
    }

    /// The scenario message named `name`.
    pub fn email(&self, name: &str) -> EmailRef {
        EmailRef::Message(self.messages[name])
    }
}

/// Starts instances for the suite.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Starts a fresh instance holding `scenario`, configured as `setup` says.
    async fn start(&self, scenario: &Scenario, setup: Setup) -> World;
}
