use async_trait::async_trait;
use holtmail_core::{EventGroupId, ItemId, Timestamp};
use holtmail_store::*;
use sqlx::AssertSqlSafe;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::pim::{ITEM_COLUMNS, item_summary};
use crate::rows::{backend, get, json, parse};

const SERIES_COLUMNS: &str = "s.item_id AS s_item_id, s.group_id, s.sequence AS s_sequence,
    s.stamp, s.starts_at AS s_starts_at, s.ends_at AS s_ends_at, s.recurring AS s_recurring,
    s.window_from, s.window_to";

fn series(row: &SqliteRow) -> Result<EventSeries, StoreError> {
    let window = match (
        get::<Option<i64>>(row, "window_from")?,
        get::<Option<i64>>(row, "window_to")?,
    ) {
        (Some(from), Some(to)) => Some((Timestamp::from_millis(from), Timestamp::from_millis(to))),
        _ => None,
    };
    Ok(EventSeries {
        item_id: parse(&get::<String>(row, "s_item_id")?)?,
        group_id: parse(&get::<String>(row, "group_id")?)?,
        sequence: get(row, "s_sequence")?,
        stamp: get::<Option<i64>>(row, "stamp")?.map(Timestamp::from_millis),
        starts_at: get::<Option<i64>>(row, "s_starts_at")?.map(Timestamp::from_millis),
        ends_at: get::<Option<i64>>(row, "s_ends_at")?.map(Timestamp::from_millis),
        recurring: get::<i64>(row, "s_recurring")? != 0,
        window,
    })
}

fn filter(query: &OccurrenceQuery) -> Result<(&'static str, Option<String>), StoreError> {
    let removed = if query.removed {
        " AND (?4 IS NULL OR i.collection_id IN (SELECT value FROM json_each(?4)))"
    } else {
        " AND i.removed_at IS NULL AND c.removed_at IS NULL
          AND (?4 IS NULL OR i.collection_id IN (SELECT value FROM json_each(?4)))"
    };
    let collections = query
        .collections
        .as_ref()
        .map(|c| json(&c.iter().map(|c| c.to_string()).collect::<Vec<_>>()))
        .transpose()?;
    Ok((removed, collections))
}

