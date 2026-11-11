use async_trait::async_trait;
use holtmail_core::{ArchiveId, FlowId, FlowRunId, IdentityId, MessageId, Timestamp, UserId};
use holtmail_store::*;
use sqlx::sqlite::SqliteRow;

use crate::SqliteStore;
use crate::rows::{backend, get, json, parse};

fn flow(row: &SqliteRow) -> Result<FlowRecord, StoreError> {
    let mode: String = get(row, "mode")?;
    let paused = match get::<Option<String>>(row, "paused_reason")? {
        Some(reason) => Some(FlowPause {
            reason,
            at: Timestamp::from_millis(get::<Option<i64>>(row, "paused_at")?.unwrap_or(0)),
        }),
        None => None,
    };
    Ok(FlowRecord {
        id: parse(&get::<String>(row, "id")?)?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        name: get(row, "name")?,
        position: u32::try_from(get::<i64>(row, "position")?).unwrap_or(0),
        mode: FlowMode::parse(&mode)
            .ok_or_else(|| StoreError::Invalid(format!("unknown flow mode {mode:?}")))?,
        paused,
        version: u32::try_from(get::<i64>(row, "version")?).unwrap_or(0),
        trigger: get(row, "trigger")?,
        saved_by: parse(&get::<String>(row, "saved_by")?)?,
        created_at: Timestamp::from_millis(get(row, "created_at")?),
        updated_at: Timestamp::from_millis(get(row, "updated_at")?),
    })
}

fn version(row: &SqliteRow) -> Result<FlowVersion, StoreError> {
    Ok(FlowVersion {
        flow_id: parse(&get::<String>(row, "flow_id")?)?,
        version: u32::try_from(get::<i64>(row, "version")?).unwrap_or(0),
        definition: get(row, "definition")?,
        saved_by: parse(&get::<String>(row, "saved_by")?)?,
        saved_at: Timestamp::from_millis(get(row, "saved_at")?),
    })
}

fn value(text: Option<String>) -> Result<Option<serde_json::Value>, StoreError> {
    text.map(|t| serde_json::from_str(&t).map_err(backend))
        .transpose()
}

fn run(row: &SqliteRow) -> Result<FlowRun, StoreError> {
    let status: String = get(row, "status")?;
    Ok(FlowRun {
        id: parse(&get::<String>(row, "id")?)?,
        new: NewFlowRun {
            flow_id: parse(&get::<String>(row, "flow_id")?)?,
            archive_id: parse(&get::<String>(row, "archive_id")?)?,
            version: u32::try_from(get::<i64>(row, "version")?).unwrap_or(0),
            event_id: get(row, "event_id")?,
            trigger: get(row, "trigger")?,
            test: get::<i64>(row, "test")? != 0,
            message_id: get::<Option<String>>(row, "message_id")?
                .map(|m| parse(&m))
                .transpose()?,
            sender: get(row, "sender")?,
            started_at: Timestamp::from_millis(get(row, "started_at")?),
        },
        status: FlowRunStatus::parse(&status)
            .ok_or_else(|| StoreError::Invalid(format!("unknown run status {status:?}")))?,
        plan: value(get(row, "plan")?)?,
        results: value(Some(get(row, "results")?))?.unwrap_or_default(),
        error: get(row, "error")?,
        acted: get::<i64>(row, "acted")? != 0,
        finished_at: get::<Option<i64>>(row, "finished_at")?.map(Timestamp::from_millis),
    })
}

fn event(row: &SqliteRow) -> Result<RuleEvent, StoreError> {
    Ok(RuleEvent {
        id: get(row, "id")?,
        archive_id: parse(&get::<String>(row, "archive_id")?)?,
        trigger: get(row, "trigger")?,
        message_id: get::<Option<String>>(row, "message_id")?
            .map(|m| parse(&m))
            .transpose()?,
        item_id: get::<Option<String>>(row, "item_id")?
            .map(|m| parse(&m))
            .transpose()?,
        live: get::<i64>(row, "live")? != 0,
        payload: value(Some(get(row, "payload")?))?.unwrap_or_default(),
        at: Timestamp::from_millis(get(row, "at")?),
    })
}

fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

impl SqliteStore {
    async fn flow_row(&self, id: FlowId) -> Result<Option<FlowRecord>, StoreError> {
        sqlx::query("SELECT * FROM flows WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(flow)
            .transpose()
    }
}

#[async_trait]
impl RuleStore for SqliteStore {
    async fn create_flow(&self, new: &NewFlow) -> Result<FlowRecord, StoreError> {
        let id = FlowId::new();
        {
            let _write = self.writer.lock().await;
            let mut tx = self.pool.begin().await.map_err(backend)?;
            let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM archives WHERE id = ?")
                .bind(new.archive_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
            if exists.is_none() {
                return Err(StoreError::NotFound("archive"));
            }
            let position: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM flows WHERE archive_id = ?")
                    .bind(new.archive_id.to_string())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(backend)?;
            sqlx::query(
                "INSERT INTO flows (id, archive_id, name, position, mode, version, trigger, saved_by,
                   created_at, updated_at)
                 VALUES (?, ?, ?, ?, 'test', 1, ?, ?, ?, ?)",
            )
            .bind(id.to_string())
            .bind(new.archive_id.to_string())
            .bind(&new.name)
            .bind(position)
            .bind(&new.trigger)
            .bind(new.saved_by.to_string())
            .bind(new.at.as_millis())
            .bind(new.at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query(
                "INSERT INTO flow_versions (flow_id, version, definition, saved_by, saved_at)
                 VALUES (?, 1, ?, ?, ?)",
            )
            .bind(id.to_string())
            .bind(&new.definition)
            .bind(new.saved_by.to_string())
            .bind(new.at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            tx.commit().await.map_err(backend)?;
        }
        self.flow_row(id).await?.ok_or(StoreError::NotFound("flow"))
    }

    async fn get_flow(&self, id: FlowId) -> Result<Option<FlowRecord>, StoreError> {
        self.flow_row(id).await
    }

    async fn list_flows(&self, archive: ArchiveId) -> Result<Vec<FlowRecord>, StoreError> {
        sqlx::query("SELECT * FROM flows WHERE archive_id = ? ORDER BY position, id")
            .bind(archive.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(flow)
            .collect()
    }

    async fn save_flow(
        &self,
        id: FlowId,
        expected: u32,
        definition: &str,
        trigger: &str,
        user: UserId,
        at: Timestamp,
    ) -> Result<FlowSave, StoreError> {
        {
            let _write = self.writer.lock().await;
            let mut tx = self.pool.begin().await.map_err(backend)?;
            let current: Option<i64> = sqlx::query_scalar("SELECT version FROM flows WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
            let current = current.ok_or(StoreError::NotFound("flow"))?;
            if current != i64::from(expected) {
                return Ok(FlowSave::Stale {
                    current: u32::try_from(current).unwrap_or(0),
                });
            }
            let next = current + 1;
            sqlx::query(
                "INSERT INTO flow_versions (flow_id, version, definition, saved_by, saved_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id.to_string())
            .bind(next)
            .bind(definition)
            .bind(user.to_string())
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            sqlx::query(
                "UPDATE flows SET version = ?, trigger = ?, saved_by = ?, updated_at = ?,
                   paused_reason = NULL, paused_at = NULL
                 WHERE id = ?",
            )
            .bind(next)
            .bind(trigger)
            .bind(user.to_string())
            .bind(at.as_millis())
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
            tx.commit().await.map_err(backend)?;
        }
        self.flow_row(id)
            .await?
            .map(FlowSave::Saved)
            .ok_or(StoreError::NotFound("flow"))
    }

    async fn change_flow(
        &self,
        id: FlowId,
        change: &FlowChange,
        at: Timestamp,
    ) -> Result<FlowRecord, StoreError> {
        {
            let _write = self.writer.lock().await;
            let mut tx = self.pool.begin().await.map_err(backend)?;
            let archive: Option<String> =
                sqlx::query_scalar("SELECT archive_id FROM flows WHERE id = ?")
                    .bind(id.to_string())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(backend)?;
            let archive = archive.ok_or(StoreError::NotFound("flow"))?;
            if let Some(name) = &change.name {
                sqlx::query("UPDATE flows SET name = ? WHERE id = ?")
                    .bind(name)
                    .bind(id.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(backend)?;
            }
            if let Some(mode) = change.mode {
                sqlx::query(
                    "UPDATE flows SET mode = ?, paused_reason = NULL, paused_at = NULL WHERE id = ?",
                )
                .bind(mode.as_str())
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            }
            if let Some(position) = change.position {
                let mut order: Vec<String> = sqlx::query_scalar(
                    "SELECT id FROM flows WHERE archive_id = ? AND id != ? ORDER BY position, id",
                )
                .bind(&archive)
                .bind(id.to_string())
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?;
                let at = usize::try_from(position)
                    .unwrap_or(usize::MAX)
                    .min(order.len());
                order.insert(at, id.to_string());
                for (index, flow) in order.iter().enumerate() {
                    sqlx::query("UPDATE flows SET position = ? WHERE id = ?")
                        .bind(i64::try_from(index).unwrap_or(i64::MAX))
                        .bind(flow)
                        .execute(&mut *tx)
                        .await
                        .map_err(backend)?;
                }
            }
            sqlx::query("UPDATE flows SET updated_at = ? WHERE id = ?")
                .bind(at.as_millis())
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
            tx.commit().await.map_err(backend)?;
        }
        self.flow_row(id).await?.ok_or(StoreError::NotFound("flow"))
    }

    async fn pause_flow(
        &self,
        id: FlowId,
        reason: &str,
        at: Timestamp,
    ) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE flows SET paused_reason = ?, paused_at = ?, updated_at = ?
             WHERE id = ? AND paused_reason IS NULL",
        )
        .bind(reason)
        .bind(at.as_millis())
        .bind(at.as_millis())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn delete_flow(&self, id: FlowId) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let archive: Option<String> =
            sqlx::query_scalar("SELECT archive_id FROM flows WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?;
        let Some(archive) = archive else {
            return Ok(false);
        };
        for statement in [
            "DELETE FROM flow_actions WHERE flow_id = ?",
            "DELETE FROM flow_runs WHERE flow_id = ?",
            "DELETE FROM flow_versions WHERE flow_id = ?",
            "DELETE FROM flows WHERE id = ?",
        ] {
            sqlx::query(statement)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        let order: Vec<String> =
            sqlx::query_scalar("SELECT id FROM flows WHERE archive_id = ? ORDER BY position, id")
                .bind(&archive)
                .fetch_all(&mut *tx)
                .await
                .map_err(backend)?;
        for (index, flow) in order.iter().enumerate() {
            sqlx::query("UPDATE flows SET position = ? WHERE id = ?")
                .bind(i64::try_from(index).unwrap_or(i64::MAX))
                .bind(flow)
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
        }
        tx.commit().await.map_err(backend)?;
        Ok(true)
    }

    async fn flow_versions(&self, id: FlowId) -> Result<Vec<FlowVersion>, StoreError> {
        sqlx::query("SELECT * FROM flow_versions WHERE flow_id = ? ORDER BY version DESC")
            .bind(id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(version)
            .collect()
    }

    async fn flow_version(
        &self,
        id: FlowId,
        number: u32,
    ) -> Result<Option<FlowVersion>, StoreError> {
        sqlx::query("SELECT * FROM flow_versions WHERE flow_id = ? AND version = ?")
            .bind(id.to_string())
            .bind(i64::from(number))
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(version)
            .transpose()
    }

    async fn claim_flow_run(&self, new: &NewFlowRun) -> Result<Option<FlowRun>, StoreError> {
        let id = FlowRunId::new();
        let inserted = {
            let _write = self.writer.lock().await;
            sqlx::query(
                "INSERT OR IGNORE INTO flow_runs (id, flow_id, archive_id, version, event_id, trigger,
                   test, message_id, sender, started_at, status)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'running')",
            )
            .bind(id.to_string())
            .bind(new.flow_id.to_string())
            .bind(new.archive_id.to_string())
            .bind(i64::from(new.version))
            .bind(&new.event_id)
            .bind(&new.trigger)
            .bind(i64::from(new.test))
            .bind(new.message_id.map(|m| m.to_string()))
            .bind(new.sender.as_deref())
            .bind(new.started_at.as_millis())
            .execute(&self.pool)
            .await
            .map_err(backend)?
            .rows_affected()
                > 0
        };
        if !inserted {
            return Ok(None);
        }
        self.get_flow_run(id).await
    }

    async fn finish_flow_run(&self, id: FlowRunId, end: &FlowRunEnd) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        let plan = end.plan.as_ref().map(json).transpose()?;
        let done = sqlx::query(
            "UPDATE flow_runs SET status = ?, plan = ?, results = ?, error = ?, acted = ?,
               finished_at = ?
             WHERE id = ?",
        )
        .bind(end.status.as_str())
        .bind(plan)
        .bind(json(&end.results)?)
        .bind(end.error.as_deref())
        .bind(i64::from(end.acted))
        .bind(end.at.as_millis())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        match done.rows_affected() {
            0 => Err(StoreError::NotFound("run")),
            _ => Ok(()),
        }
    }

    async fn set_flow_run_result(
        &self,
        id: FlowRunId,
        index: usize,
        expected: &str,
        result: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let stored: Option<String> =
            sqlx::query_scalar("SELECT results FROM flow_runs WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        let stored = stored.ok_or(StoreError::NotFound("run"))?;
        let mut results: serde_json::Value = serde_json::from_str(&stored)
            .map_err(|e| StoreError::Invalid(format!("run results: {e}")))?;
        let Some(slot) = results.as_array_mut().and_then(|r| r.get_mut(index)) else {
            return Ok(false);
        };
        if slot.get("result").and_then(serde_json::Value::as_str) != Some(expected) {
            return Ok(false);
        }
        *slot = result.clone();
        sqlx::query("UPDATE flow_runs SET results = ? WHERE id = ?")
            .bind(json(&results)?)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(true)
    }

    async fn get_flow_run(&self, id: FlowRunId) -> Result<Option<FlowRun>, StoreError> {
        sqlx::query("SELECT * FROM flow_runs WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?
            .as_ref()
            .map(run)
            .transpose()
    }

    async fn flow_run_for_event(
        &self,
        id: FlowId,
        event: &str,
    ) -> Result<Option<FlowRun>, StoreError> {
        sqlx::query(
            "SELECT * FROM flow_runs WHERE flow_id = ? AND event_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(id.to_string())
        .bind(event)
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?
        .as_ref()
        .map(run)
        .transpose()
    }

    async fn flow_runs(
        &self,
        id: FlowId,
        before: Option<FlowRunId>,
        limit: u32,
    ) -> Result<Vec<FlowRun>, StoreError> {
        sqlx::query(
            "SELECT * FROM flow_runs WHERE flow_id = ?1 AND (?2 IS NULL OR id < ?2)
             ORDER BY id DESC LIMIT ?3",
        )
        .bind(id.to_string())
        .bind(before.map(|b| b.to_string()))
        .bind(i64::from(limit.min(MAX_RUNS_PAGE)))
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?
        .iter()
        .map(run)
        .collect()
    }

    async fn interrupt_flow_runs(&self, at: Timestamp) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "UPDATE flow_runs SET status = 'interrupted', finished_at = ? WHERE status = 'running'",
        )
        .bind(at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(done.rows_affected())
    }

    async fn prune_flow_runs(
        &self,
        runs_before: Timestamp,
        actions_before: Timestamp,
    ) -> Result<u64, StoreError> {
        let _write = self.writer.lock().await;
        let runs =
            sqlx::query("DELETE FROM flow_runs WHERE started_at < ? AND status != 'running'")
                .bind(runs_before.as_millis())
                .execute(&self.pool)
                .await
                .map_err(backend)?
                .rows_affected();
        sqlx::query("DELETE FROM flow_actions WHERE at < ?")
            .bind(actions_before.as_millis())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(runs)
    }

    async fn count_flow_runs(
        &self,
        id: FlowId,
        since: Timestamp,
        acted: bool,
        sender: Option<&str>,
    ) -> Result<u64, StoreError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM flow_runs WHERE flow_id = ?1 AND started_at >= ?2
               AND (?3 = 0 OR acted = 1) AND (?4 IS NULL OR sender = lower(?4))",
        )
        .bind(id.to_string())
        .bind(since.as_millis())
        .bind(i64::from(acted))
        .bind(sender)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(count(n))
    }

    async fn count_messages_from(
        &self,
        archive: ArchiveId,
        address: &str,
        since: Timestamp,
        except: Option<MessageId>,
    ) -> Result<u64, StoreError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT a.message_id) FROM message_addresses a
             JOIN messages m ON m.id = a.message_id
             WHERE a.archive_id = ?1 AND a.field = 'from' AND a.email = lower(trim(?2))
               AND m.archived_at >= ?3 AND (?4 IS NULL OR m.id != ?4)",
        )
        .bind(archive.to_string())
        .bind(address)
        .bind(since.as_millis())
        .bind(except.map(|m| m.to_string()))
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(count(n))
    }

    async fn record_flow_action(&self, action: &FlowAction) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query(
            "INSERT INTO flow_actions (run_id, flow_id, kind, identity_id, recipient, at)
             VALUES (?, ?, ?, ?, lower(?), ?)",
        )
        .bind(action.run_id.to_string())
        .bind(action.flow_id.to_string())
        .bind(&action.kind)
        .bind(action.identity.map(|i| i.to_string()))
        .bind(action.recipient.as_deref())
        .bind(action.at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn count_flow_actions(&self, id: FlowId, since: Timestamp) -> Result<u64, StoreError> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM flow_actions WHERE flow_id = ? AND at >= ?")
                .bind(id.to_string())
                .bind(since.as_millis())
                .fetch_one(&self.pool)
                .await
                .map_err(backend)?;
        Ok(count(n))
    }

    async fn count_identity_flow_actions(
        &self,
        identity: IdentityId,
        since: Timestamp,
    ) -> Result<u64, StoreError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM flow_actions WHERE identity_id = ? AND at >= ?",
        )
        .bind(identity.to_string())
        .bind(since.as_millis())
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(count(n))
    }

