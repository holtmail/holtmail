use std::sync::{Arc, RwLock};

use holtmail_api::{MailboxError, SyncSchedule};
use holtmail_core::Timestamp;
use holtmail_store::Store;

const SETTING: &str = "sync_interval_minutes";
pub const DEFAULT_MINUTES: u64 = 15;
pub const MAX_MINUTES: u64 = 7 * 24 * 60;

/// The sync interval of accounts without their own: the config's, else the stored one, else 15
/// minutes.
pub struct Schedule {
    current: RwLock<SyncSchedule>,
    store: Arc<dyn Store>,
}

impl Schedule {
    pub async fn open(config: Option<u64>, store: Arc<dyn Store>) -> anyhow::Result<Self> {
        let current = match config {
            Some(minutes) => SyncSchedule {
                minutes,
                from_config: true,
            },
            None => SyncSchedule {
                minutes: store
                    .setting(SETTING)
                    .await?
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(DEFAULT_MINUTES),
                from_config: false,
            },
        };
        Ok(Self {
            current: RwLock::new(current),
            store,
        })
    }

    pub fn get(&self) -> SyncSchedule {
        *self.current.read().expect("schedule lock")
    }

    pub async fn set(&self, minutes: u64) -> Result<SyncSchedule, MailboxError> {
        if self.get().from_config {
            return Err(MailboxError::Invalid(
                "The sync interval is set in the server's config; change it there.".into(),
            ));
        }
        check_minutes(minutes)?;
        self.store
            .set_setting(SETTING, Some(&minutes.to_string()))
            .await
            .map_err(|e| MailboxError::Backend(e.to_string()))?;
        let schedule = SyncSchedule {
            minutes,
            from_config: false,
        };
        *self.current.write().expect("schedule lock") = schedule;
        Ok(schedule)
    }
}

pub fn check_minutes(minutes: u64) -> Result<(), MailboxError> {
    if minutes > MAX_MINUTES {
        return Err(MailboxError::Invalid(
            "Sync at least once a week, or only on request.".into(),
        ));
    }
    Ok(())
}

/// When an account syncing every `minutes` is due next, given when its last run ended in this
/// server's life; `None` when it only syncs on request. An account that has not run yet is due at
/// once.
pub fn next_sync(last: Option<Timestamp>, minutes: u64, now: Timestamp) -> Option<Timestamp> {
    if minutes == 0 {
        return None;
    }
    Some(match last {
        Some(last) => Timestamp::from_millis(last.as_millis() + minutes as i64 * 60_000),
        None => now,
    })
}

#[cfg(test)]
mod tests {
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    #[test]
    fn computes_the_next_sync() {
        let now = Timestamp::from_secs(10_000);
        assert_eq!(next_sync(None, 15, now), Some(now));
        assert_eq!(
            next_sync(Some(Timestamp::from_secs(1_000)), 15, now),
            Some(Timestamp::from_secs(1_900))
        );
        assert_eq!(next_sync(Some(Timestamp::from_secs(1_000)), 0, now), None);
    }

    #[tokio::test]
    async fn stores_the_interval_unless_the_config_sets_it() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
        let schedule = Schedule::open(None, store.clone()).await.unwrap();
        assert_eq!(schedule.get().minutes, DEFAULT_MINUTES);
        schedule.set(60).await.unwrap();
        assert!(schedule.set(MAX_MINUTES + 1).await.is_err());
        let reopened = Schedule::open(None, store.clone()).await.unwrap();
        assert_eq!(reopened.get().minutes, 60);

        let configured = Schedule::open(Some(5), store).await.unwrap();
        assert_eq!(
            configured.get(),
            SyncSchedule {
                minutes: 5,
                from_config: true
            }
        );
        assert!(configured.set(30).await.is_err());
    }
}
