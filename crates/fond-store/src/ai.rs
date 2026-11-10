//! AI-пропозиції: журнал викликів AI-аналітика і їх шляху конвеєром.

use chrono::{DateTime, NaiveDate, Utc};
use fond_core::ai::AiProposal;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::trading::{enum_parse, enum_str};
use crate::{Result, Store, StoreError, fmt_date, fmt_ts, parse_date, parse_ts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProposalStatus {
    Recorded,
    RiskRejected,
    IntentCreated,
}

/// Запис журналу: пропозиція AI + витрати LLM за неї.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiProposalRecord {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub run_date: NaiveDate,
    pub proposal: AiProposal,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
    pub status: AiProposalStatus,
    pub reject_reason: Option<String>,
}

/// Новий запис; `id` призначається сховищем.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAiProposal {
    pub at: DateTime<Utc>,
    pub run_date: NaiveDate,
    pub proposal: AiProposal,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

const AI_QUERY: &str = "SELECT id, at, run_date, symbol, action, reference_price, stop_price, \
     take_profit, strength, explanation, key_reasons, model, input_tokens, output_tokens, \
     cost_usd, status, reject_reason FROM ai_proposals \
     WHERE at >= ? AND at < ? ORDER BY at, id";

impl Store {
    /// Додає запис пропозиції, повертає новий id.
    pub async fn add_ai_proposal(&self, p: &NewAiProposal) -> Result<i64> {
        let results = sqlx::query(
            "INSERT INTO ai_proposals (at, run_date, symbol, action, reference_price, \
             stop_price, take_profit, strength, explanation, key_reasons, model, \
             input_tokens, output_tokens, cost_usd, status, reject_reason) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        )
        .bind(fmt_ts(p.at))
        .bind(fmt_date(p.run_date))
        .bind(&p.proposal.symbol)
        .bind(enum_str(&p.proposal.action)?)
        .bind(p.proposal.reference_price)
        .bind(p.proposal.stop_price)
        .bind(p.proposal.take_profit)
        .bind(p.proposal.strength)
        .bind(&p.proposal.explanation)
        .bind(
            serde_json::to_string(&p.proposal.key_reasons)
                .map_err(|e| StoreError::Corrupt(e.to_string()))?,
        )
        .bind(&p.model)
        .bind(p.input_tokens)
        .bind(p.output_tokens)
        .bind(p.cost_usd)
        .bind(enum_str(&AiProposalStatus::Recorded)?)
        .execute(&self.pool)
        .await?;
        Ok(results.last_insert_rowid())
    }

    /// Записи за інтервал часу (включно з `from`, не включаючи `to`).
    pub async fn ai_proposals_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<AiProposalRecord>> {
        let rows = sqlx::query(AI_QUERY)
            .bind(fmt_ts(from))
            .bind(fmt_ts(to))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_ai_proposal).collect()
    }

    /// Оновлює статус конвеєра для запису; повертає false, якщо id не знайдено.
    pub async fn set_ai_proposal_status(
        &self,
        id: i64,
        status: AiProposalStatus,
        reject_reason: Option<&str>,
    ) -> Result<bool> {
        let results =
            sqlx::query("UPDATE ai_proposals SET status = ?, reject_reason = ? WHERE id = ?")
                .bind(enum_str(&status)?)
                .bind(reject_reason)
                .bind(id)
                .execute(&self.pool)
                .await?;
        Ok(results.rows_affected() > 0)
    }
}

fn row_to_ai_proposal(r: &SqliteRow) -> Result<AiProposalRecord> {
    let at_s: String = r.try_get("at")?;
    let run_date_s: String = r.try_get("run_date")?;
    let symbol_s: String = r.try_get("symbol")?;
    let action_s: String = r.try_get("action")?;
    let key_reasons_s: String = r.try_get("key_reasons")?;
    let status_s: String = r.try_get("status")?;
    let key_reasons: Vec<String> = serde_json::from_str(&key_reasons_s)
        .map_err(|e| StoreError::Corrupt(format!("key_reasons: {e}")))?;
    Ok(AiProposalRecord {
        id: r.try_get("id")?,
        at: parse_ts(&at_s)?,
        run_date: parse_date(&run_date_s)?,
        proposal: AiProposal {
            symbol: symbol_s,
            action: enum_parse(&action_s, "дії AI")?,
            reference_price: r.try_get("reference_price")?,
            stop_price: r.try_get("stop_price")?,
            take_profit: r.try_get("take_profit")?,
            strength: r.try_get("strength")?,
            explanation: r.try_get("explanation")?,
            key_reasons,
        },
        model: r.try_get("model")?,
        input_tokens: r.try_get("input_tokens")?,
        output_tokens: r.try_get("output_tokens")?,
        cost_usd: r.try_get("cost_usd")?,
        status: enum_parse(&status_s, "статусу AI-пропозиції")?,
        reject_reason: r.try_get("reject_reason")?,
    })
}
