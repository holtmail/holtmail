//! WebDAV-Push: registers a Web Push subscription with every CardDAV and CalDAV collection that
//! offers it, renews registrations before they expire, and turns messages posted to
//! `<public_url>/api/push/dav/<id>` into contacts and calendars syncs. Subscription keys live
//! only in memory: after a restart new ones are registered, and the old addresses answer `410`,
//! which tells servers to forget them.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use holtmail_api::WatchMode;
use holtmail_collection::{PushRegistration, PushSubscription};
use holtmail_core::AccountId;
use holtmail_store::Account;
use holtmail_webpush::Keys;

use crate::accounts;
use crate::dav;
use crate::mailboxes::Runner;

/// How long a registration is asked to last; servers should allow at least three days.
const LIFETIME: Duration = Duration::from_secs(3 * 24 * 60 * 60);
/// A registration is renewed when it has less than this left.
const RENEW_BEFORE: Duration = Duration::from_secs(24 * 60 * 60);
/// How often an account's collections are listed again for new ones.
const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);
/// How soon a failed check is tried again.
const RETRY: Duration = Duration::from_secs(30 * 60);

struct Subscription {
    account: AccountId,
    collection: String,
    keys: Keys,
    registration: Option<PushRegistration>,
}

/// The push addresses this process registered, by id.
#[derive(Default)]
pub struct Registry {
    subscriptions: Mutex<HashMap<String, Subscription>>,
}

impl Registry {
    /// The account a message posted to `id` is for, when `id` is one of this process's push
    /// addresses and the message decrypts with its keys.
    pub fn receive(&self, id: &str, body: &[u8]) -> Option<AccountId> {
        let subscriptions = self.subscriptions.lock().expect("push lock");
        let subscription = subscriptions.get(id)?;
        subscription.keys.decrypt(body).ok()?;
        Some(subscription.account)
    }

    /// Whether a collection of `account` has a live registration.
    pub fn watches(&self, account: AccountId) -> bool {
        self.subscriptions
            .lock()
            .expect("push lock")
            .values()
            .any(|s| s.account == account && s.registration.is_some())
    }

    fn of_account(&self, account: AccountId) -> Vec<(String, String, Option<PushRegistration>)> {
        self.subscriptions
            .lock()
            .expect("push lock")
            .iter()
            .filter(|(_, s)| s.account == account)
            .map(|(id, s)| (id.clone(), s.collection.clone(), s.registration.clone()))
            .collect()
    }

    fn accounts(&self) -> HashSet<AccountId> {
        self.subscriptions
            .lock()
            .expect("push lock")
            .values()
            .map(|s| s.account)
            .collect()
    }

    fn remove(&self, id: &str) -> Option<PushRegistration> {
        self.subscriptions
            .lock()
            .expect("push lock")
            .remove(id)
            .and_then(|s| s.registration)
    }
}

fn wanted(account: &Account) -> bool {
    Runner::contacts_on(account)
        && dav::uses_dav(account)
        && accounts::watch_settings(account).mode != WatchMode::Off
}

