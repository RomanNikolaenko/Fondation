use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row;

use crate::{Result, Store, StoreError, fmt_date, fmt_ts, parse_ts};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepName {
    Analysis,
    ApprovalDeadline,
    PreOpenSubmit,
}

impl StepName {
    pub fn as_str(self) -> &'static str {
        match self {
            StepName::Analysis => "analysis",
            StepName::ApprovalDeadline => "approval_deadline",
            StepName::PreOpenSubmit => "pre_open_submit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Running,
    Done,
    Failed,
    Missed,
}

impl StepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Running => "running",
            StepStatus::Done => "done",
            StepStatus::Failed => "failed",
            StepStatus::Missed => "missed",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "running" => StepStatus::Running,
            "done" => StepStatus::Done,
            "failed" => StepStatus::Failed,
            "missed" => StepStatus::Missed,
            other => {
                return Err(StoreError::Corrupt(format!(
                    "невідомий статус кроку '{other}'"
                )));
            }
        })
    }
}

impl Store {
    /// Статус кроку та момент його старту.
    pub async fn step_status(
        &self,
        date: NaiveDate,
        step: StepName,
    ) -> Result<Option<(StepStatus, DateTime<Utc>)>> {
        let row = sqlx::query(
            "SELECT status, started_at FROM cycle_runs WHERE trading_date = ? AND step = ?",
        )
        .bind(fmt_date(date))
        .bind(step.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let status: String = r.try_get("status")?;
            let started: String = r.try_get("started_at")?;
            Ok((StepStatus::parse(&status)?, parse_ts(&started)?))
        })
        .transpose()
    }

    pub async fn mark_step(
        &self,
        date: NaiveDate,
        step: StepName,
        status: StepStatus,
        at: DateTime<Utc>,
        error: Option<&str>,
    ) -> Result<()> {
        let at_s = fmt_ts(at);
        let finished = (status != StepStatus::Running).then(|| at_s.clone());
        if status == StepStatus::Running {
            sqlx::query(
                "INSERT INTO cycle_runs (trading_date, step, status, started_at, finished_at, error) \
                 VALUES (?, ?, ?, ?, NULL, ?) \
                 ON CONFLICT(trading_date, step) DO UPDATE SET status = excluded.status, \
                 started_at = excluded.started_at, finished_at = NULL, error = excluded.error",
            )
            .bind(fmt_date(date))
            .bind(step.as_str())
            .bind(status.as_str())
            .bind(&at_s)
            .bind(error)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO cycle_runs (trading_date, step, status, started_at, finished_at, error) \
                 VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(trading_date, step) DO UPDATE SET status = excluded.status, \
                 finished_at = excluded.finished_at, error = excluded.error",
            )
            .bind(fmt_date(date))
            .bind(step.as_str())
            .bind(status.as_str())
            .bind(&at_s)
            .bind(finished)
            .bind(error)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }
}
