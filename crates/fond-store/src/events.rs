use chrono::{DateTime, Utc};
use fond_core::types::Symbol;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::{Result, Store, StoreError, fmt_ts, parse_ts};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Cycle,
    Scan,
    Signal,
    Filter,
    Risk,
    Intent,
    Order,
    Fill,
    Reconcile,
    Pause,
    Kill,
    Config,
    Error,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Cycle => "cycle",
            EventKind::Scan => "scan",
            EventKind::Signal => "signal",
            EventKind::Filter => "filter",
            EventKind::Risk => "risk",
            EventKind::Intent => "intent",
            EventKind::Order => "order",
            EventKind::Fill => "fill",
            EventKind::Reconcile => "reconcile",
            EventKind::Pause => "pause",
            EventKind::Kill => "kill",
            EventKind::Config => "config",
            EventKind::Error => "error",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "cycle" => EventKind::Cycle,
            "scan" => EventKind::Scan,
            "signal" => EventKind::Signal,
            "filter" => EventKind::Filter,
            "risk" => EventKind::Risk,
            "intent" => EventKind::Intent,
            "order" => EventKind::Order,
            "fill" => EventKind::Fill,
            "reconcile" => EventKind::Reconcile,
            "pause" => EventKind::Pause,
            "kill" => EventKind::Kill,
            "config" => EventKind::Config,
            "error" => EventKind::Error,
            other => {
                return Err(StoreError::Corrupt(format!(
                    "невідомий тип події '{other}'"
                )));
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: EventKind,
    pub symbol: Option<Symbol>,
    pub intent_id: Option<String>,
    pub message: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
    pub symbol: Option<Symbol>,
    pub intent_id: Option<String>,
    pub message: String,
    pub data: serde_json::Value,
}

/// Фільтр журналу; `limit == 0` означає типові 200.
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub symbol: Option<Symbol>,
    pub kinds: Vec<EventKind>,
    pub limit: u32,
}

const DEFAULT_LIMIT: u32 = 200;

fn row_to_event(r: &SqliteRow) -> Result<StoredEvent> {
    let at: String = r.try_get("at")?;
    let kind: String = r.try_get("kind")?;
    let symbol: Option<String> = r.try_get("symbol")?;
    let data: String = r.try_get("data")?;
    Ok(StoredEvent {
        id: r.try_get("id")?,
        at: parse_ts(&at)?,
        kind: EventKind::parse(&kind)?,
        symbol: symbol.as_deref().map(Symbol::new),
        intent_id: r.try_get("intent_id")?,
        message: r.try_get("message")?,
        data: serde_json::from_str(&data)
            .map_err(|e| StoreError::Corrupt(format!("data події: {e}")))?,
    })
}

impl Store {
    pub async fn append_event(&self, at: DateTime<Utc>, e: &NewEvent) -> Result<StoredEvent> {
        let data = serde_json::to_string(&e.data)
            .map_err(|err| StoreError::Corrupt(format!("data події: {err}")))?;
        let id = sqlx::query(
            "INSERT INTO events (at, kind, symbol, intent_id, message, data) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(fmt_ts(at))
        .bind(e.kind.as_str())
        .bind(e.symbol.as_ref().map(Symbol::as_str))
        .bind(e.intent_id.as_deref())
        .bind(&e.message)
        .bind(data)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();
        Ok(StoredEvent {
            id,
            at: parse_ts(&fmt_ts(at))?,
            kind: e.kind,
            symbol: e.symbol.clone(),
            intent_id: e.intent_id.clone(),
            message: e.message.clone(),
            data: e.data.clone(),
        })
    }

    /// Новіші першими.
    pub async fn events(&self, q: &EventQuery) -> Result<Vec<StoredEvent>> {
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT id, at, kind, symbol, intent_id, message, data FROM events WHERE 1 = 1",
        );
        if let Some(from) = q.from {
            qb.push(" AND at >= ").push_bind(fmt_ts(from));
        }
        if let Some(to) = q.to {
            qb.push(" AND at <= ").push_bind(fmt_ts(to));
        }
        if let Some(s) = &q.symbol {
            qb.push(" AND symbol = ").push_bind(s.as_str().to_string());
        }
        if !q.kinds.is_empty() {
            qb.push(" AND kind IN (");
            let mut sep = qb.separated(", ");
            for k in &q.kinds {
                sep.push_bind(k.as_str());
            }
            sep.push_unseparated(")");
        }
        let limit = if q.limit == 0 { DEFAULT_LIMIT } else { q.limit };
        qb.push(" ORDER BY id DESC LIMIT ")
            .push_bind(i64::from(limit));
        let rows = qb.build().fetch_all(&self.pool).await?;
        rows.iter().map(row_to_event).collect()
    }
}
