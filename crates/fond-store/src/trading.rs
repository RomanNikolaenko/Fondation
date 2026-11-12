//! Сигнали, фільтри, витрати LLM, наміри, ордери, позиції, еквіті.

use chrono::{DateTime, NaiveDate, Utc};
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::BrokerOrder;
use fond_core::portfolio::TrackedPosition;
use fond_core::signal::EntrySignal;
use fond_core::traits::NewsVerdict;
use fond_core::types::{StrategyId, Symbol};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, StoreError, fmt_date, fmt_ts, parse_date, parse_ts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalStatus {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalRecord {
    pub id: String,
    pub signal: EntrySignal,
    pub status: SignalStatus,
    pub reject_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EquityKind {
    Close,
    Intraday,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquitySnapshot {
    pub at: DateTime<Utc>,
    pub kind: EquityKind,
    pub session_date: NaiveDate,
    pub equity: f64,
    pub cash: f64,
}

/// snake_case-рядок для простого enum (через serde).
pub(crate) fn enum_str<T: Serialize>(v: &T) -> Result<String> {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(StoreError::Corrupt(format!(
            "очікувався рядок для enum, отримано {other}"
        ))),
        Err(e) => Err(StoreError::Corrupt(e.to_string())),
    }
}

pub(crate) fn enum_parse<T: DeserializeOwned>(s: &str, what: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|_| StoreError::Corrupt(format!("невідоме значення {what} '{s}'")))
}

fn to_json<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| StoreError::Corrupt(e.to_string()))
}

fn from_json<T: DeserializeOwned>(s: &str, what: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| StoreError::Corrupt(format!("{what}: {e}")))
}

fn kind_tag(k: &IntentKind) -> &'static str {
    match k {
        IntentKind::Entry { .. } => "entry",
        IntentKind::Exit { .. } => "exit",
        IntentKind::MoveStop { .. } => "move_stop",
        IntentKind::Rebalance { .. } => "rebalance",
    }
}

fn row_to_signal(r: &SqliteRow) -> Result<SignalRecord> {
    let status: String = r.try_get("status")?;
    Ok(SignalRecord {
        id: r.try_get("id")?,
        signal: EntrySignal {
            strategy: StrategyId::new(&r.try_get::<String, _>("strategy")?),
            symbol: Symbol::new(&r.try_get::<String, _>("symbol")?),
            signal_date: parse_date(&r.try_get::<String, _>("signal_date")?)?,
            reference_price: r.try_get("reference_price")?,
            stop_price: r.try_get("stop_price")?,
            take_profit: r.try_get("take_profit")?,
            strength: r.try_get("strength")?,
            explanation: r.try_get("explanation")?,
        },
        status: enum_parse(&status, "статусу сигналу")?,
        reject_reason: r.try_get("reject_reason")?,
    })
}

fn row_to_intent(r: &SqliteRow) -> Result<Intent> {
    let status: String = r.try_get("status")?;
    let payload: String = r.try_get("payload")?;
    Ok(Intent {
        id: r.try_get("id")?,
        session_date: parse_date(&r.try_get::<String, _>("session_date")?)?,
        signal_date: parse_date(&r.try_get::<String, _>("signal_date")?)?,
        symbol: Symbol::new(&r.try_get::<String, _>("symbol")?),
        strategy: StrategyId::new(&r.try_get::<String, _>("strategy")?),
        kind: from_json(&payload, "payload наміру")?,
        status: enum_parse(&status, "статусу наміру")?,
        explanation: r.try_get("explanation")?,
        created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
        updated_at: parse_ts(&r.try_get::<String, _>("updated_at")?)?,
        broker_order_id: r.try_get("broker_order_id")?,
        error: r.try_get("error")?,
    })
}

const INTENT_COLS: &str = "id, session_date, signal_date, symbol, strategy, payload, status, \
                           explanation, created_at, updated_at, broker_order_id, error";

impl Store {
    // ---- сигнали ----

