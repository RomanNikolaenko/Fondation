//! Запуски бектестів.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::trading::{enum_parse, enum_str};
use crate::{Result, Store, StoreError, fmt_ts, parse_ts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacktestStatus {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacktestRow {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub status: BacktestStatus,
    pub request: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

fn parse_json(s: &str, what: &str) -> Result<serde_json::Value> {
    serde_json::from_str(s).map_err(|e| StoreError::Corrupt(format!("{what}: {e}")))
}

fn row_to_backtest(r: &SqliteRow) -> Result<BacktestRow> {
    let status: String = r.try_get("status")?;
    let request: String = r.try_get("request")?;
    let result: Option<String> = r.try_get("result")?;
    Ok(BacktestRow {
        id: r.try_get("id")?,
        created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
        status: enum_parse(&status, "статусу бектесту")?,
        request: parse_json(&request, "request бектесту")?,
        result: result
            .as_deref()
            .map(|s| parse_json(s, "result бектесту"))
            .transpose()?,
        error: r.try_get("error")?,
    })
}

impl Store {
    pub async fn create_backtest(
        &self,
        id: &str,
        request: &serde_json::Value,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO backtest_runs (id, created_at, status, request) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(fmt_ts(at))
        .bind(enum_str(&BacktestStatus::Queued)?)
        .bind(request.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_backtest(
        &self,
        id: &str,
        status: BacktestStatus,
        result: Option<String>,
        error: Option<&str>,
    ) -> Result<()> {
        let res = sqlx::query(
            "UPDATE backtest_runs SET status = ?, result = COALESCE(?, result), \
             error = COALESCE(?, error) WHERE id = ?",
        )
        .bind(enum_str(&status)?)
        .bind(result)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("бектест '{id}'")));
        }
        Ok(())
    }

    pub async fn set_backtest_status(&self, id: &str, st: BacktestStatus) -> Result<()> {
        self.update_backtest(id, st, None, None).await
    }

    pub async fn finish_backtest(&self, id: &str, result: &serde_json::Value) -> Result<()> {
        self.update_backtest(id, BacktestStatus::Done, Some(result.to_string()), None)
            .await
    }

    pub async fn fail_backtest(&self, id: &str, err: &str) -> Result<()> {
        self.update_backtest(id, BacktestStatus::Failed, None, Some(err))
            .await
    }

    pub async fn backtest(&self, id: &str) -> Result<Option<BacktestRow>> {
        let row = sqlx::query(
            "SELECT id, created_at, status, request, result, error FROM backtest_runs \
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_backtest).transpose()
    }

    /// Найновіші першими, без `result`.
    pub async fn backtests(&self) -> Result<Vec<BacktestRow>> {
        let rows = sqlx::query(
            "SELECT id, created_at, status, request, NULL AS result, error FROM backtest_runs \
             ORDER BY created_at DESC, id DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_backtest).collect()
    }
}