/// Keeps registrations for every account that wants them, until the process ends. Does nothing
/// without an https `public_url`, since servers must be able to post to it.
pub async fn run(runner: Runner) {
    let Some(base) = runner.services().push_base.clone() else {
        tracing::info!("WebDAV-Push is off: it needs an https public_url that servers can reach");
        return;
    };
    if !runner.services().watch.enabled {
        return;
    }
    let mut next_check: HashMap<AccountId, tokio::time::Instant> = HashMap::new();
    loop {
        match accounts::syncable_accounts(&*runner.services().store).await {
            Ok(list) => {
                let keep: HashMap<AccountId, Account> =
                    list.into_iter().filter(wanted).map(|a| (a.id, a)).collect();
                for gone in runner
                    .push_registry()
                    .accounts()
                    .into_iter()
                    .filter(|id| !keep.contains_key(id))
                {
                    end_all(&runner, gone).await;
                    next_check.remove(&gone);
                }
                let now = tokio::time::Instant::now();
                for (id, account) in &keep {
                    let expiring = runner
                        .push_registry()
                        .of_account(*id)
                        .iter()
                        .any(|(_, _, r)| r.as_ref().and_then(|r| r.expires).is_some_and(soon));
                    if !expiring && next_check.get(id).is_some_and(|at| *at > now) {
                        continue;
                    }
                    let wait = match refresh(&runner, account, &base).await {
                        Ok(()) => RECHECK,
                        Err(error) => {
                            tracing::info!(account = %id, %error, "WebDAV-Push registration failed");
                            RETRY
                        }
                    };
                    next_check.insert(*id, now + wait);
                }
            }
            Err(error) => tracing::warn!(%error, "listing accounts for push failed"),
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

fn soon(at: SystemTime) -> bool {
    at.duration_since(SystemTime::now())
        .map_or(true, |left| left < RENEW_BEFORE)
}

/// Registers with every collection of `account` that has no registration or one about to
/// expire, and drops the registrations of collections the server no longer lists.
async fn refresh(runner: &Runner, account: &Account, base: &str) -> anyhow::Result<()> {
    let services = runner.services();
    let source = dav::source_for(
        account,
        &*services.store,
        &*services.secrets,
        &services.oauth,
    )
    .await?;
    let listed = source
        .collections()
        .await
        .map_err(|e| anyhow::anyhow!(dav::reason(&e)))?;
    let registry = runner.push_registry();
    let existing = registry.of_account(account.id);
    let listed_ids: HashSet<&str> = listed.iter().map(|c| c.id.as_str()).collect();
    for (id, collection, registration) in &existing {
        if !listed_ids.contains(collection.as_str()) {
            registry.remove(id);
            if let Some(registration) = registration {
                let _ = source.unsubscribe(registration).await;
            }
        }
    }
    let mut registered = 0usize;
    for collection in &listed {
        let current = existing.iter().find(|(_, c, r)| {
            c == &collection.id
                && r.as_ref()
                    .is_some_and(|r| r.expires.is_none_or(|at| !soon(at)))
        });
        if current.is_some() {
            registered += 1;
            continue;
        }
        let id = holtmail_webpush::random_id()?;
        let keys = Keys::generate()?;
        let subscription = PushSubscription {
            endpoint: format!("{base}/api/push/dav/{id}"),
            public_key: keys.public_key().to_vec(),
            auth_secret: keys.auth_secret().to_vec(),
            expires: Some(SystemTime::now() + LIFETIME),
        };
        registry.subscriptions.lock().expect("push lock").insert(
            id.clone(),
            Subscription {
                account: account.id,
                collection: collection.id.clone(),
                keys,
                registration: None,
            },
        );
        match source.subscribe(&collection.id, &subscription).await {
            Ok(Some(registration)) => {
                registered += 1;
                if let Some(subscription) = registry
                    .subscriptions
                    .lock()
                    .expect("push lock")
                    .get_mut(&id)
                {
                    subscription.registration = Some(registration);
                }
                for (old, c, previous) in &existing {
                    if c == &collection.id && *old != id {
                        registry.remove(old);
                        if let Some(previous) = previous {
                            let _ = source.unsubscribe(previous).await;
                        }
                    }
                }
            }
            Ok(None) => {
                registry.remove(&id);
            }
            Err(error) => {
                registry.remove(&id);
                tracing::info!(account = %account.id, error = %dav::reason(&error), "a collection refused WebDAV-Push");
            }
        }
    }
    tracing::debug!(account = %account.id, collections = listed.len(), registered, "WebDAV-Push registrations checked");
    Ok(())
}

/// Ends every registration of `account`, telling the server where it can still be reached.
async fn end_all(runner: &Runner, account: AccountId) {
    let registry = runner.push_registry();
    let mine = registry.of_account(account);
    let services = runner.services();
    let source = match services.store.get_account(account).await {
        Ok(Some(stored)) => dav::source_for(
            &stored,
            &*services.store,
            &*services.secrets,
            &services.oauth,
        )
        .await
        .ok(),
        _ => None,
    };
    for (id, _, registration) in mine {
        registry.remove(&id);
        if let (Some(source), Some(registration)) = (&source, registration) {
            let _ = source.unsubscribe(&registration).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receives_only_messages_for_its_keys() {
        let registry = Registry::default();
        let account = AccountId::new();
        let keys = Keys::generate().unwrap();
        let message =
            holtmail_webpush::encrypt(keys.public_key(), keys.auth_secret(), b"<push-message/>")
                .unwrap();
        registry.subscriptions.lock().unwrap().insert(
            "a1".into(),
            Subscription {
                account,
                collection: "/cal/".into(),
                keys,
                registration: None,
            },
        );
        assert_eq!(registry.receive("a1", &message), Some(account));
        assert_eq!(registry.receive("b2", &message), None);
        assert_eq!(registry.receive("a1", b"not encrypted"), None);
        assert!(!registry.watches(account), "nothing registered yet");
        registry
            .subscriptions
            .lock()
            .unwrap()
            .get_mut("a1")
            .unwrap()
            .registration = Some(PushRegistration {
            url: "https://dav.example.com/push/1".into(),
            expires: None,
        });
        assert!(registry.watches(account));
        assert!(registry.remove("a1").is_some());
        assert_eq!(registry.receive("a1", &message), None);
    }

    #[test]
    fn renews_a_day_before_expiry() {
        assert!(soon(SystemTime::now() + Duration::from_secs(60 * 60)));
        assert!(soon(SystemTime::now() - Duration::from_secs(1)));
        assert!(!soon(
            SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60)
        ));
    }
}
