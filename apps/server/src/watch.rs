//! Watching accounts for changes: one task per watched account keeps its mail watch (IMAP
//! IDLE or NOTIFY, JMAP event source) and its contacts and calendars watch (JMAP event source)
//! open, and turns what they report into queued syncs.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use holtmail_api::{WatchMode, WatchState};
use holtmail_core::AccountId;
use holtmail_oauth::Provider;
use holtmail_source::{Changed, SourceError, SourceWatch};
use holtmail_source_imap::ImapSettings;
use holtmail_store::{Account, AccountKind, FolderRole};
use tokio::task::JoinHandle;

use crate::accounts;
use crate::dav;
use crate::mailboxes::Runner;
use crate::oauth::OAuthSettings;

/// How long changes are gathered after the first one before a sync is queued.
const DEBOUNCE: Duration = Duration::from_secs(2);
/// How long a debounce gathers at most while changes keep coming.
const DEBOUNCE_MAX: Duration = Duration::from_secs(10);
/// A watch signed in with an OAuth access token is opened again before the token expires.
const OAUTH_REFRESH: Duration = Duration::from_secs(45 * 60);
/// Any other watch is opened again once a day.
const REFRESH: Duration = Duration::from_secs(24 * 60 * 60);

/// Keeps every watched account watched as its settings say, until the process ends. Accounts
/// are looked at every 30 seconds and whenever [`Runner::wake_watches`] is called; a changed
/// account's watch is started again.
pub async fn run(runner: Runner) {
    let mut running: HashMap<AccountId, (String, JoinHandle<()>)> = HashMap::new();
    loop {
        let enabled = runner.services().watch.enabled;
        match accounts::syncable_accounts(&*runner.services().store).await {
            Ok(list) => {
                let mut wanted = HashSet::new();
                for account in list.into_iter().filter(|_| enabled) {
                    if account.kind == AccountKind::Import
                        || accounts::watch_settings(&account).mode == WatchMode::Off
                    {
                        continue;
                    }
                    wanted.insert(account.id);
                    let key = fingerprint(&account);
                    if running.get(&account.id).is_some_and(|(k, _)| *k == key) {
                        continue;
                    }
                    if let Some((_, handle)) = running.remove(&account.id) {
                        handle.abort();
                    }
                    let id = account.id;
                    running.insert(
                        id,
                        (key, tokio::spawn(watch_account(runner.clone(), account))),
                    );
                }
                running.retain(|id, (_, handle)| {
                    if wanted.contains(id) {
                        return true;
                    }
                    handle.abort();
                    runner.set_watch_state(*id, WatchState::Off);
                    runner.set_contacts_watch_state(*id, WatchState::Off);
                    false
                });
            }
            Err(error) => tracing::warn!(%error, "listing accounts for watching failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = runner.watch_settings_changed() => {}
        }
    }
}

/// What makes a watch start again when it changes: settings, sign-in and on or off.
fn fingerprint(account: &Account) -> String {
    format!(
        "{}|{}|{}|{:?}",
        account.settings,
        account.enabled,
        account.kind.as_str(),
        account.sealed_secret
    )
}

async fn watch_account(runner: Runner, account: Account) {
    tokio::join!(
        watch_mail(runner.clone(), account.clone()),
        watch_contacts(runner, account)
    );
}

fn gmail(account: &Account) -> bool {
    OAuthSettings::of(account).and_then(|o| o.provider()) == Some(Provider::Google)
        || ImapSettings::from_json(&account.settings).is_ok_and(|s| {
            let host = s.host.trim().to_ascii_lowercase();
            host.ends_with("gmail.com") || host.ends_with("googlemail.com")
        })
}

/// The folder paths to watch: the chosen ones, or the inbox (the all-mail folder on Gmail) as
/// archived so far, `INBOX` before the first sync. JMAP watches the whole account.
async fn watched_folders(runner: &Runner, account: &Account) -> Vec<String> {
    let settings = accounts::watch_settings(account);
    if settings.mode == WatchMode::Folders {
        return settings.folders;
    }
    if account.kind != AccountKind::Imap {
        return Vec::new();
    }
    let role = if gmail(account) {
        FolderRole::All
    } else {
        FolderRole::Inbox
    };
    let known = runner
        .services()
        .store
        .list_folders(account.archive_id)
        .await
        .unwrap_or_default();
    known
        .into_iter()
        .find(|f| f.folder.account_id == account.id && f.folder.role == Some(role))
        .map_or_else(|| vec!["INBOX".to_owned()], |f| vec![f.folder.path])
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << (failures.clamp(1, 7) - 1)).min(300))
}

fn refresh_after(account: &Account) -> Duration {
    if OAuthSettings::of(account).is_some() {
        OAUTH_REFRESH
    } else {
        REFRESH
    }
}

fn source_reason(error: &SourceError) -> String {
    match error {
        SourceError::Auth(_) => "The mail server refused the sign-in.".into(),
        SourceError::Connection(reason) => format!("Connection lost: {reason}."),
        SourceError::Protocol(reason) => {
            format!("The mail server answered unexpectedly: {reason}.")
        }
        SourceError::Config(reason) => format!("Check the settings: {reason}."),
    }
}

enum Ended {
    Refresh,
    Failed(String, bool),
}