    async fn last_flow_reply(
        &self,
        id: FlowId,
        recipient: &str,
    ) -> Result<Option<Timestamp>, StoreError> {
        let at: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(at) FROM flow_actions
             WHERE flow_id = ? AND kind = 'reply' AND recipient = lower(?)",
        )
        .bind(id.to_string())
        .bind(recipient)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;
        Ok(at.map(Timestamp::from_millis))
    }

    async fn put_rule_event(&self, event: &RuleEvent) -> Result<bool, StoreError> {
        let _write = self.writer.lock().await;
        let done = sqlx::query(
            "INSERT OR IGNORE INTO rule_events (archive_id, id, seq, trigger, message_id, item_id,
               live, payload, at)
             VALUES (?, ?, (SELECT IFNULL(MAX(seq), 0) + 1 FROM rule_events), ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.archive_id.to_string())
        .bind(&event.id)
        .bind(&event.trigger)
        .bind(event.message_id.map(|m| m.to_string()))
        .bind(event.item_id.map(|i| i.to_string()))
        .bind(i64::from(event.live))
        .bind(json(&event.payload)?)
        .bind(event.at.as_millis())
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(done.rows_affected() > 0)
    }

    async fn due_rule_events(&self, limit: u32) -> Result<Vec<RuleEvent>, StoreError> {
        sqlx::query("SELECT * FROM rule_events ORDER BY seq LIMIT ?")
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?
            .iter()
            .map(event)
            .collect()
    }

    async fn drop_rule_event(&self, archive: ArchiveId, id: &str) -> Result<(), StoreError> {
        let _write = self.writer.lock().await;
        sqlx::query("DELETE FROM rule_events WHERE archive_id = ? AND id = ?")
            .bind(archive.to_string())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}
