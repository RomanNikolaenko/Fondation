use chrono::{DateTime, Utc};
use fond_core::types::EngineMode;
use sqlx::Row;

use crate::{Result, Store, StoreError, fmt_ts, parse_ts};

impl Store {
    /// Режим рушія; без запису — `Running`.
    pub async fn engine_mode(&self) -> Result<EngineMode> {
        let row = sqlx::query("SELECT mode, reason FROM engine_state WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else {
            return Ok(EngineMode::Running);
        };
        let mode: String = r.try_get("mode")?;
        let reason: Option<String> = r.try_get("reason")?;
        let reason = reason.unwrap_or_default();
        match mode.as_str() {
            "running" => Ok(EngineMode::Running),
            "paused" => Ok(EngineMode::Paused { reason }),
            "killed" => Ok(EngineMode::Killed { reason }),
            other => Err(StoreError::Corrupt(format!(
                "невідомий режим рушія '{other}'"
            ))),
        }
    }

    pub async fn set_engine_mode(&self, m: &EngineMode, at: DateTime<Utc>) -> Result<()> {
        let (mode, reason) = match m {
            EngineMode::Running => ("running", None),
            EngineMode::Paused { reason } => ("paused", Some(reason.as_str())),
            EngineMode::Killed { reason } => ("killed", Some(reason.as_str())),
        };
        sqlx::query(
            "INSERT INTO engine_state (id, mode, reason, updated_at) VALUES (1, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET mode = excluded.mode, reason = excluded.reason, \
             updated_at = excluded.updated_at",
        )
        .bind(mode)
        .bind(reason)
        .bind(fmt_ts(at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ставить паузу лише з `Running` (зокрема коли запису ще немає) одним умовним запитом:
    /// паралельна аварійна зупинка чи інша пауза ніколи не перезаписується.
    /// `true`, якщо режим змінено.
    pub async fn pause_if_running(&self, reason: &str, at: DateTime<Utc>) -> Result<bool> {
        let res = sqlx::query(
            "INSERT INTO engine_state (id, mode, reason, updated_at) VALUES (1, 'paused', ?, ?) \
             ON CONFLICT(id) DO UPDATE SET mode = excluded.mode, reason = excluded.reason, \
             updated_at = excluded.updated_at WHERE engine_state.mode = 'running'",
        )
        .bind(reason)
        .bind(fmt_ts(at))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn last_reconcile_at(&self) -> Result<Option<DateTime<Utc>>> {
        let v: Option<Option<String>> =
            sqlx::query_scalar("SELECT last_reconcile_at FROM engine_state WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?;
        v.flatten().as_deref().map(parse_ts).transpose()
    }

    pub async fn set_last_reconcile_at(&self, at: DateTime<Utc>) -> Result<()> {
        let s = fmt_ts(at);
        sqlx::query(
            "INSERT INTO engine_state (id, mode, reason, updated_at, last_reconcile_at) \
             VALUES (1, 'running', NULL, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET last_reconcile_at = excluded.last_reconcile_at",
        )
        .bind(&s)
        .bind(&s)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