    /// Дубль (той самий id) ігнорується.
    pub async fn insert_signal(&self, r: &SignalRecord) -> Result<()> {
        let s = &r.signal;
        sqlx::query(
            "INSERT OR IGNORE INTO signals (id, signal_date, strategy, symbol, reference_price, \
             stop_price, take_profit, strength, explanation, status, reject_reason) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&r.id)
        .bind(fmt_date(s.signal_date))
        .bind(s.strategy.as_str())
        .bind(s.symbol.as_str())
        .bind(s.reference_price)
        .bind(s.stop_price)
        .bind(s.take_profit)
        .bind(s.strength)
        .bind(&s.explanation)
        .bind(enum_str(&r.status)?)
        .bind(&r.reject_reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn signals_on(&self, signal_date: NaiveDate) -> Result<Vec<SignalRecord>> {
        let rows = sqlx::query(
            "SELECT id, signal_date, strategy, symbol, reference_price, stop_price, take_profit, \
             strength, explanation, status, reject_reason FROM signals \
             WHERE signal_date = ? ORDER BY symbol, id",
        )
        .bind(fmt_date(signal_date))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_signal).collect()
    }

    // ---- кеш фільтрів ----

    pub async fn put_earnings_check(
        &self,
        s: &Symbol,
        signal_date: NaiveDate,
        next: Option<NaiveDate>,
        blocked: bool,
        source_ok: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO earnings_checks (symbol, signal_date, next_earnings, blocked, source_ok) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT(symbol, signal_date) DO UPDATE SET \
             next_earnings = excluded.next_earnings, blocked = excluded.blocked, \
             source_ok = excluded.source_ok",
        )
        .bind(s.as_str())
        .bind(fmt_date(signal_date))
        .bind(next.map(fmt_date))
        .bind(blocked)
        .bind(source_ok)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn news_verdict(
        &self,
        s: &Symbol,
        signal_date: NaiveDate,
    ) -> Result<Option<NewsVerdict>> {
        let row = sqlx::query(
            "SELECT verdict, reasons, key_events FROM news_verdicts \
             WHERE symbol = ? AND signal_date = ?",
        )
        .bind(s.as_str())
        .bind(fmt_date(signal_date))
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let verdict: String = r.try_get("verdict")?;
        let reasons: String = r.try_get("reasons")?;
        let key_events: String = r.try_get("key_events")?;
        Ok(Some(NewsVerdict {
            verdict: enum_parse(&verdict, "вердикту новин")?,
            reasons: from_json(&reasons, "reasons")?,
            key_events: from_json(&key_events, "key_events")?,
        }))
    }

    pub async fn put_news_verdict(
        &self,
        s: &Symbol,
        signal_date: NaiveDate,
        v: &NewsVerdict,
        model: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO news_verdicts (symbol, signal_date, verdict, reasons, key_events, \
             model, created_at) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(symbol, signal_date) DO UPDATE SET verdict = excluded.verdict, \
             reasons = excluded.reasons, key_events = excluded.key_events, \
             model = excluded.model, created_at = excluded.created_at",
        )
        .bind(s.as_str())
        .bind(fmt_date(signal_date))
        .bind(enum_str(&v.verdict)?)
        .bind(to_json(&v.reasons)?)
        .bind(to_json(&v.key_events)?)
        .bind(model)
        .bind(fmt_ts(at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- витрати LLM ----

    pub async fn add_llm_usage(
        &self,
        at: DateTime<Utc>,
        model: &str,
        input: u64,
        output: u64,
        cost_usd: f64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO llm_usage (at, model, input_tokens, output_tokens, cost_usd) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(fmt_ts(at))
        .bind(model)
        .bind(i64::try_from(input).unwrap_or(i64::MAX))
        .bind(i64::try_from(output).unwrap_or(i64::MAX))
        .bind(cost_usd)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Сума витрат у півінтервалі `[from, to)`.
    pub async fn llm_spend_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<f64> {
        let sum: Option<f64> =
            sqlx::query_scalar("SELECT SUM(cost_usd) FROM llm_usage WHERE at >= ? AND at < ?")
                .bind(fmt_ts(from))
                .bind(fmt_ts(to))
                .fetch_one(&self.pool)
                .await?;
        Ok(sum.unwrap_or(0.0))
    }

    // ---- наміри ----

    /// `false`, якщо намір з таким id уже існує (запис не змінюється).
    pub async fn insert_intent(&self, i: &Intent) -> Result<bool> {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO intents (id, session_date, signal_date, symbol, strategy, \
             kind, payload, status, explanation, created_at, updated_at, broker_order_id, error) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&i.id)
        .bind(fmt_date(i.session_date))
        .bind(fmt_date(i.signal_date))
        .bind(i.symbol.as_str())
        .bind(i.strategy.as_str())
        .bind(kind_tag(&i.kind))
        .bind(to_json(&i.kind)?)
        .bind(enum_str(&i.status)?)
        .bind(&i.explanation)
        .bind(fmt_ts(i.created_at))
        .bind(fmt_ts(i.updated_at))
        .bind(&i.broker_order_id)
        .bind(&i.error)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn intent(&self, id: &str) -> Result<Option<Intent>> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {INTENT_COLS} FROM intents WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_intent).transpose()
    }

    pub async fn intents_with_status(&self, statuses: &[IntentStatus]) -> Result<Vec<Intent>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; statuses.len()].join(", ");
        let sql = format!(
            "SELECT {INTENT_COLS} FROM intents WHERE status IN ({marks}) \
             ORDER BY session_date, created_at, id"
        );
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for s in statuses {
            q = q.bind(enum_str(s)?);
        }
        let rows = q.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_intent).collect()
    }