async fn watch_mail(runner: Runner, account: Account) {
    let id = account.id;
    let folders = watched_folders(&runner, &account).await;
    let catch_up = (account.kind == AccountKind::Imap).then(|| folders.clone());
    let mut failures = 0u32;
    runner.set_watch_state(id, WatchState::Starting);
    loop {
        let services = runner.services();
        let opened = match accounts::source_for(
            &account,
            &*services.store,
            &*services.secrets,
            &services.oauth,
        )
        .await
        {
            Ok(source) => source
                .watch(&folders)
                .await
                .map_err(|e| (source_reason(&e), matches!(e, SourceError::Auth(_)))),
            Err(error) => Err((format!("{error:#}"), false)),
        };
        let ended = match opened {
            Ok(None) => {
                tracing::info!(account = %id, "the mail server cannot push; it is synced on its interval");
                runner.set_watch_state(id, WatchState::Polling);
                return;
            }
            Ok(Some(mut watch)) => {
                tracing::info!(account = %id, folders = folders.len(), "watching mail");
                runner.set_watch_state(id, WatchState::Live);
                if failures > 0 {
                    runner.request_mail(id, catch_up.clone());
                }
                failures = 0;
                let ended = pump_mail(&runner, id, &mut *watch, refresh_after(&account)).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), watch.close()).await;
                ended
            }
            Err((reason, auth)) => Ended::Failed(reason, auth),
        };
        match ended {
            Ended::Refresh => continue,
            Ended::Failed(reason, auth) => {
                failures = if auth { failures.max(7) } else { failures + 1 };
                tracing::info!(account = %id, failures, "mail watch interrupted");
                runner.set_watch_state(id, WatchState::Reconnecting(reason));
            }
        }
        tokio::time::sleep(backoff(failures)).await;
    }
}

async fn pump_mail(
    runner: &Runner,
    id: AccountId,
    watch: &mut dyn SourceWatch,
    refresh: Duration,
) -> Ended {
    let until = tokio::time::Instant::now() + refresh;
    loop {
        let first = match tokio::time::timeout_at(until, watch.next()).await {
            Err(_) => return Ended::Refresh,
            Ok(Err(error)) => {
                return Ended::Failed(source_reason(&error), matches!(error, SourceError::Auth(_)));
            }
            Ok(Ok(changed)) => changed,
        };
        let mut changed: Changed = first;
        let gather_until = tokio::time::Instant::now() + DEBOUNCE_MAX;
        let failure = loop {
            let quiet = (tokio::time::Instant::now() + DEBOUNCE).min(gather_until);
            match tokio::time::timeout_at(quiet, watch.next()).await {
                Err(_) => break None,
                Ok(Ok(more)) => changed = changed.merge(more),
                Ok(Err(error)) => {
                    break Some(Ended::Failed(
                        source_reason(&error),
                        matches!(error, SourceError::Auth(_)),
                    ));
                }
            }
        };
        runner.request_mail(id, changed.folders);
        if let Some(failure) = failure {
            return failure;
        }
    }
}

async fn watch_contacts(runner: Runner, account: Account) {
    if !Runner::contacts_on(&account) {
        return;
    }
    let id = account.id;
    let mut failures = 0u32;
    loop {
        let services = runner.services();
        let opened = match dav::source_for(
            &account,
            &*services.store,
            &*services.secrets,
            &services.oauth,
        )
        .await
        {
            Ok(source) => source.watch().await.map_err(|e| dav::reason(&e)),
            Err(error) => Err(format!("{error:#}")),
        };
        let reason = match opened {
            Ok(None) => {
                runner.set_contacts_watch_state(id, WatchState::Off);
                return;
            }
            Ok(Some(mut watch)) => {
                tracing::info!(account = %id, "watching contacts and calendars");
                runner.set_contacts_watch_state(id, WatchState::Live);
                if failures > 0 {
                    runner.request_contacts(id);
                }
                failures = 0;
                let until = tokio::time::Instant::now() + refresh_after(&account);
                let reason = loop {
                    match tokio::time::timeout_at(until, watch.next()).await {
                        Err(_) => break None,
                        Ok(Err(error)) => break Some(dav::reason(&error)),
                        Ok(Ok(_)) => {
                            let gather_until = tokio::time::Instant::now() + DEBOUNCE_MAX;
                            let failed = loop {
                                let quiet =
                                    (tokio::time::Instant::now() + DEBOUNCE).min(gather_until);
                                match tokio::time::timeout_at(quiet, watch.next()).await {
                                    Err(_) => break None,
                                    Ok(Ok(_)) => {}
                                    Ok(Err(error)) => break Some(dav::reason(&error)),
                                }
                            };
                            runner.request_contacts(id);
                            if failed.is_some() {
                                break failed;
                            }
                        }
                    }
                };
                let _ = tokio::time::timeout(Duration::from_secs(5), watch.close()).await;
                match reason {
                    None => continue,
                    Some(reason) => reason,
                }
            }
            Err(reason) => reason,
        };
        failures += 1;
        tracing::info!(account = %id, failures, "contacts and calendars watch interrupted");
        runner.set_contacts_watch_state(id, WatchState::Reconnecting(reason));
        tokio::time::sleep(backoff(failures)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_up_to_five_minutes() {
        assert_eq!(backoff(1), Duration::from_secs(5));
        assert_eq!(backoff(2), Duration::from_secs(10));
        assert_eq!(backoff(4), Duration::from_secs(40));
        assert_eq!(backoff(6), Duration::from_secs(160));
        assert_eq!(backoff(7), Duration::from_secs(300));
        assert_eq!(backoff(100), Duration::from_secs(300));
    }
}
