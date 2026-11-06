//! One `holtmail` instance with an owner and an archive whose accounts read contacts and
//! calendars, for the CardDAV and CalDAV suites.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::dav::DavServer;
use crate::{Api, Holtmail, LogScanner, Options};

/// A running instance, its owner's session, one archive and a log scanner.
pub struct PimRun {
    /// The instance.
    pub holtmail: Holtmail,
    /// The owner's session.
    pub api: Api,
    /// The archive the accounts go into.
    pub archive: String,
    /// What must not appear in the logs.
    pub scanner: LogScanner,
}

impl PimRun {
    /// Starts an instance with the default options and an archive named `name`.
    pub async fn start(name: &str) -> Self {
        Self::start_with(name, Options::default()).await
    }

    /// Starts an instance with `options` and an archive named `name`.
    pub async fn start_with(name: &str, options: Options) -> Self {
        let holtmail = Holtmail::start(options).await;
        let api = Api::setup_owner(&holtmail).await;
        let archive = api.create_archive(name).await;
        Self {
            holtmail,
            api,
            archive,
            scanner: LogScanner::new(),
        }
    }

    /// Adds `values`, described as `what`, to the log scanner.
    pub fn secrets(&mut self, what: &str, values: &[&str]) {
        self.scanner = std::mem::take(&mut self.scanner).secrets(what, values.iter().copied());
    }

    /// Adds `user` as an IMAP account on `server`'s mail server, waits for its first mail sync,
    /// and returns the account id. The user and its password join the scanner.
    pub async fn add_mail(&mut self, server: &DavServer, user: &str) -> String {
        self.secrets("a DAV user", &[user, server.password()]);
        let account = self
            .api
            .add_imap(&self.archive, server.imap.account(user))
            .await;
        self.api.settle(&self.archive, &account).await;
        account
    }

    /// Adds `user` as an IMAP account on `server`'s mail server, turns contacts and calendars on
    /// at `url` (the server's own address when `None`), waits for the first contacts run and
    /// checks it ended without an error; returns the account id.
    pub async fn add_dav(&mut self, server: &DavServer, user: &str, url: Option<&str>) -> String {
        let account = self.add_mail(server, user).await;
        let url = url.map_or_else(|| server.url(), str::to_owned);
        let enabled = self.api.enable_contacts(&account, Some(&url)).await.ok();
        assert_eq!(
            enabled["contacts_calendars"]["protocol"], "dav",
            "{enabled}"
        );
        let first = self.api.wait_for_contacts(&self.archive, &account, 1).await;
        assert_eq!(
            first["status"]["contacts_calendars"]["error"],
            Value::Null,
            "{first}"
        );
        account
    }

    /// Syncs `account`, checks that its contacts and calendars run ended without an error, and
    /// returns that run's log lines.
    pub async fn sync(&self, account: &str) -> Vec<String> {
        let synced = self.api.sync_contacts(&self.archive, account).await;
        assert_eq!(
            synced["status"]["contacts_calendars"]["error"],
            Value::Null,
            "{synced}"
        );
        self.log(account).await
    }

    /// The log lines of the latest contacts and calendars run of `account`.
    pub async fn log(&self, account: &str) -> Vec<String> {
        self.api.task_log("contacts_calendars", account).await
    }

    /// Every contact (`contacts`) or event (`events`) of the archive by title, removed ones too.
    pub async fn items(&self, list: &str) -> BTreeMap<String, Value> {
        self.api
            .items(&self.archive, list)
            .await
            .into_iter()
            .map(|i| (i["title"].as_str().unwrap_or_default().to_owned(), i))
            .collect()
    }

    /// The address books and calendars of the archive's accounts by name, removed ones too.
    pub async fn collections(&self) -> BTreeMap<String, Value> {
        self.api
            .collections(&self.archive)
            .await
            .into_iter()
            .filter(|c| c["account_id"].is_string())
            .map(|c| (c["name"].as_str().unwrap_or_default().to_owned(), c))
            .collect()
    }

    /// The calendar's instances between `from` and `to` (RFC 3339), removed ones too.
    pub async fn occurrences(&self, from: &str, to: &str) -> Vec<Value> {
        let ms = |t: &str| {
            chrono::DateTime::parse_from_rfc3339(t)
                .expect("an RFC 3339 time")
                .timestamp_millis()
        };
        let range = self
            .api
            .calendar(&self.archive, ms(from), ms(to), true)
            .await;
        range["occurrences"].as_array().cloned().unwrap_or_default()
    }

    /// Fails the test when anything seeded appears in the instance's logs.
    #[track_caller]
    pub fn check_logs(&self) {
        self.scanner.check(&self.holtmail.logs());
    }
}

/// The line of `log` about the collection named `collection`.
#[track_caller]
pub fn line<'a>(log: &'a [String], collection: &str) -> &'a str {
    log.iter()
        .find(|l| {
            l.strip_prefix(collection)
                .is_some_and(|rest| rest.starts_with(':') || rest.starts_with(" (new):"))
        })
        .unwrap_or_else(|| panic!("no line for {collection} in {log:?}"))
}