#[async_trait]
impl CalendarStore for SqliteStore {
    async fn replace_series(
        &self,
        item: ItemId,
        series: Option<&EventSeries>,
        occurrences: &[NewOccurrence],
    ) -> Result<(), StoreError> {
        if series.is_some_and(|s| s.item_id != item) {
            return Err(StoreError::Invalid("the series is of another item".into()));
        }
        let id = item.to_string();
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: Option<String> =
            sqlx::query_scalar("SELECT archive_id FROM items WHERE id = ?")
                .bind(&id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        let Some(archive) = archive else {
            return Err(StoreError::NotFound("item"));
        };
        for statement in [
            "DELETE FROM occurrences WHERE item_id = ?",
            "DELETE FROM event_series WHERE item_id = ?",
        ] {
            sqlx::query(statement)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        if let Some(series) = series {
            sqlx::query(
                "INSERT INTO event_series (item_id, archive_id, group_id, sequence, stamp,
                 starts_at, ends_at, recurring, window_from, window_to)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&archive)
            .bind(series.group_id.to_string())
            .bind(series.sequence)
            .bind(series.stamp.map(|t| t.as_millis()))
            .bind(series.starts_at.map(|t| t.as_millis()))
            .bind(series.ends_at.map(|t| t.as_millis()))
            .bind(i64::from(series.recurring))
            .bind(series.window.map(|w| w.0.as_millis()))
            .bind(series.window.map(|w| w.1.as_millis()))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            for o in occurrences {
                sqlx::query(
                    "INSERT INTO occurrences (item_id, archive_id, recurrence_id, starts_at,
                     ends_at, component, start_local, end_local, time_zone, all_day, floating,
                     cancelled, changed) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(&archive)
                .bind(&o.recurrence_id)
                .bind(o.starts_at.as_millis())
                .bind(o.ends_at.as_millis().max(o.starts_at.as_millis()))
                .bind(i64::from(o.component))
                .bind(&o.start_local)
                .bind(&o.end_local)
                .bind(&o.time_zone)
                .bind(i64::from(o.all_day))
                .bind(i64::from(o.floating))
                .bind(i64::from(o.cancelled))
                .bind(i64::from(o.changed))
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
        }
        tx.commit().await.map_err(backend)
    }

    async fn series(&self, item: ItemId) -> Result<Option<EventSeries>, StoreError> {
        sqlx::query(AssertSqlSafe(format!(
            "SELECT {SERIES_COLUMNS} FROM event_series s WHERE s.item_id = ?"
        )))
        .bind(item.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .map(|row| series(&row))
        .transpose()
    }

    async fn occurrences(&self, query: &OccurrenceQuery) -> Result<Vec<Occurrence>, StoreError> {
        let (filter, collections) = filter(query)?;
        let sql = format!(
            "SELECT o.*, i.collection_id, s.group_id, s.sequence AS s_sequence, s.stamp,
                    (i.removed_at IS NOT NULL OR c.removed_at IS NOT NULL) AS removed
             FROM occurrences o
             JOIN items i ON i.id = o.item_id
             JOIN collections c ON c.id = i.collection_id
             JOIN event_series s ON s.item_id = o.item_id
             WHERE o.archive_id = ?1 AND o.ends_at >= ?2 AND o.starts_at < ?3
               AND (o.ends_at > ?2 OR o.ends_at = o.starts_at)
               AND (s.window_from IS NULL OR (s.window_from <= ?2 AND s.window_to >= ?3))
               {}
             ORDER BY o.starts_at, o.item_id, o.recurrence_id",
            filter
        );
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(query.archive_id.to_string())
            .bind(query.from.as_millis())
            .bind(query.to.as_millis())
            .bind(collections)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        rows.iter()
            .map(|row| {
                Ok(Occurrence {
                    item_id: parse(&get::<String>(row, "item_id")?)?,
                    collection_id: parse(&get::<String>(row, "collection_id")?)?,
                    group_id: parse(&get::<String>(row, "group_id")?)?,
                    removed: get::<i64>(row, "removed")? != 0,
                    sequence: get(row, "s_sequence")?,
                    stamp: get::<Option<i64>>(row, "stamp")?.map(Timestamp::from_millis),
                    occurrence: NewOccurrence {
                        recurrence_id: get(row, "recurrence_id")?,
                        starts_at: Timestamp::from_millis(get(row, "starts_at")?),
                        ends_at: Timestamp::from_millis(get(row, "ends_at")?),
                        component: u32::try_from(get::<i64>(row, "component")?).unwrap_or(0),
                        start_local: get(row, "start_local")?,
                        end_local: get(row, "end_local")?,
                        time_zone: get(row, "time_zone")?,
                        all_day: get::<i64>(row, "all_day")? != 0,
                        floating: get::<i64>(row, "floating")? != 0,
                        cancelled: get::<i64>(row, "cancelled")? != 0,
                        changed: get::<i64>(row, "changed")? != 0,
                    },
                })
            })
            .collect()
    }

    async fn uncovered_series(
        &self,
        query: &OccurrenceQuery,
    ) -> Result<Vec<UncoveredSeries>, StoreError> {
        let (filter, collections) = filter(query)?;
        let sql = format!(
            "SELECT {ITEM_COLUMNS}, {SERIES_COLUMNS},
                    (i.removed_at IS NOT NULL OR c.removed_at IS NOT NULL) AS removed
             FROM event_series s
             JOIN items i ON i.id = s.item_id
             JOIN collections c ON c.id = i.collection_id
             LEFT JOIN pim_index p ON p.item_id = i.id
             WHERE s.archive_id = ?1 AND s.window_from IS NOT NULL
               AND NOT (s.window_from <= ?2 AND s.window_to >= ?3)
               AND s.starts_at < ?3 AND (s.ends_at IS NULL OR s.ends_at >= ?2)
               {}
             ORDER BY i.id",
            filter
        );
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(query.archive_id.to_string())
            .bind(query.from.as_millis())
            .bind(query.to.as_millis())
            .bind(collections)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        rows.iter()
            .map(|row| {
                Ok(UncoveredSeries {
                    summary: item_summary(row)?,
                    series: series(row)?,
                    removed: get::<i64>(row, "removed")? != 0,
                })
            })
            .collect()
    }

    async fn group_members(&self, group: EventGroupId) -> Result<Vec<GroupMember>, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {ITEM_COLUMNS}, {SERIES_COLUMNS}, c.removed_at AS collection_removed_at
             FROM event_series s
             JOIN items i ON i.id = s.item_id
             JOIN collections c ON c.id = i.collection_id
             LEFT JOIN pim_index p ON p.item_id = i.id
             WHERE s.group_id = ?
             ORDER BY i.id"
        )))
        .bind(group.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;
        rows.iter()
            .map(|row| {
                Ok(GroupMember {
                    summary: item_summary(row)?,
                    series: series(row)?,
                    collection_removed_at: get::<Option<i64>>(row, "collection_removed_at")?
                        .map(Timestamp::from_millis),
                })
            })
            .collect()
    }
}