    pub async fn intents_for_session(&self, session: NaiveDate) -> Result<Vec<Intent>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {INTENT_COLS} FROM intents WHERE session_date = ? \
             ORDER BY session_date, created_at, id"
        )))
        .bind(fmt_date(session))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_intent).collect()
    }

    /// Змінює статус за автоматом станів, в одній транзакції.
    /// `broker_order_id` / `error` оновлюються лише якщо `Some`.
    pub async fn update_intent(
        &self,
        id: &str,
        next: IntentStatus,
        at: DateTime<Utc>,
        broker_order_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<Intent> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let cur: Option<String> = sqlx::query_scalar("SELECT status FROM intents WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(cur) = cur else {
            return Err(StoreError::NotFound(format!("намір '{id}'")));
        };
        let from: IntentStatus = enum_parse(&cur, "статусу наміру")?;
        if !from.can_transition_to(next) {
            return Err(StoreError::InvalidTransition { from, to: next });
        }
        sqlx::query(
            "UPDATE intents SET status = ?, updated_at = ?, \
             broker_order_id = COALESCE(?, broker_order_id), error = COALESCE(?, error) \
             WHERE id = ?",
        )
        .bind(enum_str(&next)?)
        .bind(fmt_ts(at))
        .bind(broker_order_id)
        .bind(error)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {INTENT_COLS} FROM intents WHERE id = ?"
        )))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let intent = row_to_intent(&row)?;
        tx.commit().await?;
        Ok(intent)
    }

    // ---- ордери ----

    pub async fn upsert_order(&self, o: &BrokerOrder, at: DateTime<Utc>) -> Result<()> {
        let status = match serde_json::to_value(&o.status) {
            Ok(serde_json::Value::String(s)) => s,
            Ok(serde_json::Value::Object(m)) => m
                .values()
                .next()
                .and_then(|v| v.as_str())
                .unwrap_or("other")
                .to_string(),
            _ => "other".to_string(),
        };
        sqlx::query(
            "INSERT INTO orders (id, client_order_id, symbol, payload, status, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET \
             client_order_id = excluded.client_order_id, symbol = excluded.symbol, \
             payload = excluded.payload, status = excluded.status, \
             updated_at = excluded.updated_at",
        )
        .bind(&o.id)
        .bind(&o.client_order_id)
        .bind(o.symbol.as_str())
        .bind(to_json(o)?)
        .bind(status)
        .bind(fmt_ts(at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- позиції ----

    pub async fn upsert_position(&self, p: &TrackedPosition, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO positions (symbol, payload, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT(symbol) DO UPDATE SET payload = excluded.payload, \
             updated_at = excluded.updated_at",
        )
        .bind(p.position.symbol.as_str())
        .bind(to_json(p)?)
        .bind(fmt_ts(at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_position(&self, s: &Symbol) -> Result<()> {
        sqlx::query("DELETE FROM positions WHERE symbol = ?")
            .bind(s.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn positions(&self) -> Result<Vec<TrackedPosition>> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT payload FROM positions ORDER BY symbol")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|p| from_json(p, "payload позиції"))
            .collect()
    }

    // ---- еквіті ----

    pub async fn insert_equity(&self, e: &EquitySnapshot) -> Result<()> {
        sqlx::query(
            "INSERT INTO equity_snapshots (at, kind, session_date, equity, cash) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(fmt_ts(e.at))
        .bind(enum_str(&e.kind)?)
        .bind(fmt_date(e.session_date))
        .bind(e.equity)
        .bind(e.cash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Остання Close-точка цієї сесії.
    pub async fn close_equity(&self, session: NaiveDate) -> Result<Option<f64>> {
        Ok(sqlx::query_scalar(
            "SELECT equity FROM equity_snapshots WHERE kind = 'close' AND session_date = ? \
             ORDER BY at DESC, id DESC LIMIT 1",
        )
        .bind(fmt_date(session))
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Остання Close-точка серед сесій, раніших за `session`.
    pub async fn last_close_equity_before(
        &self,
        session: NaiveDate,
    ) -> Result<Option<(NaiveDate, f64)>> {
        let row = sqlx::query(
            "SELECT session_date, equity FROM equity_snapshots \
             WHERE kind = 'close' AND session_date < ? \
             ORDER BY session_date DESC, at DESC, id DESC LIMIT 1",
        )
        .bind(fmt_date(session))
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        Ok(Some((
            parse_date(&r.try_get::<String, _>("session_date")?)?,
            r.try_get("equity")?,
        )))
    }
}
